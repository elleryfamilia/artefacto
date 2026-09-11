//! The artifact index (spec 4.4): every artifact artefacto has produced for
//! this repository, in one registry file, with a drawn poster per row.
//!
//! Not a view over the event log, on purpose: `clean` truncates the log and
//! a static `render` is made with no server running, so the index is its
//! own file, `index.json` in the state directory. `render` writes it, the
//! server writes it whenever a review's facts change, and `list` and the
//! served index page read it. It keeps the conventions rosita settled on
//! for its Recents registry: a file written by a newer artefacto is left
//! alone and read as empty, a corrupt file loads empty and is repaired by
//! the next write, and nothing is ever pruned on the user's behalf — an
//! absent source file usually means an unmounted volume, not a dead
//! artifact.
//!
//! Two writers can race: a `render` in a shell while the server records a
//! thread. Every write takes an advisory lock on `index.lock`, reloads the
//! file, applies its one change, and writes atomically, so neither loses
//! the other's row.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

pub const INDEX_FILE: &str = "index.json";
pub const POSTERS_DIR: &str = "posters";
/// The format this binary writes. A file declaring a higher number was
/// written by a newer artefacto and is never rewritten by this one.
pub const INDEX_FORMAT: &str = "artefacto.index/1";
const INDEX_VERSION: u32 = 1;

/// One artifact. `id` is the key: `plan:<meta.id>`, the same id the server
/// uses, so a render and a push of the same plan are one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub plan_hash: String,
    /// Absolute. Where the plan came from; the row says whether it is still
    /// there.
    pub source_path: String,
    /// The static render's output, when the last record was a `render`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_path: Option<String>,
    /// The server's revision; 0 for a plan that was rendered and never
    /// pushed.
    pub revision: u32,
    /// When the last revision was made, which is what "how long ago" is
    /// computed from.
    pub revised_at: String,
    /// When this row was last written.
    pub recorded_at: String,
    #[serde(default)]
    pub open_threads: usize,
    #[serde(default)]
    pub unanchored_threads: usize,
    #[serde(default)]
    pub submitted: bool,
    /// The last verdict, once a review has been sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Fields a newer artefacto may have written, carried through a rewrite
    /// by this one.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The file as written: rows are kept raw so that one row this binary
