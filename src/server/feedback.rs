//! `artefacto.feedback/1`: what a submitted review looks like on disk.
//!
//! # The order is the whole point
//!
//! The file is written **before** the `review.submitted` event that names it.
//! An earlier draft of this had it the other way round — append the event,
//! then work out the path, then put the path into the already-appended event —
//! which is not a thing an append-only log can do.
//!
//! # And the write is atomic
//!
//! `std::fs::write` truncates and then fills. A reader that arrives in between
//! finds a short document and cannot tell it from a complete one, because
//! nothing in the format says how long it should be. So: temp file, `fsync`,
//! rename.
//!
//! # The ids come from the fold, not from the page
//!
//! Spec 6.6: comment ids are server-assigned and stable, `c-<n>` per artifact,
//! never renumbered. The document is built from the folded `Review` for that
//! reason — whatever the page believes about ids is not authoritative.

use crate::server::review::{Review, ThreadStatus};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const FEEDBACK_FORMAT: &str = "artefacto.feedback/1";

/// `<stem>-feedback.json`, beside the plan file that was pushed.
///
/// Spec 6.7 puts it there so "the file-based loop keeps working": an agent
/// with no server, or a later session, reads the review from the repository
/// rather than from a log it no longer has.
pub fn feedback_path(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "plan".to_string());
    source.with_file_name(format!("{stem}-feedback.json"))
}

/// Write the document where a reader will find it, and return that path so the
/// caller can put it **into** the event it is about to append.
pub fn write(source: &Path, document: &serde_json::Value) -> Result<PathBuf> {
    let final_path = feedback_path(source);
    let tmp = final_path.with_extension("json.tmp");
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    serde_json::to_writer_pretty(&file, document)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(final_path)
}

