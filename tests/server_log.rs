use artefacto::server::event::Actor;
use artefacto::server::log::EventLog;

fn data() -> serde_json::Value {
    serde_json::json!({ "ref": "task:t-a" })
}

fn append(log: &mut EventLog, kind: &str) -> u64 {
    log.append("plan:x", 1, Actor::Reviewer, kind, data())
        .unwrap()
        .seq
}

#[test]
fn append_assigns_monotonic_sequence_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(append(&mut log, "thread.opened"), 1);
    assert_eq!(append(&mut log, "thread.opened"), 2);
    assert_eq!(log.last_seq(), 2);
}

#[test]
fn seq_continues_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    let mut reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(reopened.last_seq(), 2, "a restart must not renumber");
    assert_eq!(append(&mut reopened, "thread.opened"), 3);
}

#[test]
fn since_is_exclusive_of_the_cursor_and_reads_no_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    for _ in 0..5 {
        append(&mut log, "thread.opened");
    }
    let got = log.since(2);
    assert_eq!(got.len(), 3, "seq 3, 4, 5");
    assert_eq!(got[0].seq, 3, "the cursor names what was already delivered");
    assert_eq!(got[2].seq, 5);
    assert!(log.since(5).is_empty(), "caught up means nothing");
    assert!(
        log.since(99).is_empty(),
        "a cursor past the end is not an error"
    );
}

#[test]
fn an_unterminated_tail_is_dropped_and_its_sequence_reused() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    // A crash mid-write: a partial line with no terminating newline.
    let path = dir.path().join("events.ndjson");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"format\":\"artefacto.event/1\",\"seq\":3,\"ts\"");
    std::fs::write(&path, raw).unwrap();

    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(log.last_seq(), 2, "a torn line was never a committed event");
    assert_eq!(
        append(&mut log, "thread.opened"),
        3,
        "so seq 3 is still free"
    );
    assert_eq!(log.since(0).len(), 3);

    // And the file is clean: reopening sees exactly three events.
    let reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(
        reopened.since(0).len(),
        3,
        "the torn bytes were truncated, not concatenated"
    );
}

#[test]
fn a_file_ending_mid_newline_is_recovered_without_corruption() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
    }
    let path = dir.path().join("events.ndjson");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.pop(); // drop the trailing newline
    std::fs::write(&path, bytes).unwrap();

    let mut log = EventLog::open(dir.path()).unwrap();
    // The line parses but was never terminated, so it is treated as torn.
    assert_eq!(
        log.last_seq(),
        0,
        "an unterminated line is not a committed event"
    );
    append(&mut log, "thread.opened");
    let reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(
        reopened.since(0).len(),
        1,
        "the next append must not be glued onto the unterminated line"
    );
}

#[test]
fn a_malformed_line_in_the_middle_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    let path = dir.path().join("events.ndjson");
    let lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let corrupted = format!("{}\n{{garbage}}\n{}\n", lines[0], lines[2]);
    std::fs::write(&path, corrupted).unwrap();

    let err =
        EventLog::open(dir.path()).expect_err("committed history must never be dropped silently");
    let text = format!("{err:#}");
    assert!(text.contains("line 2"), "the error names the line: {text}");
}

#[test]
fn a_sequence_gap_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let line = |seq: u64| {
        format!(
            "{{\"format\":\"artefacto.event/1\",\"seq\":{seq},\"ts\":\"t\",\"artifact\":\"plan:x\",\
             \"revision\":1,\"actor\":\"reviewer\",\"type\":\"thread.opened\",\"data\":null}}\n"
        )
    };
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(&path, format!("{}{}", line(1), line(3))).unwrap();

    let err = EventLog::open(dir.path()).expect_err("a gap means an event went missing");
    assert!(format!("{err:#}").contains("expected seq 2"));
}

#[test]
fn a_foreign_format_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("events.ndjson"),
        "{\"format\":\"something.else/9\",\"seq\":1,\"ts\":\"t\",\"artifact\":\"plan:x\",\
         \"revision\":1,\"actor\":\"reviewer\",\"type\":\"thread.opened\",\"data\":null}\n",
    )
    .unwrap();
    assert!(
        EventLog::open(dir.path()).is_err(),
        "a log from another tool is not ours to read"
    );
}

