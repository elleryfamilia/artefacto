//! `artefacto list`: every artifact for this repository, newest first.
//!
//! Spec 5: "one row per artifact: id, kind, title, revision, relative age,
//! open and unanchored thread counts, last verdict, source path, and whether
//! that path still exists. `--json` adds absolute timestamps and the poster
//! path. It works with no server running, because it reads the registry,
//! not the log."

use crate::index::{self, Entry, Index};
use anyhow::Result;
use std::path::Path;

pub fn list(json: bool) -> Result<()> {
    let dir = crate::commands::serve::current_state_dir()?;
    let index = Index::load(&dir);
    let now = crate::time::now_secs();
    let rows: Vec<serde_json::Value> = index
        .entries()
        .iter()
        .map(|e| row_json(&dir, e, now))
        .collect();
    if json {
        let doc = serde_json::json!({
            "ok": true,
            "state_dir": dir.to_string_lossy(),
            "readonly": index.is_readonly(),
            "corrupt": index.is_corrupt(),
            "unreadable_rows": index.unreadable_rows(),
            "artifacts": rows,
        });
        println!("{doc}");
        return Ok(());
    }
    let notes = notes(&index);
    for note in &notes {
        eprintln!("{note}");
    }
    // "No artifacts yet" is for a registry with nothing in it, not for one
    // whose rows could not be read: the two would contradict each other.
    if rows.is_empty() && notes.is_empty() {
        println!("no artifacts yet; render or push a plan");
    }
    for row in &rows {
        print!("{}", row_text(row));
    }
    Ok(())
}

/// What a reader should be told about the file itself, when anything. The
/// served index page shows the same lines.
pub fn notes(index: &Index) -> Vec<String> {
    let mut notes = Vec::new();
    if index.is_readonly() {
        notes.push(
            "index.json was written by a newer artefacto; update artefacto to read it".to_string(),
        );
    }
    if index.is_corrupt() {
        notes.push(
            "index.json could not be read; the next render or push replaces it and keeps the \
             old file as index.json.corrupt"
                .to_string(),
        );
    }
    match index.unreadable_rows() {
        0 => {}
        1 => notes.push(
            "1 row in index.json could not be read by this artefacto and is kept as it is"
                .to_string(),
        ),
        n => notes.push(format!(
            "{n} rows in index.json could not be read by this artefacto and are kept as they are"
        )),
    }
    notes
}

/// One row, as `list --json` and the served index both see it. The facts
/// come from the registry; `source_exists`, `age`, and `poster` are
/// computed now, because they are about this moment and this machine.
pub fn row_json(dir: &Path, e: &Entry, now: u64) -> serde_json::Value {
    let poster = index::poster_path(dir, &e.id);
    serde_json::json!({
        "id": e.id,
        "kind": e.kind,
        "title": e.title,
        "plan_hash": e.plan_hash,
        "revision": e.revision,
        "revised_at": e.revised_at,
        "recorded_at": e.recorded_at,
        "age": index::age_label(&e.revised_at, now),
        "open_threads": e.open_threads,
        "unanchored_threads": e.unanchored_threads,
        "submitted": e.submitted,
        "verdict": e.verdict,
        "source_path": e.source_path,
        "source_exists": Path::new(&e.source_path).exists(),
        "rendered_path": e.rendered_path,
        "poster": poster.exists().then(|| poster.display().to_string()),
    })
}

fn row_text(row: &serde_json::Value) -> String {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let revision = match row["revision"].as_u64() {
        Some(0) | None => "not pushed".to_string(),
        Some(n) => format!("rev {n}"),
    };
    let mut out = format!(
        "{}  \"{}\"  {}  {}  threads: {} open, {} unanchored  verdict: {}\n",
        s(&row["id"]),
        s(&row["title"]),
        revision,
        s(&row["age"]),
        row["open_threads"],
        row["unanchored_threads"],
        row["verdict"].as_str().unwrap_or("none"),
    );
    out.push_str(&format!("    {}", s(&row["source_path"])));
    if row["source_exists"] != true {
        out.push_str("  (missing)");
    }
    if let Some(rendered) = row["rendered_path"].as_str() {
        out.push_str(&format!("  rendered {rendered}"));
    }
    out.push('\n');
    out
}