/// Build the document from folded state.
///
/// A superset of the v1 feedback document artefacto inherited — spec 6.6 names
/// it — so a reader that knows the old fields finds all of them under the new
/// name: `format`, `plan_id`, `plan_hash`, `verdict`, and `comments[]` with
/// `id`, `ref`, `quote`, `text` and `blocking`. Spec 6.6 adds `base_revision`,
/// per-comment `status` and `replies[]`, plus `answers[]` and `reviewed[]`.
pub fn document(
    review: &Review,
    artifact_id: &str,
    verdict: &str,
    base_revision: u32,
) -> serde_json::Value {
    let Some(artifact) = review.artifacts.get(artifact_id) else {
        return serde_json::json!({ "format": FEEDBACK_FORMAT, "verdict": verdict });
    };
    let plan_id = artifact
        .plan
        .pointer("/meta/id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let comments: Vec<serde_json::Value> = artifact
        .threads
        .iter()
        .map(|thread| {
            // The opening message is the comment; everything after it is a
            // reply, whoever wrote it.
            let mut messages = thread.messages.iter();
            let opening = messages.next();
            serde_json::json!({
                "id": thread.id,
                "ref": thread.target,
                "quote": (!thread.quote.is_empty()).then(|| thread.quote.clone()),
                "text": opening.map(|m| m.text.clone()).unwrap_or_default(),
                "blocking": thread.blocking,
                "asked": thread.asked,
                "status": thread.status.as_str(),
                "replies": messages
                    .map(|m| serde_json::json!({ "actor": m.actor, "text": m.text, "ts": m.ts }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    serde_json::json!({
        "format": FEEDBACK_FORMAT,
        "plan_id": plan_id,
        "plan_hash": artifact.plan_hash,
        "verdict": verdict,
        "base_revision": base_revision,
        "comments": comments,
        // An answer the reviewer removed is stored as empty text; it is not
        // an answer.
        "answers": artifact
            .answers
            .iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(question, text)| serde_json::json!({ "question": question, "text": text }))
            .collect::<Vec<_>>(),
        "reviewed": artifact.reviewed.iter().collect::<Vec<_>>(),
    })
}

/// Spec 6.6's old rule, kept: `request_changes` if any open comment blocks.
///
/// The page sends a verdict and the reviewer may choose `approve` outright, so
/// this only ever raises `comment` to `request_changes` — it never overrides
/// what the reviewer actually chose.
pub fn settle_verdict(review: &Review, artifact_id: &str, chosen: &str) -> String {
    if chosen != "comment" {
        return chosen.to_string();
    }
    let blocking = review.artifacts.get(artifact_id).is_some_and(|artifact| {
        artifact
            .threads
            .iter()
            .any(|t| t.blocking && t.status == ThreadStatus::Open)
    });
    if blocking {
        "request_changes".to_string()
    } else {
        chosen.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::event::{Actor, Event, EVENT_FORMAT};
    use crate::server::fold::fold;

    fn ev(seq: u64, actor: Actor, kind: &str, data: serde_json::Value) -> Event {
        Event {
            format: EVENT_FORMAT.to_string(),
            seq,
            ts: "2026-09-10T00:00:00Z".to_string(),
            artifact: "plan:demo".to_string(),
            revision: 1,
            actor,
            r#type: kind.to_string(),
            data,
            batch: None,
        }
    }

    fn reviewed() -> Review {
        fold(&[
            ev(
                1,
                Actor::Agent,
                "revision.published",
                serde_json::json!({
                    "plan": {
                        "format": "artefacto.plan/1",
                        "meta": { "id": "demo", "title": "Demo" },
                        "phases": [{ "id": "p-one", "tasks": [{ "id": "t-a" }] }]
                    },
                    "plan_hash": "sha256:abc",
                }),
            ),
            ev(
                2,
                Actor::Reviewer,
                "thread.opened",
                serde_json::json!({
                    "thread": "c-1", "ref": "task:t-a", "text": "why this?",
                    "blocking": true, "quote": "the line",
                }),
            ),
            ev(
                3,
                Actor::Agent,
                "thread.replied",
                serde_json::json!({ "thread": "c-1", "text": "because" }),
            ),
            ev(
                4,
                Actor::Reviewer,
                "question.answered",
                serde_json::json!({ "question": "q-ttl", "text": "an hour" }),
            ),
            ev(
                5,
                Actor::Reviewer,
                "element.reviewed",
                serde_json::json!({ "ref": "phase:p-one", "on": true }),
            ),
        ])
    }

    #[test]
    fn the_document_is_a_superset_of_the_old_one() {
        let doc = document(&reviewed(), "plan:demo", "request_changes", 1);
        // Everything a reader of the inherited v1 document knows.
        assert_eq!(doc["format"], FEEDBACK_FORMAT);
        assert_eq!(doc["plan_id"], "demo");
        assert_eq!(doc["plan_hash"], "sha256:abc");
        assert_eq!(doc["verdict"], "request_changes");
        assert_eq!(doc["comments"][0]["id"], "c-1");
        assert_eq!(doc["comments"][0]["ref"], "task:t-a");
        assert_eq!(doc["comments"][0]["quote"], "the line");
        assert_eq!(doc["comments"][0]["text"], "why this?");
        assert_eq!(doc["comments"][0]["blocking"], true);
        // And what spec 6.6 adds.
        assert_eq!(doc["base_revision"], 1);
        assert_eq!(doc["comments"][0]["status"], "open");
        assert_eq!(doc["comments"][0]["replies"][0]["text"], "because");
        assert_eq!(doc["comments"][0]["replies"][0]["actor"], "agent");
        assert_eq!(doc["answers"][0]["question"], "q-ttl");
        assert_eq!(doc["answers"][0]["text"], "an hour");
        assert_eq!(doc["reviewed"][0], "phase:p-one");
    }

    #[test]
    fn a_removed_answer_is_not_in_the_document() {
        let mut review = reviewed();
        crate::server::fold::apply(
            &mut review,
            &ev(
                6,
                Actor::Reviewer,
                "question.answered",
                serde_json::json!({ "question": "q-ttl", "text": "" }),
            ),
        );
        let doc = document(&review, "plan:demo", "comment", 1);
        assert!(
            doc["answers"].as_array().unwrap().is_empty(),
            "an emptied answer is a removed one: {doc}"
        );
    }

    #[test]
    fn the_opening_message_is_the_comment_and_the_rest_are_replies() {
        let doc = document(&reviewed(), "plan:demo", "comment", 1);
        assert_eq!(
            doc["comments"][0]["replies"].as_array().unwrap().len(),
            1,
            "the comment's own text is not also one of its replies"
        );
    }

    #[test]
    fn an_open_blocking_comment_raises_comment_to_request_changes() {
        let review = reviewed();
        assert_eq!(
            settle_verdict(&review, "plan:demo", "comment"),
            "request_changes"
        );
        assert_eq!(
            settle_verdict(&review, "plan:demo", "approve"),
            "approve",
            "a reviewer who approves is not overridden by their own earlier comment"
        );
    }

    #[test]
    fn the_path_sits_beside_the_plan() {
        assert_eq!(
            feedback_path(Path::new("/repo/docs/plan.json")),
            Path::new("/repo/docs/plan-feedback.json")
        );
        assert_eq!(
            feedback_path(Path::new("/repo/2026-09-10-auth.json")),
            Path::new("/repo/2026-09-10-auth-feedback.json")
        );
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("plan.json");
        let written = write(&source, &serde_json::json!({ "format": FEEDBACK_FORMAT })).unwrap();
        assert_eq!(written, dir.path().join("plan-feedback.json"));

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["plan-feedback.json"], "a rename leaves no .tmp");
    }

    #[test]
    fn the_final_path_is_never_opened_for_writing() {
        // The difference between a rename and a truncate-in-place is only
        // visible mid-write, which a test cannot watch. This watches the
        // mechanism instead: `File::create` on a read-only file fails, while a
        // rename over one succeeds, because rename needs write permission on
        // the directory rather than on the file.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("plan.json");
        let path = write(&source, &serde_json::json!({ "verdict": "comment" })).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

        write(&source, &serde_json::json!({ "verdict": "approve" }))
            .expect("a rename replaces it; opening it for writing would not");
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["verdict"], "approve");
    }

    #[test]
    fn a_rewrite_replaces_the_document_whole() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("plan.json");
        write(
            &source,
            &serde_json::json!({ "verdict": "comment", "long": "x".repeat(500) }),
        )
        .unwrap();
        let path = write(&source, &serde_json::json!({ "verdict": "approve" })).unwrap();

        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["verdict"], "approve");
        assert!(
            back.get("long").is_none(),
            "no tail of the older, longer document"
        );
    }
}