// ---------------------------------------------------------------------------
// Multi-event commits. Codex finding #8: one `write_all` is not a transaction.
// ---------------------------------------------------------------------------

use artefacto::server::log::Pending;

fn pending(kind: &str) -> Pending {
    Pending::new("plan:x", 1, Actor::Reviewer, kind, data())
}

#[test]
fn a_commit_numbers_its_events_in_order_and_marks_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    let written = log
        .append_all(vec![
            pending("revision.published"),
            pending("thread.resolved"),
            pending("thread.resolved"),
        ])
        .unwrap();

    assert_eq!(written.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
    let marks: Vec<_> = written.iter().map(|e| e.batch.clone().unwrap()).collect();
    assert!(
        marks.iter().all(|m| m.id == marks[0].id && m.count == 3),
        "one commit, named once"
    );
    assert_eq!(marks.iter().map(|m| m.index).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(marks[2].is_last());
}

#[test]
fn a_single_event_carries_no_batch_mark() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    let one = log.append_all(vec![pending("chat.sent")]).unwrap();
    assert!(
        one[0].batch.is_none(),
        "the torn-tail rule already makes one line all-or-nothing"
    );
    let plain = log
        .append("plan:x", 1, Actor::Reviewer, "chat.sent", data())
        .unwrap();
    assert!(plain.batch.is_none());
}

/// Cut the log after `keep` whole lines, the way a crash mid-commit leaves it:
/// every surviving line complete and newline-terminated.
fn keep_lines(dir: &std::path::Path, keep: usize) {
    let path = dir.join("events.ndjson");
    let raw = std::fs::read_to_string(&path).unwrap();
    let kept: String = raw.lines().take(keep).map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, kept).unwrap();
}

#[test]
fn an_interrupted_commit_is_dropped_whole() {
    // This is the failure the batch mark exists for. Every surviving line here
    // is complete and terminated, so the torn-tail rule sees nothing wrong —
    // and a revision would have been committed without its resolutions.
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append("plan:x", 1, Actor::Reviewer, "chat.sent", data())
            .unwrap();
        log.append_all(vec![
            pending("revision.published"),
            pending("thread.resolved"),
            pending("thread.resolved"),
        ])
        .unwrap();
    }
    keep_lines(dir.path(), 3); // the chat, the revision, one resolution

    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(
        log.last_seq(),
        1,
        "half a commit is not history; only the event before it survives"
    );
    assert_eq!(log.since(0).len(), 1);
    assert_eq!(log.since(0)[0].r#type, "chat.sent");
    assert_eq!(
        log.append("plan:x", 1, Actor::Reviewer, "chat.sent", data())
            .unwrap()
            .seq,
        2,
        "and the dropped sequence numbers are reused"
    );
}

#[test]
fn an_interrupted_commit_is_dropped_from_the_file_too() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append_all(vec![
            pending("revision.published"),
            pending("thread.resolved"),
        ])
        .unwrap();
    }
    keep_lines(dir.path(), 1);
    EventLog::open(dir.path()).unwrap();

    let raw = std::fs::read_to_string(dir.path().join("events.ndjson")).unwrap();
    assert!(
        raw.is_empty(),
        "the partial commit is cut from the file, not just from memory: {raw}"
    );
}

#[test]
fn a_complete_commit_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append_all(vec![
            pending("revision.published"),
            pending("thread.resolved"),
        ])
        .unwrap();
    }
    let log = EventLog::open(dir.path()).unwrap();
    assert_eq!(log.last_seq(), 2, "nothing incomplete about it");
    assert_eq!(log.since(0).len(), 2);
}

#[test]
fn a_commit_interrupted_at_its_first_record_leaves_nothing_of_it() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append_all(vec![
            pending("revision.published"),
            pending("thread.resolved"),
            pending("thread.resolved"),
        ])
        .unwrap();
    }
    keep_lines(dir.path(), 1);
    let log = EventLog::open(dir.path()).unwrap();
    assert_eq!(log.last_seq(), 0);
}
