//! The append-only event log. One per server, one server-wide `seq`.
//!
//! Holds only its own lock (`Shared.log`), and holds it across `fsync`. That
//! is the reason it is a separate lock: appends serialize against each other
//! without blocking any other route. It never takes `core` or `sockets`.
//!
//! Recovery has two rules, and the second is the reason [`EventLog::append_all`]
//! exists:
//!
//! 1. An **unterminated** final line is a torn write and is truncated.
//!    Anything else that does not parse — a bad line in the middle, a sequence
//!    gap, a foreign format — is a hard error. Dropping committed history
//!    quietly is worse than refusing to start.
//! 2. A final **incomplete batch** is an interrupted commit and is dropped
//!    whole. One `write_all` is not a transaction: the kernel can take a
//!    prefix of the buffer and the process can die, leaving two complete,
//!    well-terminated records of a three-record commit. Rule 1 would accept
//!    those as history. The batch mark is what tells them apart.

use crate::server::event::{Actor, BatchMark, Event, EVENT_FORMAT};
use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One event waiting to be written. A `Vec` of these is what a multi-event
/// commit hands to [`EventLog::append_all`]; the log assigns the sequence
/// number, the timestamp, and the batch mark.
#[derive(Debug, Clone)]
pub struct Pending {
    pub artifact: String,
    pub revision: u32,
    pub actor: Actor,
    pub kind: String,
    pub data: serde_json::Value,
}

impl Pending {
    pub fn new(
        artifact: &str,
        revision: u32,
        actor: Actor,
        kind: &str,
        data: serde_json::Value,
    ) -> Pending {
        Pending {
            artifact: artifact.to_string(),
            revision,
            actor,
            kind: kind.to_string(),
            data,
        }
    }
}

/// `Debug` is derived because the tests use `Result::expect_err`, which
/// requires `T: Debug` on the success type.
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
    file: File,
    /// The whole log, in order. `since` slices this; nothing re-parses disk.
    events: Vec<Event>,
    next_seq: u64,
    /// Set when a write may or may not have reached the disk. Every later
    /// append refuses, because reusing a sequence number that might already be
    /// on disk is the one unrecoverable mistake this file can make.
    poisoned: bool,
}

/// Whether this state directory has a log with anything in it: something
/// was pushed here once, so there is a review to open or resume.
pub fn exists(dir: &Path) -> bool {
    std::fs::metadata(dir.join("events.ndjson")).is_ok_and(|m| m.len() > 0)
}

impl EventLog {
    pub fn open(dir: &Path) -> Result<EventLog> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("events.ndjson");
        let mut events = Vec::new();

        if path.exists() {
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let mut offset = 0usize;
            let mut line_no = 0usize;
            let mut good_bytes = 0usize;
            // Where each accepted record starts, so an interrupted commit can
            // be cut back to the byte before its first member.
            let mut starts: Vec<usize> = Vec::new();

            while offset < bytes.len() {
                starts.push(offset);
                let rest = &bytes[offset..];
                let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
                    // No terminator: a torn tail. Truncate it, whatever it holds.
                    break;
                };
                line_no += 1;
                let line = &rest[..nl];
                let text = std::str::from_utf8(line)
                    .with_context(|| format!("{}: line {line_no} is not utf-8", path.display()))?;
                let event: Event = serde_json::from_str(text).with_context(|| {
                    format!("{}: line {line_no} is not an event", path.display())
                })?;
                if event.format != EVENT_FORMAT {
                    bail!(
                        "{}: line {line_no} declares {}, not {EVENT_FORMAT}",
                        path.display(),
                        event.format
                    );
                }
                let expected = events.len() as u64 + 1;
                if event.seq != expected {
                    bail!(
                        "{}: line {line_no} has seq {}, expected seq {expected}",
                        path.display(),
                        event.seq
                    );
                }
                events.push(event);
                offset += nl + 1;
                good_bytes = offset;
            }

            // An interrupted commit: the last record on disk says it is one of
            // several, and the rest never arrived.
            if let Some(cut) = incomplete_batch_start(&events, &starts)? {
                events.truncate(cut);
                good_bytes = starts[cut];
            }

