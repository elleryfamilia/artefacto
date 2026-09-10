//! The append-only event log. One per server, one server-wide `seq`.
//!
//! Holds only its own lock (`Shared.log`), and holds it across `fsync`. That
//! is the reason it is a separate lock: appends serialize against each other
//! without blocking any other route. It never takes `core` or `sockets`.
//!
//! Recovery rule: an **unterminated** final line is a torn write and is
//! truncated. Anything else that does not parse — a bad line in the middle, a
//! sequence gap, a foreign format — is a hard error. Dropping committed
//! history quietly is worse than refusing to start.

use crate::server::event::{Actor, Event, EVENT_FORMAT};
use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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

            while offset < bytes.len() {
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
        if self.poisoned {
            bail!("the event log is in an unknown state after a failed write; restart the server");
        }
        let event = Event {
            format: EVENT_FORMAT.to_string(),
            seq: self.next_seq,
            ts: now_rfc3339(),
            artifact: artifact.to_string(),
            revision,
            actor,
            r#type: kind.to_string(),
            data,
        };
        let line = serde_json::to_string(&event)?;
        // From here the write may be partially visible on disk, so any failure
        // poisons rather than being retried at the same sequence number.
        if let Err(e) = writeln!(self.file, "{line}").and_then(|()| self.file.sync_all()) {
            self.poisoned = true;
            return Err(e).with_context(|| format!("appending to {}", self.path.display()));
        }
        self.next_seq += 1;
        self.events.push(event.clone());
        Ok(event)
    }
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