/// cannot read is carried through a rewrite untouched rather than costing
/// every other row.
#[derive(Debug, Serialize, Deserialize)]
struct IndexFile {
    format: String,
    artifacts: Vec<serde_json::Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl Default for IndexFile {
    fn default() -> Self {
        IndexFile {
            format: INDEX_FORMAT.to_string(),
            artifacts: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// The row a static `render` records, and its poster. A render knows the
/// plan and where its page went; it knows nothing of the review, so the
/// review's facts — revision, its time, the counts, the verdict — come from
/// the row already there. A plan never pushed has none, and the render is
/// then the revision: `revised_at` is now.
pub fn render_row(
    plan: &crate::plan::model::Plan,
    source_path: String,
    rendered_path: String,
    previous: Option<&Entry>,
) -> (Entry, String) {
    let now = crate::time::now_rfc3339();
    let review = previous.filter(|p| p.revision > 0);
    let state = crate::plan::poster::ReviewState {
        revision: review.map(|p| p.revision).unwrap_or(0),
        open_threads: review.map(|p| p.open_threads).unwrap_or(0),
        unanchored_threads: review.map(|p| p.unanchored_threads).unwrap_or(0),
        submitted: review.map(|p| p.submitted).unwrap_or(false),
        verdict: review.and_then(|p| p.verdict.clone()),
    };
    let poster = crate::plan::poster::poster_svg(plan, &state);
    let entry = Entry {
        id: crate::server::push::artifact_id(plan),
        kind: "plan".to_string(),
        title: plan.meta.title.clone(),
        plan_hash: crate::plan::model::plan_hash(plan),
        source_path,
        rendered_path: Some(rendered_path),
        revision: state.revision,
        revised_at: review.map(|p| p.revised_at.clone()).unwrap_or(now),
        recorded_at: String::new(),
        open_threads: state.open_threads,
        unanchored_threads: state.unanchored_threads,
        submitted: state.submitted,
        verdict: state.verdict,
        extra: BTreeMap::new(),
    };
    (entry, poster)
}

/// Which events change what a row says. A reply, an answer, a reviewed
/// mark, or a chat message changes nothing the index shows.
pub fn changes_row(event_type: &str) -> bool {
    matches!(
        event_type,
        "revision.published"
            | "thread.opened"
            | "thread.deleted"
            | "thread.resolved"
            | "review.submitted"
    )
}

/// The rows to write after `events` were folded: one per distinct artifact
/// that a row-changing event named, with its poster.
pub fn rows_for(
    review: &crate::server::review::Review,
    events: &[crate::server::event::Event],
) -> Vec<(Entry, String)> {
    let mut seen = std::collections::BTreeSet::new();
    events
        .iter()
        .filter(|e| !e.artifact.is_empty() && changes_row(&e.r#type))
        .filter(|e| seen.insert(e.artifact.clone()))
        .filter_map(|e| review.artifacts.get(&e.artifact))
        .filter_map(review_entry)
        .collect()
}

/// The row a live review records, and its poster, from the folded artifact.
/// `None` when the stored plan will not parse even leniently, which a
/// validated push cannot produce; the row is then left as it was.
pub fn review_entry(artifact: &crate::server::review::Artifact) -> Option<(Entry, String)> {
    use crate::server::review::ThreadStatus;
    let raw = serde_json::to_string(&artifact.plan).ok()?;
    let plan = crate::plan::model::parse(&raw, true).ok()?.plan;
    let count = |status: ThreadStatus| {
        artifact
            .threads
            .iter()
            .filter(|t| t.status == status)
            .count()
    };
    let state = crate::plan::poster::ReviewState {
        revision: artifact.revision,
        open_threads: count(ThreadStatus::Open),
        unanchored_threads: count(ThreadStatus::Unanchored),
        submitted: artifact.submitted,
        verdict: artifact.verdict.clone(),
    };
    let poster = crate::plan::poster::poster_svg(&plan, &state);
    let entry = Entry {
        id: artifact.id.clone(),
        kind: artifact
            .id
            .split_once(':')
            .map(|(kind, _)| kind)
            .unwrap_or_default()
            .to_string(),
        title: plan.meta.title.clone(),
        plan_hash: artifact.plan_hash.clone(),
        source_path: artifact.source_path.clone(),
        rendered_path: None,
        revision: artifact.revision,
        revised_at: artifact.revised_at.clone(),
        recorded_at: String::new(),
        open_threads: state.open_threads,
        unanchored_threads: state.unanchored_threads,
        submitted: state.submitted,
        verdict: state.verdict,
        extra: BTreeMap::new(),
    };
    Some((entry, poster))
}

/// The version number of an `artefacto.index/N` string, if it is one.
fn format_version(format: &str) -> Option<u32> {
    format.strip_prefix("artefacto.index/")?.parse().ok()
}

/// The registry as read from disk. A reader; the writers are the free
/// functions below, which reload under the lock.
#[derive(Debug)]
pub struct Index {
    dir: PathBuf,
    /// The rows this binary could read.
    entries: Vec<Entry>,
    /// Rows it could not, kept verbatim and written back as they are.
    unreadable: Vec<serde_json::Value>,
    extra: BTreeMap<String, serde_json::Value>,
    readonly: bool,
    /// The file was there and was not an index. It reads as empty, and the
    /// next write keeps the old bytes aside as `index.json.corrupt`.
    corrupt: bool,
}

impl Index {
    pub fn load(dir: &Path) -> Index {
        let path = dir.join(INDEX_FILE);
        let mut index = Index {
            dir: dir.to_path_buf(),
            entries: Vec::new(),
            unreadable: Vec::new(),
            extra: BTreeMap::new(),
            readonly: false,
            corrupt: false,
        };
        if let Ok(text) = fs::read_to_string(&path) {
            index.parse(&text);
        }
        index
    }

    /// The version is read before the rows are parsed: a newer artefacto's
    /// rows may have a shape this binary cannot read, and that must come
    /// out as "newer, leave it alone", never as "corrupt, overwrite it".
    /// Rows are then parsed one at a time, so one bad row costs that row's
    /// listing and nothing else: it is carried through the next write as
    /// it is, because the registry never prunes on the user's behalf.
    fn parse(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            // Not JSON: corrupt. It reads as empty and the next write
            // repairs it, keeping the old bytes aside, because the registry
            // is a convenience and not a record anything else depends on.
            self.corrupt = true;
            return;
        };
        let version = value
            .get("format")
            .and_then(|f| f.as_str())
            .and_then(format_version);
        match version {
            // Newer: read-only. A rewrite would destroy structure this
            // binary cannot represent, and the bytes are preserved.
            Some(v) if v > INDEX_VERSION => self.readonly = true,
            Some(_) => match serde_json::from_value::<IndexFile>(value) {
                Ok(file) => {
                    self.extra = file.extra;
                    for row in file.artifacts {
                        match serde_json::from_value::<Entry>(row.clone()) {
                            Ok(entry) => self.entries.push(entry),
                            Err(_) => self.unreadable.push(row),
                        }
                    }
                }
                // The envelope itself is wrong: not an index.
                Err(_) => self.corrupt = true,
            },
            None => self.corrupt = true,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    /// True when the file was written by a newer artefacto: it reads as
    /// empty and every write is refused.
    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    /// True when the file was there and was not an index at all. Every row
    /// it held is unreadable, and the next write keeps its bytes aside.
    pub fn is_corrupt(&self) -> bool {
        self.corrupt
    }

    /// Rows this binary could not read, kept as they are.
    pub fn unreadable_rows(&self) -> usize {
        self.unreadable.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Newest first by `revised_at`; the most recently recorded first among
    /// rows revised in the same second, because `record` puts a row at the
    /// front and the sort is stable.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.entries.iter().collect();
        v.sort_by(|a, b| b.revised_at.cmp(&a.revised_at));
        v
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.path();
        let tmp = self.dir.join("index.json.tmp");
        let file = IndexFile {
            format: INDEX_FORMAT.to_string(),
            artifacts: self
                .entries
                .iter()
                .map(|e| serde_json::to_value(e).expect("a row serializes"))
                .chain(self.unreadable.iter().cloned())
                .collect(),
            extra: self.extra.clone(),
        };
        let text = serde_json::to_string_pretty(&file)?;
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        if self.corrupt && path.exists() {
            // Whatever was there is not thrown away: a file that was not an
            // index may still be something the user wants to look at. Nor is
            // an earlier copy: a second repair keeps its own.
            let aside = aside_path(&self.dir);
            fs::rename(&path, &aside)
                .with_context(|| format!("keeping {} aside", path.display()))?;
        }
        // No fsync, on purpose. The server writes this under its commit
        // gate, and a full fsync on macOS costs tens of milliseconds per
        // commit for a file that is a convenience: a crash that loses the
        // newest row loses nothing the next state change does not write
        // again, and a torn file loads empty and is repaired by that write.
        // The rename still makes each row's contents all-or-nothing.
        fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }
}

/// `index.json.corrupt`, or the first free `index.json.corrupt.N` when one
/// is already there.
fn aside_path(dir: &Path) -> PathBuf {
    let base = dir.join("index.json.corrupt");
    if !base.exists() {
        return base;
    }
    (1..)
        .map(|n| dir.join(format!("index.json.corrupt.{n}")))
        .find(|p| !p.exists())
        .expect("the naturals do not run out")
}

/// What a write did. `ReadOnlyNewer` is the one refusal that is not an
/// error: the caller reports it and carries on, because the index is never
/// the reason a render or a push fails.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Recorded,
    ReadOnlyNewer,
}

/// Upsert `entry` by id, and write its poster beside it. The row goes to
/// the front. Under the lock: a concurrent writer's row is reloaded before
/// this one is added, not overwritten.
///
/// A render and a push of one plan are one row, and each knows something
/// the other does not: the render its output path, the push nothing of it.
/// A field the new row leaves empty keeps the old row's value, so a push
/// does not erase where the static page was written, and fields a newer
/// artefacto wrote survive a rewrite by this one.
pub fn record(dir: &Path, entry: Entry, poster_svg: Option<&str>) -> Result<Outcome> {
    let id = entry.id.clone();
    record_with(dir, &id, |_| (entry, poster_svg.map(str::to_string)))
}

/// `record`, with the row built from the one already there. `build` runs
/// under the lock and sees the previous row for `id`, if any, so a writer
/// that knows only half the facts — a static render, which knows the plan
/// and nothing of the review — can keep the other half.
pub fn record_with(
    dir: &Path,
    id: &str,
    build: impl FnOnce(Option<&Entry>) -> (Entry, Option<String>),
) -> Result<Outcome> {
    let _lock = IndexLock::acquire(dir)?;
    let mut index = Index::load(dir);
    if index.readonly {
        return Ok(Outcome::ReadOnlyNewer);
    }
    let previous = index.entries.iter().find(|e| e.id == id).cloned();
    let (mut entry, poster) = build(previous.as_ref());
    entry.recorded_at = crate::time::now_rfc3339();
    if let Some(svg) = &poster {
        write_poster(dir, &entry.id, svg)?;
    }
    if let Some(previous) = &previous {
        if entry.rendered_path.is_none() {
            entry.rendered_path = previous.rendered_path.clone();
        }
        for (key, value) in &previous.extra {
            entry
                .extra
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
    index.entries.retain(|e| e.id != entry.id);
    // A row this binary could not read under the same id is replaced too:
    // the id now has a row that can be read, and two rows with one id is
    // a registry that lists one and can never clear the other.
    index
        .unreadable
        .retain(|row| row.get("id") != Some(&serde_json::Value::from(entry.id.as_str())));
    index.entries.insert(0, entry);
    index.save()?;
    Ok(Outcome::Recorded)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    Removed,
    Absent,
    ReadOnlyNewer,
}

/// Forget one row and its poster. Never touches the artifact's own files:
/// the source plan and a static render belong to the user.
pub fn remove(dir: &Path, id: &str) -> Result<Removed> {
    let _lock = IndexLock::acquire(dir)?;
    let mut index = Index::load(dir);
    if index.readonly {
        return Ok(Removed::ReadOnlyNewer);
    }
    let before = index.entries.len() + index.unreadable.len();
    index.entries.retain(|e| e.id != id);
    index
        .unreadable
        .retain(|row| row.get("id") != Some(&serde_json::Value::from(id)));
    if index.entries.len() + index.unreadable.len() == before {
        return Ok(Removed::Absent);
    }
    index.save()?;
    let _ = fs::remove_file(poster_path(dir, id));
    Ok(Removed::Removed)
}

/// `<state dir>/posters/<artifact id>.svg`. Artifact ids are constrained to
/// `[A-Za-z0-9:_.-]` by the mint and the model, so the id is a safe file
/// name as it is.
pub fn poster_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(POSTERS_DIR).join(format!("{id}.svg"))
}

fn write_poster(dir: &Path, id: &str, svg: &str) -> Result<PathBuf> {
    let path = poster_path(dir, id);
    let parent = path.parent().expect("posters dir");
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = parent.join(format!("{id}.svg.tmp"));
    fs::write(&tmp, svg).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(path)
}

/// An advisory exclusive lock on `index.lock`, held while a writer reloads,
/// changes, and saves. Blocking: the critical section is one small file.
struct IndexLock {
    _file: fs::File,
}

impl IndexLock {
    fn acquire(dir: &Path) -> Result<IndexLock> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("index.lock"))
            .context("opening index.lock")?;
        // SAFETY: flock on a valid owned descriptor; it blocks until the
        // other holder releases and touches no memory.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            anyhow::bail!("locking index.lock: {}", std::io::Error::last_os_error());
        }
        Ok(IndexLock { _file: file })
    }
}

/// "How long ago", from a row's `revised_at`. Elapsed time, not calendar
/// days: "yesterday" is between one and two days ago. Empty when the
/// timestamp is not one this crate wrote, because display sugar must never
/// fail a listing.
pub fn age_label(revised_at: &str, now_secs: u64) -> String {
    let Some(then) = crate::time::parse_rfc3339(revised_at) else {
        return String::new();
    };
    let secs = now_secs.saturating_sub(then);
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match secs {
        0..=59 => "just now".to_string(),
        60..=3_599 => plural(secs / 60, "minute"),
        3_600..=86_399 => plural(secs / 3_600, "hour"),
        86_400..=172_799 => "yesterday".to_string(),
        172_800..=2_591_999 => plural(secs / 86_400, "day"),
        _ => format!("on {}", &revised_at[..10]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, revised_at: &str) -> Entry {
        Entry {
            id: id.to_string(),
            kind: "plan".to_string(),
            title: id.to_string(),
            plan_hash: "sha256:x".to_string(),
            source_path: format!("/tmp/{id}.json"),
            rendered_path: None,
            revision: 0,
            revised_at: revised_at.to_string(),
            recorded_at: String::new(),
            open_threads: 0,
            unanchored_threads: 0,
            submitted: false,
            verdict: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn ages_read_correctly_either_side_of_each_boundary() {
        let now = 1_789_000_000u64;
        let at = |ago: u64| crate::time::rfc3339(now - ago);
        assert_eq!(age_label(&at(0), now), "just now");
        assert_eq!(age_label(&at(59), now), "just now");
        assert_eq!(age_label(&at(60), now), "1 minute ago");
        assert_eq!(age_label(&at(3_599), now), "59 minutes ago");
        assert_eq!(age_label(&at(3_600), now), "1 hour ago");
        assert_eq!(age_label(&at(86_399), now), "23 hours ago");
        assert_eq!(
            age_label(&at(86_400), now),
            "yesterday",
            "one day: the boundary"
        );
        assert_eq!(age_label(&at(172_799), now), "yesterday");
        assert_eq!(age_label(&at(172_800), now), "2 days ago");
        assert_eq!(age_label(&at(29 * 86_400), now), "29 days ago");
        assert_eq!(age_label(&at(30 * 86_400), now), "on 2026-08-11");
        assert_eq!(
            age_label("2026-09-10T05:46:40Z", 0),
            "just now",
            "a clock behind the row"
        );
        assert_eq!(age_label("last tuesday", now), "");
    }

    #[test]
    fn a_record_goes_to_the_front_and_a_repeat_replaces_its_row() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), entry("plan:a", "2026-09-10T00:00:00Z"), None).unwrap();
        record(dir.path(), entry("plan:b", "2026-09-10T00:00:00Z"), None).unwrap();
        let ids = |i: &Index| i.entries().iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&Index::load(dir.path())),
            ["plan:b", "plan:a"],
            "same second: last recorded first"
        );

        let mut again = entry("plan:a", "2026-09-10T00:00:00Z");
        again.title = "A, again".to_string();
        record(dir.path(), again, None).unwrap();
        let index = Index::load(dir.path());
        assert_eq!(ids(&index), ["plan:a", "plan:b"]);
        assert_eq!(index.get("plan:a").unwrap().title, "A, again");
        assert_eq!(index.entries().len(), 2, "one row per id");
    }

    #[test]
    fn newest_revision_comes_first_whatever_the_record_order() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), entry("plan:new", "2026-09-11T00:00:00Z"), None).unwrap();
        record(dir.path(), entry("plan:old", "2026-09-01T00:00:00Z"), None).unwrap();
        let index = Index::load(dir.path());
        let ids: Vec<&str> = index.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["plan:new", "plan:old"]);
    }

    #[test]
    fn a_file_from_a_newer_artefacto_is_read_as_empty_and_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        let future = r#"{"format":"artefacto.index/2","artifacts":[{"id":"plan:future","shape":"unknown"}],"aisle":7}"#;
        fs::write(&path, future).unwrap();
        let index = Index::load(dir.path());
        assert!(index.is_readonly());
        assert!(index.is_empty());
        assert_eq!(
            record(
                dir.path(),
                entry("plan:a", "2026-09-10T00:00:00Z"),
                Some("<svg/>")
            )
            .unwrap(),
            Outcome::ReadOnlyNewer
        );
        assert_eq!(
            remove(dir.path(), "plan:future").unwrap(),
            Removed::ReadOnlyNewer
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            future,
            "bytes preserved"
        );
        assert!(
            !poster_path(dir.path(), "plan:a").exists(),
            "no poster for a row that was not written"
        );
    }

    #[test]
    fn a_corrupt_file_loads_empty_and_the_next_record_repairs_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        for junk in [
            "{not json",
            r#"{"format":"artefacto.feedback/1","artifacts":[]}"#,
            r#"{"format":"artefacto.index/1","artifacts":{"not":"a list"}}"#,
            "",
        ] {
            fs::write(&path, junk).unwrap();
            let index = Index::load(dir.path());
            assert!(!index.is_readonly(), "{junk:?}");
            assert!(index.is_empty(), "{junk:?}");
            record(dir.path(), entry("plan:a", "2026-09-10T00:00:00Z"), None).unwrap();
            let repaired = Index::load(dir.path());
            assert_eq!(repaired.entries().len(), 1, "{junk:?}");
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(INDEX_FORMAT), "{text}");
        }
    }

    #[test]
    fn one_unreadable_row_is_carried_not_corrupt_and_gives_way_to_its_own_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        fs::write(
            &path,
            r#"{"format":"artefacto.index/1","artifacts":[{"id":"plan:x","revision":"bad"},{"id":"plan:y"}]}"#,
        )
        .unwrap();
        let index = Index::load(dir.path());
        assert!(!index.is_corrupt());
        assert_eq!(index.unreadable_rows(), 2);
        assert!(index.is_empty());

        // A row for another id carries both through.
        record(dir.path(), entry("plan:a", "2026-09-10T00:00:00Z"), None).unwrap();
        let index = Index::load(dir.path());
        assert_eq!(index.unreadable_rows(), 2, "kept as they were");
        assert_eq!(index.entries().len(), 1);

        // A row for the same id replaces the one that could not be read.
        record(dir.path(), entry("plan:x", "2026-09-10T00:00:00Z"), None).unwrap();
        let index = Index::load(dir.path());
        assert_eq!(index.unreadable_rows(), 1, "plan:x is one row again");
        assert_eq!(index.entries().len(), 2);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("\"id\": \"plan:x\"").count(), 1, "{text}");

        // And removing an id the user names forgets the unreadable row too.
        assert_eq!(remove(dir.path(), "plan:y").unwrap(), Removed::Removed);
        assert_eq!(Index::load(dir.path()).unreadable_rows(), 0);
    }

    #[test]
    fn every_repair_keeps_its_own_copy_of_the_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        fs::write(&path, "first junk").unwrap();
        record(dir.path(), entry("plan:a", "2026-09-10T00:00:00Z"), None).unwrap();
        fs::write(&path, "second junk").unwrap();
        record(dir.path(), entry("plan:b", "2026-09-10T00:00:00Z"), None).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("index.json.corrupt")).unwrap(),
            "first junk"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("index.json.corrupt.1")).unwrap(),
            "second junk",
            "the second repair keeps its own copy"
        );
    }

    #[test]
    fn fields_this_binary_does_not_know_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        let mut e = entry("plan:a", "2026-09-10T00:00:00Z");
        e.extra
            .insert("screenshot".to_string(), serde_json::json!("/tmp/a.png"));
        record(dir.path(), e, None).unwrap();
        let mut file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        file["studio"] = serde_json::json!({"pinned": true});
        fs::write(&path, file.to_string()).unwrap();

        record(dir.path(), entry("plan:b", "2026-09-10T00:00:00Z"), None).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"screenshot\""), "row field kept: {text}");
        assert!(text.contains("\"studio\""), "file field kept: {text}");

        // And a rewrite of the row itself, by a writer that knows nothing of
        // the field, keeps it too.
        record(dir.path(), entry("plan:a", "2026-09-10T00:00:00Z"), None).unwrap();
        let index = Index::load(dir.path());
        assert_eq!(
            index.get("plan:a").unwrap().extra.get("screenshot"),
            Some(&serde_json::json!("/tmp/a.png")),
            "kept across the row's own rewrite"
        );
    }

    #[test]
    fn remove_forgets_the_row_and_its_poster_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.json");
        fs::write(&source, "{}").unwrap();
        let mut e = entry("plan:a", "2026-09-10T00:00:00Z");
        e.source_path = source.display().to_string();
        record(dir.path(), e, Some("<svg/>")).unwrap();
        let poster = poster_path(dir.path(), "plan:a");
        assert_eq!(fs::read_to_string(&poster).unwrap(), "<svg/>");

        assert_eq!(remove(dir.path(), "plan:a").unwrap(), Removed::Removed);
        assert!(Index::load(dir.path()).is_empty());
        assert!(!poster.exists(), "the poster goes with the row");
        assert!(source.exists(), "the user's file is never touched");
        assert_eq!(remove(dir.path(), "plan:a").unwrap(), Removed::Absent);
    }

    #[test]
    fn a_missing_source_is_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = entry("plan:gone", "2026-09-10T00:00:00Z");
        e.source_path = dir.path().join("unmounted/plan.json").display().to_string();
        record(dir.path(), e, None).unwrap();
        record(dir.path(), entry("plan:b", "2026-09-10T00:00:00Z"), None).unwrap();
        let index = Index::load(dir.path());
        let gone = index.get("plan:gone").expect("the row is still there");
        assert!(!Path::new(&gone.source_path).exists());
    }

    #[test]
    fn concurrent_records_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    record(
                        &path,
                        entry(&format!("plan:t{n}"), "2026-09-10T00:00:00Z"),
                        Some("<svg/>"),
                    )
                    .unwrap()
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Outcome::Recorded);
        }
        assert_eq!(
            Index::load(&path).entries().len(),
            8,
            "every writer's row survived"
        );
    }
}
