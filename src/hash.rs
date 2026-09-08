//! Content hashing for plan fingerprints.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Compute `sha256:<hex>` over the canonical JSON of `value`.
///
/// `serde_json` serializes derived structs in field-declaration order and we use
/// `BTreeMap`/sorted `Vec`s for collections, so the encoding is stable across
/// runs without an explicit canonicalization pass.
pub fn context_hash<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_vec(value).expect("context must serialize to JSON");
    let mut hasher = Sha256::new();
    hasher.update(&json);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(7 + digest.len() * 2);
    hex.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Short form of a hash for compact display (`sha256:abcd1234…`).
pub fn short(hash: &str) -> String {
    match hash.strip_prefix("sha256:") {
        Some(hex) if hex.len() > 12 => format!("sha256:{}…", &hex[..12]),
        _ => hash.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_hash_is_prefixed_and_stable() {
        let a = context_hash(&serde_json::json!({"k": 1}));
        let b = context_hash(&serde_json::json!({"k": 1}));
        assert_eq!(a, b, "same input must hash the same");
        assert!(a.starts_with("sha256:"), "got {a}");
        assert_eq!(a.len(), "sha256:".len() + 64, "hex digest is 64 chars");
        assert!(
            a.chars()
                .skip("sha256:".len())
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "digest must be lowercase hex: {a}"
        );
    }

    #[test]
    fn context_hash_changes_with_input() {
        assert_ne!(
            context_hash(&serde_json::json!({"k": 1})),
            context_hash(&serde_json::json!({"k": 2}))
        );
    }

    #[test]
    fn short_truncates_only_long_hashes() {
        let long = format!("sha256:{}", "a".repeat(64));
        assert_eq!(short(&long), "sha256:aaaaaaaaaaaa…");
        assert_eq!(short("sha256:abc"), "sha256:abc", "too short to truncate");
        assert_eq!(
            short("not-a-hash"),
            "not-a-hash",
            "no prefix, returned as-is"
        );
    }
}
