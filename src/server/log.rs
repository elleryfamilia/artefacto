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
//!    Anything else that does not parse — a bad line in the middle, a
//!    sequence number that does not go up, a foreign format — is a hard
//!    error. Dropping committed history quietly is worse than refusing to
//!    start. A **gap** in the numbers is not an error: `clean` takes a
//!    finished review's events out and leaves the rest at their numbers.
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

/// Whether this state directory's log holds an artifact: something was
/// pushed here once, so there is a review to open or resume. A log that
/// holds only lease and cursor records — `serve` followed by an `await` —
/// is not one. A substring test, because the log is compact serde output
/// and the type field is written exactly this way; reading every record
/// through the parser just to answer a yes/no would cost the same as
/// starting the server this is deciding whether to start. On bytes, not a
/// decoded string: a torn tail can end inside a multibyte character, and a
/// log the server would truncate and serve must not read as empty here.
pub fn has_artifact(dir: &Path) -> bool {
    const MARK: &[u8] = b"\"type\":\"revision.published\"";
    std::fs::read(dir.join("events.ndjson"))
        .map(|bytes| bytes.windows(MARK.len()).any(|w| w == MARK))
        .unwrap_or(false)
}

/// Whether the log would open: the same reading as [`EventLog::open`] with
/// nothing written back, for a caller that must know before it acts. A
/// torn tail or an interrupted commit passes (open would recover them); a
/// bad line, a foreign format, or a number that does not go up fails.
pub fn check(dir: &Path) -> Result<()> {
    let path = dir.join("events.ndjson");
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    parse(&path, &bytes).map(|_| ())
}

/// Every complete record of `bytes`, and where the good bytes end.
fn parse(path: &Path, bytes: &[u8]) -> Result<(Vec<Event>, usize)> {
    let mut events = Vec::new();
    let mut offset = 0usize;
    let mut line_no = 0usize;
    let mut good_bytes = 0usize;
    // Where each accepted record starts, so an interrupted commit can be
    // cut back to the byte before its first member.
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
        let event: Event = serde_json::from_str(text)
            .with_context(|| format!("{}: line {line_no} is not an event", path.display()))?;
        if event.format != EVENT_FORMAT {
            bail!(
                "{}: line {line_no} declares {}, not {EVENT_FORMAT}",
                path.display(),
                event.format
            );
        }
        // Strictly increasing, not contiguous: `clean` takes a finished
        // review's events out and leaves the rest at their numbers, so a
        // gap is history, not corruption. A number that does not go up is.
        let last = events.last().map(|e: &Event| e.seq).unwrap_or(0);
        if event.seq <= last {
            bail!(
                "{}: line {line_no} has seq {}, not above seq {last}",
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
    Ok((events, good_bytes))
}

impl EventLog {
    pub fn open(dir: &Path) -> Result<EventLog> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("events.ndjson");
        let mut events = Vec::new();

        if path.exists() {
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let (parsed, good_bytes) = parse(&path, &bytes)?;
            events = parsed;
            if good_bytes < bytes.len() {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(good_bytes as u64)?;
                f.sync_all()?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let next_seq = events.last().map(|e| e.seq + 1).unwrap_or(1);
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

/// What `clean` did to the log.
#[derive(Debug, Default)]
pub struct Cleaned {
    /// Artifacts whose review was sent, and whose events are gone.
    pub removed: Vec<String>,
    /// Artifacts still in the log, their review open.
    pub kept: Vec<String>,
    pub events_removed: usize,
}

/// `artefacto clean` (spec 6.7): drop every event of every artifact whose
/// review was sent, keep the rest at their numbers (spec 4.2: "it never
/// renumbers"), and end the log with a `log.cleaned` record numbered past
/// the old high-water mark. That last record is what keeps a cursor
/// honest: an agent that acknowledged seq 50 before the clean must not
/// find the next event numbered 41 and never see it, so the log's highest
/// number survives even when the events that carried it do not. Lease and
/// cursor records name no artifact and stay.
///
/// Runs only with no server, because the server is the log's only writer
/// while it lives; the command stops it first.
pub fn clean(dir: &Path) -> Result<Cleaned> {
    let log = EventLog::open(dir)?;
    let events = log.since(0).to_vec();
    let last_seq = log.last_seq();
    drop(log);
    let review = crate::server::fold::fold(&events);
    let submitted: std::collections::BTreeSet<String> = review
        .artifacts
        .values()
        .filter(|a| a.submitted)
        .map(|a| a.id.clone())
        .collect();
    let kept_ids: Vec<String> = review
        .artifacts
        .keys()
        .filter(|id| !submitted.contains(*id))
        .cloned()
        .collect();
    if submitted.is_empty() {
        return Ok(Cleaned {
            removed: Vec::new(),
            kept: kept_ids,
            events_removed: 0,
        });
    }
    let (kept, removed): (Vec<Event>, Vec<Event>) = events
        .into_iter()
        .partition(|e| !submitted.contains(&e.artifact));
    let marker = Event {
        format: EVENT_FORMAT.to_string(),
        seq: last_seq + 1,
        ts: now_rfc3339(),
        artifact: String::new(),
        revision: 0,
        actor: Actor::Server,
        r#type: "log.cleaned".to_string(),
        data: serde_json::json!({
            "removed": submitted.iter().collect::<Vec<_>>(),
            "events": removed.len(),
        }),
        batch: None,
    };
    let mut buffer = String::with_capacity(4096);
    for event in kept.iter().chain(std::iter::once(&marker)) {
        buffer.push_str(&serde_json::to_string(event)?);
        buffer.push('\n');
    }
    let path = dir.join("events.ndjson");
    let tmp = dir.join("events.ndjson.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(buffer.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(Cleaned {
        removed: submitted.into_iter().collect(),
        kept: kept_ids,
        events_removed: removed.len(),
    })
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

/// Kept here by name for its callers; the implementation lives in `time`.
pub use crate::time::now_rfc3339;
