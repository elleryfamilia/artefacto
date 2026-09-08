//! The machine-readable first line of every generated file.
//!
//! Tools read this line back to decide whether a rendered page is still fresh
//! for its plan. The exact bytes are frozen in
//! `tests/fixtures/marker/first-line.txt`. Anything that consumes artefacto's
//! output pins the same bytes on its side.

/// Prefix of the machine-readable first line.
pub const MARKER_PREFIX: &str = "<!-- artefacto:generated";

/// The complete first line for a document fingerprinted by `hash`.
/// No trailing newline; the caller joins it to the body.
pub fn line(hash: &str) -> String {
    format!("{MARKER_PREFIX} context={hash} -->")
}

/// Read the fingerprint back out of a generated document. Returns the hash
/// from the last marker line found, or `None` if there is no marker line
/// carrying a `context=` token.
pub fn extract_hash(content: &str) -> Option<String> {
    let mut last = None;
    for raw in content.lines() {
        let Some(rest) = raw.trim_start().strip_prefix(MARKER_PREFIX) else {
            continue;
        };
        let Some(token) = rest.trim_start().strip_prefix("context=") else {
            continue;
        };
        let hash: String = token.chars().take_while(|c| !c.is_whitespace()).collect();
        if !hash.is_empty() {
            last = Some(hash);
        }
    }
    last
}
