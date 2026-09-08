//! The generated first line carries the plan's fingerprint, and tools read it
//! back to decide whether a render is stale. These bytes are frozen: loadout
//! learns to accept them in a later plan, and pins the same fixture on its
//! side. Changing them means changing both, deliberately.

use artefacto::marker;

const ZERO: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn emitted_line_matches_the_frozen_fixture() {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/marker/first-line.txt"
    ))
    .expect("read marker fixture");
    assert_eq!(
        marker::line(ZERO),
        fixture.trim_end_matches('\n'),
        "the generated first line drifted from the frozen contract fixture"
    );
}

#[test]
fn extract_hash_round_trips() {
    assert_eq!(
        marker::extract_hash(&marker::line(ZERO)),
        Some(ZERO.to_string())
    );
}

#[test]
fn extract_hash_reads_the_line_from_a_full_document() {
    let doc = format!(
        "{}\n<!doctype html><html><body>x</body></html>",
        marker::line(ZERO)
    );
    assert_eq!(marker::extract_hash(&doc), Some(ZERO.to_string()));
}

#[test]
fn extract_hash_returns_none_without_a_marker() {
    assert_eq!(marker::extract_hash("<!doctype html><html></html>"), None);
    assert_eq!(
        marker::extract_hash("<!-- artefacto:generated -->"),
        None,
        "right prefix, no context= token"
    );
}