            if good_bytes < bytes.len() {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(good_bytes as u64)?;
                f.sync_all()?;
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let next_seq = events.len() as u64 + 1;
        Ok(EventLog {
            path,
            file,
            events,
            next_seq,
            poisoned: false,
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Every event with `seq` strictly greater than `cursor`. In memory, so a
    /// long poll can call it as often as it likes.
    pub fn since(&self, cursor: u64) -> &[Event] {
        let idx = self.events.partition_point(|e| e.seq <= cursor);
        &self.events[idx..]
    }

    pub fn append(
        &mut self,
        artifact: &str,
        revision: u32,
        actor: Actor,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<Event> {
        let mut written =
            self.append_all(vec![Pending::new(artifact, revision, actor, kind, data)])?;
        Ok(written.remove(0))
    }

    /// Write several events as one commit.
    ///
    /// Every record carries a batch mark naming the commit and its own place
    /// in it, so a crash that lands only a prefix leaves a log whose last
    /// record announces that more was coming. `open` drops such a group whole.
    /// Without the mark, a prefix of complete, newline-terminated records is
    /// indistinguishable from history — which is how "a revision and its
    /// resolutions" ends up half applied.
    ///
    /// The single `write_all` and `fsync` still matter: they make the
    /// interrupted case rare. The mark is what makes it *safe*.
    pub fn append_all(&mut self, entries: Vec<Pending>) -> Result<Vec<Event>> {
        if self.poisoned {
            bail!("the event log is in an unknown state after a failed write; restart the server");
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let count = entries.len() as u32;
        let id = batch_id();
        let ts = now_rfc3339();
        let mut events = Vec::with_capacity(entries.len());
        let mut buffer = String::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let event = Event {
                format: EVENT_FORMAT.to_string(),
                seq: self.next_seq + index as u64,
                ts: ts.clone(),
                artifact: entry.artifact,
                revision: entry.revision,
                actor: entry.actor,
                r#type: entry.kind,
                data: entry.data,
                // A single event needs no framing: the torn-tail rule already
                // makes one line all-or-nothing.
                batch: (count > 1).then(|| BatchMark {
                    id: id.clone(),
                    index: index as u32,
                    count,
                }),
            };
            buffer.push_str(&serde_json::to_string(&event)?);
            buffer.push('\n');
            events.push(event);
        }
        // From here the write may be partially visible on disk, so any failure
        // poisons rather than being retried at the same sequence numbers.
        if let Err(e) = self
            .file
            .write_all(buffer.as_bytes())
            .and_then(|()| self.file.sync_all())
        {
            self.poisoned = true;
            return Err(e).with_context(|| format!("appending to {}", self.path.display()));
        }
        self.next_seq += events.len() as u64;
        self.events.extend(events.iter().cloned());
        Ok(events)
    }
}

/// Where an interrupted commit begins, as an index into `events`.
///
/// Only the tail can be interrupted: the log has one writer, appends in order,
/// and every start runs this before writing anything. A group in the middle of
/// the file was therefore completed before the next record was written.
fn incomplete_batch_start(events: &[Event], starts: &[usize]) -> Result<Option<usize>> {
    let Some(last) = events.last() else {
        return Ok(None);
    };
    let Some(mark) = last.batch.as_ref() else {
        return Ok(None);
    };
    // The group's first member. A mark that points before the start of the
    // log is corruption, and corruption is a hard error here — never a panic,
    // and never a silent guess. This is checked whether or not the mark says
    // the group is complete: a final record claiming to be the last of three
    // with no first two behind it is just as wrong as one claiming to be the
    // first of three with nothing after it.
    let first = (events.len() - 1)
        .checked_sub(mark.index as usize)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the log's final record says it is member {} of commit {}, but the log is not that long",
                mark.index,
                mark.id
            )
        })?;
    for (offset, event) in events[first..].iter().enumerate() {
        let belongs = event
            .batch
            .as_ref()
            .is_some_and(|b| b.id == mark.id && b.index as usize == offset);
        if !belongs {
            bail!(
                "the log's final commit {} is not contiguous; refusing to guess what to drop",
                mark.id
            );
        }
    }
    if mark.is_last() {
        return Ok(None);
    }
    debug_assert!(first < starts.len());
    Ok(Some(first))
}

fn batch_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the OS must provide randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 3339 in UTC to the second. No date crate: the only consumers are a
/// human reading the log and a client echoing the string back.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm, public domain.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        // A leap day, where naive implementations go wrong.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
