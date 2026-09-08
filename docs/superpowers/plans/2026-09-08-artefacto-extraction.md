# artefacto Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `artefacto` binary as a standalone static plan renderer, moving the plan module out of loadout with its full test suite green, plus the `--json`, multi-file, `--lenient`, and marker-line work the later phases depend on. artefacto emits its own marker and format strings; teaching loadout to accept them is plan 6.

**Architecture:** A single Rust binary. The plan model, validator, deterministic SVG graph, and HTML renderer move across from the loadout repo essentially unedited; only their `crate::` paths change. Two small helpers loadout owns (the hash and the markdown sanitizer) are copied rather than shared, because publishing a crate for two files is not worth it. The generated-marker line is not copied: artefacto defines its own, in its own name. Nothing in this plan starts a server or touches the browser page's behaviour.

**Tech Stack:** Rust 2021, edition floor 1.85. `clap` (derive) for the CLI, `serde` + `serde_json` for the model, `maud` for HTML, `pulldown-cmark` for markdown, `sha2` for hashing. `assert_cmd` + `predicates` for CLI tests.

**Spec:** `docs/superpowers/specs/2026-09-06-artefacto-design.md`

**Source repo:** loadout lives at `/Users/ellery/_git/rosita`. Referred to below as `$ROSITA`. Nothing in this plan modifies it. Removing loadout's copy happens in a later plan, so the two coexist until then.

## Global Constraints

- Rust edition 2021, `rust-version = "1.85"`, `[toolchain] channel = "stable"` with `rustfmt` and `clippy`.
- License MIT. Repository `https://github.com/elleryfamilia/artefacto`.
- The binary is named `artefacto`.
- **Nothing artefacto writes carries loadout's name.** The generated first line is `<!-- artefacto:generated context=<hash> -->`. Documents artefacto writes declare `artefacto.plan/1`. The word "loadout" belongs only in prose describing the integration, never in output, a format string, a default path, or a message. See spec section 11.1.
- The parser still **accepts** `loadout.plan/1` on read as a deprecated alias, so plan documents that already exist keep working. It is never written and is not documented in the skill reference.
- Making loadout accept artefacto's marker is plan 6's job, not this plan's. Do not edit the rosita repository.
- The plan hash is `sha256:` + lowercase hex of the SHA-256 of the plan's JSON serialization, unchanged from loadout.
- Golden fixtures move byte-identical **below the first line**. The first line changes, because it now carries artefacto's marker instead of loadout's. Task 6 asserts that split explicitly. Apart from that one line, never regenerate a golden to make a test pass: a mismatch means the move was wrong.
- Every task ends green on `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Commit at the end of every task using Conventional Commits.

## Plan set

This is plan 1 of 6. The spec's v1 covers several subsystems that each ship working software on their own, so it is split rather than written as one document:

1. **Extraction** (this plan) — the binary, the moved renderer, static commands.
2. **Server core** — event log, sequence and cursors, lease and session tokens, HTTP and WebSocket, security, `serve`/`stop`/`status`/`open`, and the agent verbs against a fake page client.
3. **The page** — re-entrant mount, server-side store, in-place revision swap, threads, chat, submit.
4. **The artifact index** — registry, drawn posters, `list`, the index page.
5. **Skill package and releases** — `skill --print` manifest, `skill --install`, cargo-dist.
6. **The loadout dispatcher** — `load plan` dispatches to artefacto, install offer, update, doctor, recents, batched badge.

## File structure

| file | responsibility |
|---|---|
| `Cargo.toml`, `rust-toolchain.toml` | package metadata, pinned toolchain components |
| `src/main.rs` | binary entry; parses args, calls `commands::dispatch`, maps errors to exit codes |
| `src/lib.rs` | library root; declares the public modules so integration tests can use them |
| `src/cli.rs` | clap derive types only, no logic |
| `src/hash.rs` | `context_hash`, `short` |
| `src/marker.rs` | the generated first line: build it, parse it back |
| `src/markdown.rs` | the sanitizing markdown renderer |
| `src/plan/model.rs` | schema, parse, validate, advisories, plan_hash |
| `src/plan/render.rs` | plan model to HTML |
| `src/plan/svg.rs` | deterministic dependency graph |
| `src/plan/icons.rs` | icon name validation |
| `src/plan/assets/plan.css`, `plan.js` | embedded page assets |
| `src/paths.rs` | relative path resolution, `file://` URLs, opening a browser |
| `src/commands/plan.rs` | `check`, `render`, `status`, `schema` |
| `tests/fixtures/plan/*` | moved fixtures and goldens |
| `tests/cli.rs` | CLI behaviour over the real binary |
| `tests/marker_contract.rs` | the frozen cross-repo marker fixture |
| `tests/skill_examples.rs` | every JSON example in the skill reference parses |
| `skills/artefacto-plan/` | the skill package, moved |

---

### Task 1: Cargo scaffold and the hash module

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, `src/lib.rs`, `src/hash.rs`
- Test: `src/hash.rs` (unit tests in-file, matching loadout's convention)

**Interfaces:**
- Consumes: nothing.
- Produces: `artefacto::hash::context_hash<T: Serialize>(&T) -> String` returning `"sha256:<64 hex>"`, and `artefacto::hash::short(&str) -> String` returning `"sha256:<12 hex>…"` for long hashes and the input unchanged otherwise.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "artefacto"
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
description = "Interactive artifacts between you and your coding agent."
license = "MIT"
repository = "https://github.com/elleryfamilia/artefacto"
readme = "README.md"
keywords = ["cli", "ai", "agents", "plan", "review"]
categories = ["command-line-utilities", "development-tools"]

[[bin]]
name = "artefacto"
path = "src/main.rs"

[lib]
name = "artefacto"
path = "src/lib.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_path_to_error = "0.1"
maud = "0.26"
pulldown-cmark = { version = "0.13", default-features = false, features = ["html"] }
sha2 = "0.10"
anyhow = "1"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"

[profile.release]
strip = true
lto = "thin"
```

- [ ] **Step 2: Write `rust-toolchain.toml`**

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Write the failing test in `src/hash.rs`**

Create `src/hash.rs` containing only the test module for now:

```rust
//! Content hashing for plan fingerprints.

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
            a.chars().skip("sha256:".len()).all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
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
        assert_eq!(short("not-a-hash"), "not-a-hash", "no prefix, returned as-is");
    }
}
```

- [ ] **Step 4: Write `src/lib.rs` and `src/main.rs` so the crate compiles**

`src/lib.rs`:

```rust
//! artefacto — interactive artifacts between a human and a coding agent.

pub mod hash;
```

`src/main.rs`:

```rust
fn main() {
    println!("artefacto");
}
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test --lib hash`
Expected: FAIL to compile, `cannot find function \`context_hash\` in this scope`.

- [ ] **Step 6: Implement the module**

Copy the two functions verbatim from `$ROSITA/src/hash.rs` (they are 20 lines total and already correct), placing them above the `mod tests` block in `src/hash.rs`, and add the imports the file needs:

```rust
use serde::Serialize;
use sha2::{Digest, Sha256};
```

The two functions to copy are `pub fn context_hash<T: Serialize>(value: &T) -> String` and `pub fn short(hash: &str) -> String`. Keep their doc comments. Do not change their bodies: the digest they produce is a cross-repo contract.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --lib hash`
Expected: PASS, 3 tests.

- [ ] **Step 8: Verify the gate is green**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml src/main.rs src/lib.rs src/hash.rs
git commit -m "feat: scaffold the crate and port the plan hash"
```

---

### Task 2: The generated-marker line and its frozen contract fixture

**Why this task exists:** the first line of a rendered page carries the plan's fingerprint, and tools read it back to decide whether a render is stale. artefacto emits its **own** line, `<!-- artefacto:generated … -->`; it does not adopt loadout's. A committed fixture pins the exact bytes, because a prefix-only test would let a producer and a consumer drift apart while both test suites stayed green. Plan 6 teaches loadout to accept this line; that is not this plan's job.

**Files:**
- Create: `src/marker.rs`, `tests/marker_contract.rs`, `tests/fixtures/marker/first-line.txt`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `hash::context_hash` from Task 1 (only in tests here).
- Produces: `artefacto::marker::MARKER_PREFIX: &str`, `artefacto::marker::line(hash: &str) -> String` returning the complete first line with no trailing newline, and `artefacto::marker::extract_hash(content: &str) -> Option<String>` returning the hash from the last marker line found.

- [ ] **Step 1: Write the frozen fixture**

Create `tests/fixtures/marker/first-line.txt` containing exactly one line and a trailing newline:

```
<!-- artefacto:generated context=sha256:0000000000000000000000000000000000000000000000000000000000000000 -->
```

- [ ] **Step 2: Write the failing tests in `tests/marker_contract.rs`**

```rust
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
    assert_eq!(marker::extract_hash(&marker::line(ZERO)), Some(ZERO.to_string()));
}

#[test]
fn extract_hash_reads_the_line_from_a_full_document() {
    let doc = format!("{}\n<!doctype html><html><body>x</body></html>", marker::line(ZERO));
    assert_eq!(marker::extract_hash(&doc), Some(ZERO.to_string()));
}

#[test]
fn extract_hash_returns_none_without_a_marker() {
    assert_eq!(marker::extract_hash("<!doctype html><html></html>"), None);
    assert_eq!(marker::extract_hash("<!-- artefacto:generated -->"), None, "right prefix, no context= token");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test marker_contract`
Expected: FAIL to compile, `unresolved import \`artefacto::marker\``.

- [ ] **Step 4: Implement `src/marker.rs`**

```rust
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
```

- [ ] **Step 5: Declare the module**

Add to `src/lib.rs`, keeping the list alphabetical:

```rust
pub mod hash;
pub mod marker;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test marker_contract`
Expected: PASS, 4 tests.

- [ ] **Step 7: Prove the fixture actually catches drift**

Temporarily change `MARKER_PREFIX` to `"<!-- generated"` and run `cargo test --test marker_contract`. Expected: `emitted_line_matches_the_frozen_fixture` FAILS. Revert the change and confirm the test passes again. Do not commit the temporary change.

- [ ] **Step 8: Commit**

```bash
git add src/marker.rs src/lib.rs tests/marker_contract.rs tests/fixtures/marker/first-line.txt
git commit -m "feat: add the generated-marker line with a frozen contract fixture"
```

---

### Task 3: The markdown sanitizer

**Files:**
- Create: `src/markdown.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `artefacto::markdown::render_markdown(&str) -> String` and `artefacto::markdown::strip_leading_comments(&str) -> &str`.

- [ ] **Step 1: Copy the module**

```bash
cp "$ROSITA/src/markdown.rs" src/markdown.rs
```

The file is self-contained: it imports only from `pulldown_cmark` and has no `crate::` references. Read it top to bottom to confirm that is still true before continuing. If any `crate::` path appears, stop and report it rather than guessing at a replacement.

- [ ] **Step 2: Declare the module**

In `src/lib.rs`:

```rust
pub mod hash;
pub mod markdown;
pub mod marker;
```

- [ ] **Step 3: Run the moved tests**

Run: `cargo test --lib markdown`
Expected: PASS. The file carries its own `mod tests` covering the sanitizer's threat model, including that raw HTML is neutralized and that `javascript:` link destinations are dropped.

- [ ] **Step 4: Confirm the moved tests already cover the threat model, and add nothing**

Do **not** write new safety tests here. The module arrives with tests that are stronger than anything worth adding:

- `javascript_links_are_delinked` covers seven hostile destinations — a `javascript:` URL, an upper-case variant, one with a tab injected mid-scheme, a `data:` URL, a `vbscript:` URL, and two protocol-relative URLs — and asserts for each that no `href` survives and the link text is kept.
- `raw_html_is_neutralized` asserts both that the script tag is gone and that it was escaped to `&lt;script&gt;`, which is the stronger claim.

Read both and confirm they are present and passing. Adding a narrower test beside either one is duplication that asserts less, so it is a defect, not extra safety.

Record in your report that you checked this and deliberately added nothing.

- [ ] **Step 5: Run the moved tests**

Run: `cargo test --lib markdown`
Expected: PASS, with `javascript_links_are_delinked` and `raw_html_is_neutralized` among them.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/markdown.rs src/lib.rs
git commit -m "feat: port the sanitizing markdown renderer"
```

---

### Task 4: The plan model

**Files:**
- Create: `src/plan/mod.rs`, `src/plan/model.rs`, `src/plan/icons.rs`
- Create: `tests/fixtures/plan/` (moved fixtures)
- Modify: `src/lib.rs`

**Why `icons.rs` is here and not in Task 5:** the validator calls
`crate::plan::icons::is_icon_name` and `icon_names` at three sites in `model.rs`,
including one inside its own tests. Without the module the crate does not compile, so
Task 4 cannot reach a green gate on its own. `icons.rs` is 81 lines with no imports and
no crate references, so it costs nothing to bring along.

**Interfaces:**
- Consumes: `hash::context_hash` from Task 1.
- Produces: `artefacto::plan::model::{Plan, Issue, Parsed}`, `parse(input: &str, lenient: bool) -> Result<Parsed, Vec<Issue>>`, `validate(&Plan) -> Vec<Issue>`, `advisories(&Plan) -> Vec<Issue>`, `plan_hash(&Plan) -> String`. `Issue` has public fields `path: String`, `code: String`, `message: String`, `hint: Option<String>`. `Parsed` has public fields `plan: Plan`, `warnings: Vec<Issue>`.

- [ ] **Step 1: Copy the model and the fixtures**

```bash
mkdir -p src/plan tests/fixtures/plan
cp "$ROSITA/src/plan/model.rs" src/plan/model.rs
cp "$ROSITA/src/plan/icons.rs" src/plan/icons.rs
cp "$ROSITA/tests/fixtures/plan/"*.json tests/fixtures/plan/
```

The JSON fixtures are `hostile.json`, `invalid-cycle.json`, `invalid-dangling-ref.json`, `invalid-dup-id.json`, `kitchen-sink.json`, `learning-v0-15.json`, and `minimal.json`. Copy the `.html` and `.svg` goldens in Task 6 and Task 5 respectively, not here.

- [ ] **Step 2: Create `src/plan/mod.rs`**

```rust
//! The `artefacto.plan/1` artifact kind: schema, validation, and rendering.

pub mod icons;
pub mod model;
```

- [ ] **Step 3: Rewrite the crate paths**

`model.rs` references loadout's hash helper. Change the one call site:

```bash
rg -n "crate::hash" src/plan/model.rs
```

Expected: one hit, `crate::hash::context_hash(plan)` inside `pub fn plan_hash`. It needs no edit, because artefacto's `hash` module sits at the same crate path. Run the search and confirm there are no other `crate::` references outside `crate::plan` and `crate::hash`. If there are, stop and report them.

- [ ] **Step 4: Declare the module**

In `src/lib.rs`:

```rust
pub mod hash;
pub mod markdown;
pub mod marker;
pub mod plan;
```

- [ ] **Step 5: Run the moved tests**

Run: `cargo test --lib plan::model`
Expected: PASS. `model.rs` carries its own unit tests for parse, validate, the id rules, the cycle detector, the size limits, and the advisories.

- [ ] **Step 5a: Rename the format string and keep the old one readable**

The model gates on a format constant before anything else. Change the constant and widen the gate so existing documents still parse.

In `src/plan/model.rs`, replace the constant:

```rust
/// The format string every document artefacto writes declares.
pub const FORMAT: &str = "artefacto.plan/1";

/// Accepted on read only, so plan documents written before the rename keep
/// working. Never written, and deliberately absent from the skill reference.
pub const LEGACY_FORMAT: &str = "loadout.plan/1";
```

Then widen the match arm inside `parse` that gates the format, keeping the
"too new" arm for artefacto's own namespace:

```rust
    match value.get("format").and_then(|f| f.as_str()) {
        Some(f) if f == FORMAT => {}
        Some(f) if f == LEGACY_FORMAT => {
            eprintln!(
                "note: \"format\": \"{LEGACY_FORMAT}\" is deprecated; write \"{FORMAT}\" instead"
            );
        }
        Some(f) if f.starts_with("artefacto.plan/") => {
            return Err(vec![Issue::new("/format", "format_too_new",
                format!("plan format {f} is newer than this artefacto understands ({FORMAT})"))]);
        }
        _ => {
            return Err(vec![Issue::new(
                "/format",
                "bad_format",
                format!("expected \"format\": \"{FORMAT}\""),
            )])
        }
    }
```

**Two other places in this file name loadout, and both break if you miss them.**

First, the "format is newer than I understand" error message. It reads `newer than this loadout understands ({FORMAT}) — run \`load update\``, which is user-facing output naming another product and telling the reader to run its command. The replacement gate above already fixes the wording; make sure you took it verbatim.

Second, the test `newer_format_gets_clear_error` builds its input like this:

```rust
        let newer = fixture("minimal.json").replace("loadout.plan/1", "loadout.plan/2");
```

Once the fixtures are migrated, that `replace` matches nothing, the document stays valid, and the test fails asserting `format_too_new` on a document that parsed fine. It also asserts the message contains `load update`, which the new message does not say. Rewrite it:

```rust
    #[test]
    fn newer_format_gets_clear_error() {
        let newer = fixture("minimal.json").replace(FORMAT, "artefacto.plan/2");
        let errs = parse(&newer, false).unwrap_err();
        assert_eq!(errs[0].code, "format_too_new");
        assert!(
            errs[0].message.contains("artefacto.plan/2"),
            "the error names the version it could not read: {}",
            errs[0].message
        );
    }
```

When you are done, confirm the file is clean:

```bash
rg -ni loadout src/plan/model.rs
```

Expected: exactly one line, the `LEGACY_FORMAT` constant. Anything else is a miss.

Update the JSON fixtures to the new string, except one kept as the alias test:

```bash
cd tests/fixtures/plan
sed -i '' 's|"loadout.plan/1"|"artefacto.plan/1"|' *.json
cp minimal.json legacy-format.json
sed -i '' 's|"artefacto.plan/1"|"loadout.plan/1"|' legacy-format.json
```

On Linux, `sed -i` takes no argument: drop the `''`.

- [ ] **Step 5b: Test both the new name and the alias**

Add to `mod tests` in `src/plan/model.rs`:

```rust
    #[test]
    fn documents_declare_artefactos_own_format() {
        assert_eq!(FORMAT, "artefacto.plan/1");
        let parsed = parse(&fixture("minimal.json"), false).expect("new format parses");
        assert_eq!(parsed.plan.format, FORMAT, "fixtures were migrated to the new name");
    }

    #[test]
    fn the_legacy_format_still_parses() {
        parse(&fixture("legacy-format.json"), false)
            .expect("documents written before the rename must keep working");
    }

    #[test]
    fn an_unrelated_format_is_rejected() {
        let err = parse(r#"{"format":"something/1","meta":{"id":"a","title":"A"}}"#, false)
            .expect_err("unknown format is an error");
        assert_eq!(err[0].code, "bad_format");
    }
```

`Plan` has a public `format: String` field. It is deserialized from the document, serialized back into the rendered page's embedded plan data, and covered by the plan hash. So a legacy document would otherwise carry the old string all the way into artefacto's output, which breaks this plan's global constraint in exactly the case the alias exists to serve.

**Normalize it.** After the format gate accepts a document, overwrite the field with the canonical value before returning. `parse` ends with a deserialize followed immediately by the `Ok(Parsed { … })`. Make the binding mutable and set the field between them:

```rust
    let mut plan: Plan = serde_path_to_error::deserialize(de).map_err(|e| {
        vec![Issue::new(
            e.path().to_string(),
            "invalid_shape",
            e.inner().to_string(),
        )]
    })?;
    // The alias is accepted on read, never propagated. Normalizing here means
    // output never carries the old name, and one plan hashes the same however
    // its source file spelled the format.
    plan.format = FORMAT.to_string();
    Ok(Parsed {
        plan,
        warnings: unknown,
    })
```

That is the whole change: `let plan` becomes `let mut plan`, and one assignment is added before the existing `Ok`.

Add the test that pins it:

```rust
    #[test]
    fn a_legacy_document_is_normalized_to_the_current_format() {
        let parsed = parse(&fixture("legacy-format.json"), false).expect("alias parses");
        assert_eq!(
            parsed.plan.format, FORMAT,
            "the deprecated name must never survive into the model, the render, or the hash"
        );
    }

    #[test]
    fn format_spelling_does_not_change_the_hash() {
        let new = parse(&fixture("minimal.json"), false).unwrap().plan;
        let old = parse(&fixture("legacy-format.json"), false).unwrap().plan;
        assert_eq!(
            plan_hash(&new),
            plan_hash(&old),
            "legacy-format.json is minimal.json with the old format string; they are one plan"
        );
    }
```

- [ ] **Step 6: Add a fixture-backed test proving the fixtures are reachable and correct**

Create the test at the bottom of `src/plan/model.rs`, inside its existing `mod tests`:

```rust
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/plan/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
    }

    #[test]
    fn kitchen_sink_fixture_parses_and_validates_clean() {
        let parsed = parse(&fixture("kitchen-sink.json"), false).expect("kitchen sink parses");
        assert!(
            validate(&parsed.plan).is_empty(),
            "kitchen sink must validate clean: {:?}",
            validate(&parsed.plan)
        );
    }

    #[test]
    fn invalid_fixtures_are_rejected() {
        for name in ["invalid-cycle.json", "invalid-dangling-ref.json", "invalid-dup-id.json"] {
            let issues = match parse(&fixture(name), false) {
                Err(errs) => errs,
                Ok(p) => validate(&p.plan),
            };
            assert!(!issues.is_empty(), "{name} should have produced issues");
        }
    }

    #[test]
    fn plan_hash_is_stable_for_the_same_plan() {
        let a = parse(&fixture("minimal.json"), false).unwrap().plan;
        let b = parse(&fixture("minimal.json"), false).unwrap().plan;
        assert_eq!(plan_hash(&a), plan_hash(&b));
        assert!(plan_hash(&a).starts_with("sha256:"));
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --lib plan::model`
Expected: PASS, including the three new tests.

- [ ] **Step 8: Commit**

```bash
git add src/plan/ tests/fixtures/plan/ src/lib.rs
git commit -m "feat: port the plan model under artefacto's own format string"
```

---

### Task 5: Icons and the deterministic dependency graph

**Files:**
- Create: `src/plan/svg.rs`, `tests/fixtures/plan/kitchen-sink-p-core.svg`
- Modify: `src/plan/mod.rs`

**Note:** `icons.rs` moved to Task 4, because `model.rs` calls it and the crate does not
compile without it. It is already present when this task starts.

**Interfaces:**
- Consumes: `plan::model::Plan` from Task 4.
- Produces: `artefacto::plan::icons::is_icon_name(&str) -> bool`, `artefacto::plan::svg::phase_svg(plan: &Plan, phase_id: &str) -> Option<String>` (one phase's graph), and `artefacto::plan::svg::phase_graph_svg(plan: &Plan) -> Option<String>` (the whole-plan graph). Task 6's renderer calls both.

- [ ] **Step 1: Copy the modules and the golden**

```bash
cp "$ROSITA/src/plan/svg.rs" src/plan/svg.rs
cp "$ROSITA/tests/fixtures/plan/kitchen-sink-p-core.svg" tests/fixtures/plan/
```

- [ ] **Step 2: Declare the modules**

`src/plan/mod.rs`:

```rust
//! The `artefacto.plan/1` artifact kind: schema, validation, and rendering.

pub mod icons;
pub mod model;
pub mod svg;
```

`icons` is already declared by Task 4; you are adding `svg`.

- [ ] **Step 3: Check for crate paths that need rewriting**

```bash
rg -n "crate::" src/plan/icons.rs src/plan/svg.rs
```

Every hit should be `crate::plan::…` or `crate::hash::…`, which resolve unchanged. Anything else means the module depends on something that did not move; stop and report it.

- [ ] **Step 4: Run the moved tests**

Run: `cargo test --lib plan::svg` (`cargo test` takes one name filter, not two — run a second invocation if you also want `plan::icons`)
Expected: PASS, including the golden comparison against `kitchen-sink-p-core.svg`.

- [ ] **Step 5: Prove the golden is load-bearing**

Temporarily change one literal in `svg.rs` that affects output, such as a node's corner radius, and run `cargo test --lib plan::svg`. Expected: the golden test FAILS. Revert and confirm it passes. Do not commit the temporary change.

- [ ] **Step 6: Confirm the determinism coverage already exists, and add nothing**

Do **not** write a determinism test here. The module arrives with
`phase_svg_is_deterministic_and_links_tasks`, which already makes that assertion, and
`golden_phase_svg`, which pins the output against the committed fixture. A third test
covering the same ground asserts less than either.

Read both, confirm they are present and passing, and record in your report that you
checked and deliberately added nothing. The two entry points are
`phase_svg(&Plan, &str) -> Option<String>` and `phase_graph_svg(&Plan) -> Option<String>`;
the test module has a `kitchen()` helper that loads and parses the kitchen-sink fixture,
so use that rather than re-reading the file if you ever do need a plan in a test here.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --lib plan`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/plan/icons.rs src/plan/svg.rs src/plan/mod.rs tests/fixtures/plan/kitchen-sink-p-core.svg
git commit -m "feat: port the icon names and the deterministic dependency graph"
```

---

### Task 6: The HTML renderer and the page assets

**Files:**
- Create: `src/plan/render.rs`, `src/plan/assets/plan.css`, `src/plan/assets/plan.js`, `tests/fixtures/plan/kitchen-sink.html`, `tools/build-plan-fonts.py`
- Modify: `src/plan/mod.rs`

**Interfaces:**
- Consumes: `plan::model::Plan`, `markdown::render_markdown`, `marker::line`, `hash` from earlier tasks.
- Produces: `artefacto::plan::render::render(&Plan) -> String`, returning the complete document with the marker line first.

- [ ] **Step 1: Copy the renderer, assets, golden and font tool**

```bash
mkdir -p src/plan/assets tools
cp "$ROSITA/src/plan/render.rs" src/plan/render.rs
cp "$ROSITA/src/plan/assets/plan.css" src/plan/assets/plan.css
cp "$ROSITA/src/plan/assets/plan.js" src/plan/assets/plan.js
cp "$ROSITA/tests/fixtures/plan/kitchen-sink.html" tests/fixtures/plan/
cp "$ROSITA/tools/build-plan-fonts.py" tools/build-plan-fonts.py
```

- [ ] **Step 1a: Rename every loadout-branded string in the moved files**

The moved assets are not neutral: the page **displays "Loadout" as its brand name**, and the script carries loadout-named storage keys and test identifiers. Shipping them as-is would put loadout's name on every page artefacto renders, which the global constraints forbid. Apply this map exactly, and nothing else:

| file | from | to |
|---|---|---|
| `src/plan/render.rs` | `title { (plan.meta.title) " — loadout plan" }` | the same with `" — artefacto plan"` — this is the **browser tab title**, the most visible place the name appears |
| `src/plan/render.rs` | `span.pv-brand-name { "Loadout" }` | `span.pv-brand-name { "artefacto" }` |
| `src/plan/render.rs` | `assert!(html.contains(">Loadout</span>")` | `assert!(html.contains(">artefacto</span>")` |
| `src/plan/render.rs` | the comment naming Loadout as the product | rewrite to name artefacto |
| `src/plan/assets/plan.js` | `"loadout.plan-feedback/1"` | `"artefacto.feedback/1"` |
| `src/plan/assets/plan.js` | `window.loadoutPlan` | `window.artefactoPlan` |
| `src/plan/assets/plan.js` | `"loadout-plan:theme"` | `"artefacto-plan:theme"` |
| `src/plan/assets/plan.js` | `"loadout-plan:"` (draft key prefix) | `"artefacto-plan:"` |
| `src/plan/assets/plan.js` | `"loadout-plan-reviewed:"` | `"artefacto-plan-reviewed:"` |
| `src/plan/assets/plan.js` | `"loadout-selftest"` | `"artefacto-selftest"` |
| `src/plan/assets/plan.js` | `LOADOUT_SELFTEST_RELAY`, `LOADOUT_SELFTEST_PASS`, `LOADOUT_SELFTEST_FAIL` | the same names with `ARTEFACTO_` |
| `src/plan/assets/plan.js`, `src/plan/assets/plan.css` | the leading `loadout plan viewer` comment | `artefacto plan viewer` |

Renaming the two `localStorage` key prefixes silently discards any draft or reviewed-mark a reader had stored under the old key. That is correct here: this is a new tool with no existing users, and the keys are namespaced per plan hash anyway.

Verify nothing was missed:

```bash
rg -ci loadout src/plan/render.rs src/plan/assets/plan.js src/plan/assets/plan.css
```

Expected: `0` for all three. Do not rename anything in `src/plan/model.rs` here — Task 4 already handled its format strings.

- [ ] **Step 2: Rewrite the one crate path that moved**

`render.rs` builds the first line from loadout's header module. Find it:

```bash
rg -n "crate::render::header::GENERATED_MARKER" src/plan/render.rs
```

Expected: one hit, inside a `format!` that produces `"{} context={hash} -->\n{}"`. Replace that whole `format!` with a call to the marker module from Task 2, so there is exactly one place that knows the line's shape:

```rust
    format!("{}\n{}", crate::marker::line(&hash), page.into_string())
```

Then confirm no other `crate::render::` reference remains:

```bash
rg -n "crate::render::" src/plan/render.rs
```

Expected: no output.

- [ ] **Step 3: Declare the module**

`src/plan/mod.rs`:

```rust
//! The `artefacto.plan/1` artifact kind: schema, validation, and rendering.

pub mod icons;
pub mod model;
pub mod render;
pub mod svg;
```

- [ ] **Step 4: Build the expected golden by applying the same renames**

Do not hand-edit the golden, and do not regenerate it blind. Instead derive what it *should* become by applying the Step 1a rename map to loadout's copy. Anything that then still differs is a real change, and there should be exactly one kind of it: the plan hash, which moved because Task 4 renamed the format string that the hash covers.

```bash
python3 - <<'PY'
import pathlib
p = pathlib.Path("tests/fixtures/plan/kitchen-sink.html")
t = p.read_text()
for a, b in [
    ("<!-- loadout:generated", "<!-- artefacto:generated"),
    (" — loadout plan</title>", " — artefacto plan</title>"),
    ("loadout.plan-feedback/1", "artefacto.feedback/1"),
    ("loadout.plan/1", "artefacto.plan/1"),
    ("loadoutPlan", "artefactoPlan"),
    ("loadout-plan-reviewed:", "artefacto-plan-reviewed:"),
    ("loadout-plan:", "artefacto-plan:"),
    ("loadout-selftest", "artefacto-selftest"),
    ("LOADOUT_SELFTEST", "ARTEFACTO_SELFTEST"),
    (">Loadout</span>", ">artefacto</span>"),
    ("loadout plan viewer", "artefacto plan viewer"),
]:
    t = t.replace(a, b)
p.write_text(t)
leftover = [l for l in t.splitlines() if "loadout" in l.lower()]
print("remaining loadout lines:", len(leftover))
for l in leftover[:5]:
    print("  ", l[:120])
PY
```

Expected: `remaining loadout lines: 0`. This map was run against loadout's actual golden before the plan was written and does reach zero, so a non-zero count means something changed underneath it. If any line is printed, add the missing case to Step 1a as well, so the code and the fixture stay in step.

- [ ] **Step 5: Run the moved tests and confirm only the hash differs**

Run: `cargo test --lib plan::render`
Expected: `render_is_deterministic_and_matches_golden` FAILS, and **every** differing line is a plan-hash occurrence. There are two: the `context=` value in the first line, and the `data-plan-fingerprint` attribute on the body.

Read the failure output and confirm that. If any structural markup differs — an element, an attribute other than the fingerprint, a class, any text a reader would see — the move is wrong. Stop and report it rather than regenerating.

- [ ] **Step 6: Regenerate the golden with the project's own switch, then audit the diff**

The golden test carries an escape hatch: setting `UPDATE_GOLDEN` rewrites the fixture
instead of asserting against it. Use it rather than editing the file by hand.

The test that owns the golden is `render_is_deterministic_and_matches_golden`. It also
asserts that rendering twice gives identical output, so you do not need to add a
determinism test here — it already exists.

```bash
UPDATE_GOLDEN=1 cargo test --lib plan::render::tests::render_is_deterministic_and_matches_golden
git diff --stat tests/fixtures/plan/kitchen-sink.html
git diff tests/fixtures/plan/kitchen-sink.html
```

Then **audit that diff line by line**. Every changed line must be a plan-hash
occurrence: the `context=` value in the first line, and the `data-plan-fingerprint`
attribute on the body. That is two lines.

If the diff shows anything else — an element, a class, an attribute, any text a reader
would see — the rename map in Step 1a and Step 4 disagree with each other, or the move
changed behaviour. Revert the fixture (`git checkout -- tests/fixtures/plan/kitchen-sink.html`),
fix the real cause, and start this step again. Do not accept a wider diff.

Record the audited diff in your completion report.

- [ ] **Step 7: Add a test proving no rendered page carries loadout's name**

Append to `mod tests` in `src/plan/render.rs`:

```rust
    #[test]
    fn a_rendered_page_never_mentions_loadout() {
        // artefacto is a separate project. Someone using it will not have
        // loadout installed and must never see its name on the page, in a
        // storage key, or in an embedded format string.
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/plan/kitchen-sink.json"
        ))
        .unwrap();
        let plan = crate::plan::model::parse(&raw, false).unwrap().plan;
        let html = render(&plan).to_lowercase();
        assert!(!html.contains("loadout"), "the rendered page mentions loadout");
    }
```

- [ ] **Step 8: Add a test tying the render to the marker contract**

Append to `mod tests` in `src/plan/render.rs`:

```rust
    #[test]
    fn rendered_document_starts_with_the_contract_marker_line() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/plan/kitchen-sink.json"
        ))
        .unwrap();
        let plan = crate::plan::model::parse(&raw, false).unwrap().plan;
        let html = render(&plan);
        let expected = crate::plan::model::plan_hash(&plan);

        let first = html.lines().next().expect("document has a first line");
        assert_eq!(first, crate::marker::line(&expected), "first line is the contract line");
        assert_eq!(
            crate::marker::extract_hash(&html),
            Some(expected),
            "loadout parses this line back to decide whether a render is fresh"
        );
    }
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test --lib plan::render`
Expected: PASS.

- [ ] **Step 10: Verify the whole gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 11: Commit**

```bash
git add src/plan/render.rs src/plan/assets/ src/plan/mod.rs tests/fixtures/plan/kitchen-sink.html tools/build-plan-fonts.py
git commit -m "feat: port the plan renderer and page assets under artefacto's own name"
```

---

### Task 7: CLI scaffold and `artefacto plan check`

**Files:**
- Create: `src/cli.rs`, `src/commands/mod.rs`, `src/commands/plan.rs`, `tests/cli.rs`
- Modify: `src/main.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: `plan::model::{parse, validate, advisories, plan_hash}`, `hash::short`.
- Produces: the `artefacto plan check` command, and `commands::plan::run(&PlanArgs) -> anyhow::Result<()>` for Tasks 8 and 9 to extend. Exit codes: 0 clean, 1 validation errors, 2 usage or IO error.

**Spec reference:** section 5. `check` takes one or more files, `--json`, and `--lenient`. The multi-file and lenient modes exist because loadout's studio needs one batched call for up to 30 Recents rows, and parses leniently.

- [ ] **Step 1: Write the failing CLI test**

Create `tests/cli.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;

fn bin() -> Command {
    Command::cargo_bin("artefacto").expect("binary builds")
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/plan/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn check_reports_a_valid_plan() {
    bin()
        .args(["plan", "check", &fixture("minimal.json")])
        .assert()
        .success()
        .stdout(contains("valid"));
}

#[test]
fn check_fails_on_an_invalid_plan() {
    bin()
        .args(["plan", "check", &fixture("invalid-dup-id.json")])
        .assert()
        .code(1);
}

#[test]
fn check_json_emits_one_object_per_file() {
    let out = bin()
        .args(["plan", "check", "--json", &fixture("minimal.json"), &fixture("kitchen-sink.json")])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).expect("stdout is one JSON document");
    let files = doc["files"].as_array().expect("files array");
    assert_eq!(files.len(), 2, "one entry per input file");
    for f in files {
        assert_eq!(f["ok"], true);
        assert!(f["plan_hash"].as_str().unwrap().starts_with("sha256:"));
        assert!(f["title"].is_string(), "title is needed by the studio badge");
        assert!(f["phases"].is_number());
        assert!(f["tasks"].is_number());
        assert!(f["path"].is_string());
    }
}

#[test]
fn check_json_reports_a_bad_file_without_failing_the_good_one() {
    let out = bin()
        .args(["plan", "check", "--json", &fixture("minimal.json"), &fixture("invalid-dup-id.json")])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let files = doc["files"].as_array().unwrap();
    assert_eq!(files[0]["ok"], true);
    assert_eq!(files[1]["ok"], false);
    assert!(!files[1]["errors"].as_array().unwrap().is_empty());
}

#[test]
fn check_lenient_downgrades_unknown_fields_to_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("extra.json");
    let base = std::fs::read_to_string(fixture("minimal.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&base).unwrap();
    v["meta"]["not_a_real_field"] = serde_json::json!("x");
    std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();

    bin().args(["plan", "check", p.to_str().unwrap()]).assert().code(1);
    bin().args(["plan", "check", "--lenient", p.to_str().unwrap()]).assert().success();
}

#[test]
fn check_reports_a_missing_file_as_a_usage_error() {
    bin()
        .args(["plan", "check", "/nonexistent/plan.json"])
        .assert()
        .code(2)
        .stderr(contains("/nonexistent/plan.json"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test cli`
Expected: FAIL. The binary prints `artefacto` and ignores its arguments, so every assertion fails.

- [ ] **Step 3: Write `src/cli.rs`**

```rust
//! Command-line surface. Types only — no behaviour lives here.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "artefacto", version, about = "Interactive artifacts between you and your coding agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Work with plan artifacts.
    Plan(PlanArgs),
}

#[derive(Args, Debug)]
pub struct PlanArgs {
    #[command(subcommand)]
    pub action: PlanAction,
}

#[derive(Subcommand, Debug)]
pub enum PlanAction {
    /// Validate one or more plan files.
    Check {
        /// Plan files to validate.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Prune unknown fields and report them as warnings instead of errors.
        #[arg(long)]
        lenient: bool,
    },
}
```

- [ ] **Step 4: Write `src/commands/mod.rs`**

```rust
//! Command implementations.

pub mod plan;

use crate::cli::{Cli, Command};

/// Route a parsed command line to its implementation.
pub fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan(args) => plan::run(args),
    }
}
```

- [ ] **Step 5: Write `src/commands/plan.rs`**

```rust
//! `artefacto plan` — validate, render and inspect plan artifacts.

use crate::cli::{PlanAction, PlanArgs};
use crate::plan::model;
use anyhow::{Context as _, Result};
use std::path::Path;

/// A validation error the caller should see as exit code 1.
#[derive(Debug)]
pub struct PlanInvalid;

impl std::fmt::Display for PlanInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plan validation failed")
    }
}

impl std::error::Error for PlanInvalid {}

pub fn run(args: &PlanArgs) -> Result<()> {
    match &args.action {
        PlanAction::Check { files, json, lenient } => check(files, *json, *lenient),
    }
}

/// One file's verdict, shared by `check` and (later) `render` and `status`.
struct Checked {
    plan: model::Plan,
    warnings: Vec<model::Issue>,
}

/// Read and validate one file. Returns the plan plus warnings, or the errors.
fn check_one(path: &Path, lenient: bool) -> Result<std::result::Result<Checked, Vec<model::Issue>>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let parsed = match model::parse(&raw, lenient) {
        Ok(p) => p,
        Err(errors) => return Ok(Err(errors)),
    };
    let errors = model::validate(&parsed.plan);
    if !errors.is_empty() {
        return Ok(Err(errors));
    }
    let mut warnings = parsed.warnings;
    warnings.extend(model::advisories(&parsed.plan));
    Ok(Ok(Checked { plan: parsed.plan, warnings }))
}

fn task_count(plan: &model::Plan) -> usize {
    plan.phases.iter().map(|p| p.tasks.len()).sum()
}

fn check(files: &[std::path::PathBuf], json: bool, lenient: bool) -> Result<()> {
    let mut entries = Vec::with_capacity(files.len());
    let mut any_bad = false;

    for path in files {
        let outcome = check_one(path, lenient)?;
        match outcome {
            Ok(ok) => {
                entries.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "ok": true,
                    "plan_hash": model::plan_hash(&ok.plan),
                    "title": ok.plan.meta.title,
                    "phases": ok.plan.phases.len(),
                    "tasks": task_count(&ok.plan),
                    "errors": [],
                    "warnings": ok.warnings,
                }));
                if !json {
                    println!(
                        "{}: valid ({} phases, {} tasks, {})",
                        path.display(),
                        ok.plan.phases.len(),
                        task_count(&ok.plan),
                        crate::hash::short(&model::plan_hash(&ok.plan))
                    );
                    for w in &ok.warnings {
                        println!("  warning[{}] {}: {}", w.code, w.path, w.message);
                    }
                }
            }
            Err(errors) => {
                any_bad = true;
                entries.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "ok": false,
                    "errors": errors,
                    "warnings": [],
                }));
                if !json {
                    println!("{}: INVALID", path.display());
                    for e in &errors {
                        println!("  error[{}] {}: {}", e.code, e.path, e.message);
                    }
                }
            }
        }
    }

    if json {
        let doc = serde_json::json!({ "ok": !any_bad, "files": entries });
        println!("{}", serde_json::to_string(&doc)?);
    }

    if any_bad {
        return Err(PlanInvalid.into());
    }
    Ok(())
}
```

If `plan.meta.title` is not the field's real name, run `rg -n "pub struct Meta" -A 12 src/plan/model.rs` and use the actual field. Do not invent one.

- [ ] **Step 6: Wire `src/main.rs` and `src/lib.rs`**

`src/lib.rs`:

```rust
//! artefacto — interactive artifacts between a human and a coding agent.

pub mod cli;
pub mod commands;
pub mod hash;
pub mod markdown;
pub mod marker;
pub mod plan;
```

`src/main.rs`:

```rust
use artefacto::cli::Cli;
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = artefacto::commands::dispatch(&cli) {
        // A plan that failed validation already printed its errors; anything
        // else is a usage or IO problem and belongs on stderr.
        if err.downcast_ref::<artefacto::commands::plan::PlanInvalid>().is_some() {
            std::process::exit(1);
        }
        eprintln!("error: {err:#}");
        std::process::exit(2);
    }
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --test cli`
Expected: PASS, 6 tests.

- [ ] **Step 8: Verify the gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 9: Commit**

```bash
git add src/cli.rs src/commands/ src/main.rs src/lib.rs tests/cli.rs
git commit -m "feat: add the CLI and multi-file plan check"
```

---

### Task 8: `artefacto plan render`

**Files:**
- Create: `src/paths.rs`
- Modify: `src/cli.rs`, `src/commands/plan.rs`, `src/lib.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `check_one` and `Checked` from Task 7, `plan::render::render`.
- Produces: `artefacto::paths::{resolve_relative, file_url, open_browser}`, and the `render` action.

**Spec reference:** section 5. Relative paths anchor to the invocation directory, not the repo root, which is the behaviour loadout settled on after a bug where they anchored to the process working directory.

- [ ] **Step 1: Write the failing tests**

Append to `tests/cli.rs`:

```rust
#[test]
fn render_writes_a_document_starting_with_the_marker_line() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args(["plan", "render", &fixture("minimal.json"), "--out", out.to_str().unwrap(), "--no-open"])
        .assert()
        .success();
    let html = std::fs::read_to_string(&out).expect("render wrote the file");
    assert!(html.starts_with("<!-- artefacto:generated context=sha256:"), "first line: {:?}", html.lines().next());
    assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));
}

#[test]
fn render_json_reports_what_the_dispatcher_records() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    let stdout = bin()
        .args(["plan", "render", &fixture("kitchen-sink.json"), "--out", out.to_str().unwrap(), "--no-open", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(doc["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(doc["title"].is_string());
    assert!(doc["phases"].is_number());
    assert!(doc["tasks"].is_number());
    assert_eq!(doc["out"], out.display().to_string());
}

#[test]
fn render_refuses_an_invalid_plan_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args(["plan", "render", &fixture("invalid-cycle.json"), "--out", out.to_str().unwrap(), "--no-open"])
        .assert()
        .code(1);
    assert!(!out.exists(), "a rejected plan must not leave a partial file");
}

#[test]
fn render_resolves_a_relative_out_against_the_invocation_directory() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .current_dir(dir.path())
        .args(["plan", "render", &fixture("minimal.json"), "--out", "nested/plan.html", "--no-open"])
        .assert()
        .success();
    assert!(dir.path().join("nested/plan.html").exists(), "relative --out anchors to cwd");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test cli render`
Expected: FAIL, clap reports `unrecognized subcommand 'render'`.

- [ ] **Step 3: Write `src/paths.rs`**

```rust
//! Filesystem and browser helpers.

use std::path::{Path, PathBuf};

/// Resolve `path` against `cwd` when it is relative. Absolute paths pass
/// through. Anchoring to the invocation directory (not the repository root)
/// is what a user typing a relative path expects.
pub fn resolve_relative(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// A `file://` URL for `path`, percent-encoding the characters that break
/// browsers. Spaces are the common case; `#` and `?` would truncate the URL.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Open `url` in the user's browser, best effort. A failure is never fatal:
/// the caller has already printed the path.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_anchor_to_cwd() {
        let got = resolve_relative(Path::new("/work"), Path::new("out/plan.html"));
        assert_eq!(got, PathBuf::from("/work/out/plan.html"));
    }

    #[test]
    fn absolute_paths_pass_through() {
        let got = resolve_relative(Path::new("/work"), Path::new("/tmp/plan.html"));
        assert_eq!(got, PathBuf::from("/tmp/plan.html"));
    }

    #[test]
    fn file_url_escapes_spaces_and_fragments() {
        assert_eq!(file_url(Path::new("/a b/c.html")), "file:///a%20b/c.html");
        assert_eq!(file_url(Path::new("/a#b.html")), "file:///a%23b.html");
    }
}
```

- [ ] **Step 4: Add the CLI variant**

In `src/cli.rs`, add to `enum PlanAction`:

```rust
    /// Render a plan to a self-contained HTML file.
    Render {
        /// The plan file to render.
        file: PathBuf,
        /// Where to write the HTML. Relative paths anchor to the current directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Do not open the rendered file in a browser.
        #[arg(long)]
        no_open: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
```

- [ ] **Step 5: Implement the action**

In `src/commands/plan.rs`, add to the `match` in `run`:

```rust
        PlanAction::Render { file, out, no_open, json } => {
            render(file, out.as_deref(), *no_open, *json)
        }
```

and add the function:

```rust
fn render(
    file: &Path,
    out: Option<&Path>,
    no_open: bool,
    json: bool,
) -> Result<()> {
    let checked = match check_one(file, false)? {
        Ok(ok) => ok,
        Err(errors) => {
            for e in &errors {
                eprintln!("error[{}] {}: {}", e.code, e.path, e.message);
            }
            return Err(PlanInvalid.into());
        }
    };

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let target = match out {
        Some(p) => crate::paths::resolve_relative(&cwd, p),
        None => crate::paths::resolve_relative(&cwd, Path::new("plan.html")),
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }

    let html = crate::plan::render::render(&checked.plan);
    std::fs::write(&target, &html)
        .with_context(|| format!("could not write {}", target.display()))?;

    if json {
        let doc = serde_json::json!({
            "ok": true,
            "path": file.display().to_string(),
            "out": target.display().to_string(),
            "plan_hash": model::plan_hash(&checked.plan),
            "title": checked.plan.meta.title,
            "phases": checked.plan.phases.len(),
            "tasks": task_count(&checked.plan),
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        println!("rendered {}", target.display());
        for w in &checked.warnings {
            println!("  warning[{}] {}: {}", w.code, w.path, w.message);
        }
    }

    if !no_open {
        crate::paths::open_browser(&crate::paths::file_url(&target));
    }
    Ok(())
}
```

- [ ] **Step 6: Declare the module**

Add `pub mod paths;` to `src/lib.rs`, keeping the list alphabetical.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --test cli`
Expected: PASS, 10 tests.

- [ ] **Step 8: Verify the gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 9: Commit**

```bash
git add src/paths.rs src/cli.rs src/commands/plan.rs src/lib.rs tests/cli.rs
git commit -m "feat: add plan render with json output and path helpers"
```

---

### Task 9: `artefacto plan status`, `schema`, and the skill package

**Files:**
- Create: `skills/artefacto-plan/SKILL.md`, `skills/artefacto-plan/reference.md`
- Modify: `src/cli.rs`, `src/commands/plan.rs`, `tests/cli.rs`

**Interfaces:**
- Consumes: `marker::extract_hash`, `check_one`, `model::plan_hash`.
- Produces: the `status` and `schema` actions, and the skill package on disk. `status` exits 0 when fresh, 1 when stale or missing, so a script can branch on it.

**Spec reference:** sections 5 and 10. loadout's `load plan status` is a reverse dependency the dispatcher forwards, and it works by comparing the hash inside the rendered HTML to the plan's hash.

**Why the skill files land here:** `schema` embeds `reference.md` with `include_str!`, which is resolved at compile time. If the file arrives in a later task, this task's crate does not build at all.

- [ ] **Step 1: Copy the skill package**

```bash
mkdir -p skills/artefacto-plan
cp "$ROSITA/skills/loadout-plan-preview/SKILL.md" skills/artefacto-plan/SKILL.md
cp "$ROSITA/skills/loadout-plan-preview/reference.md" skills/artefacto-plan/reference.md
```

- [ ] **Step 2: Remove every trace of loadout from the skill text**

Someone using artefacto will not have loadout and will never have heard of it, so its name must not appear in either file. Rewrite:

- `name:` in the SKILL.md front matter becomes `artefacto-plan`.
- `load plan` becomes `artefacto plan` everywhere, including `check --json` and `schema`.
- `loadout.plan/1` becomes `artefacto.plan/1`. Do **not** document the deprecated alias; new plans use the new name.
- `loadout.plan-feedback/1` becomes `artefacto.feedback/1`.
- The paths `.loadout/workflow/artifacts/plan.json` and `plan-feedback.json` become plain `plan.json` and `plan-feedback.json`, described as "wherever the plan file lives". loadout's dispatcher supplies its own paths in plan 6; the skill must not assume them.
- Any sentence describing loadout as the thing that renders the page now describes artefacto.

Verify nothing is left:

```bash
rg -ni "loadout" skills/artefacto-plan/
```

Expected: no output. If a hit remains, rewrite it rather than leaving it.

- [ ] **Step 2a: Write the description so the model invokes the skill**

The skill is meant to be picked up by an agent that has just written a plan, not typed by a person. Make the front matter say when it applies, in those terms. Replace the `description` and `when_to_use` fields with:

```yaml
description: Turn a development plan into a reviewable, commentable page. Use this whenever you have written or revised a plan and the human is going to read it — you emit a structured plan document and artefacto renders it. Never write the HTML yourself.
when_to_use: You have just produced a plan, or are revising one after feedback, and a person needs to read or comment on it. Also applies when someone asks to see a plan visually. If a person invokes this skill directly, the plan content already exists: start from what is there rather than asking them to write it.
```

Keep the rest of the body as it is, apart from the renames in Step 2.

- [ ] **Step 3: Write the failing tests**

Append to `tests/cli.rs`:

```rust
#[test]
fn status_reports_fresh_after_a_render() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args(["plan", "render", &fixture("minimal.json"), "--out", out.to_str().unwrap(), "--no-open"])
        .assert()
        .success();
    bin()
        .args(["plan", "status", &fixture("minimal.json"), "--out", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("fresh"));
}

#[test]
fn status_reports_stale_when_the_plan_changed() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args(["plan", "render", &fixture("minimal.json"), "--out", out.to_str().unwrap(), "--no-open"])
        .assert()
        .success();
    bin()
        .args(["plan", "status", &fixture("kitchen-sink.json"), "--out", out.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(contains("stale"));
}

#[test]
fn status_reports_missing_when_nothing_was_rendered() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .args(["plan", "status", &fixture("minimal.json"), "--out", dir.path().join("absent.html").to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(contains("none"));
}

#[test]
fn status_json_carries_the_hashes_it_compared() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args(["plan", "render", &fixture("minimal.json"), "--out", out.to_str().unwrap(), "--no-open"])
        .assert()
        .success();
    let stdout = bin()
        .args(["plan", "status", &fixture("minimal.json"), "--out", out.to_str().unwrap(), "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(doc["state"], "fresh");
    assert_eq!(doc["plan_hash"], doc["rendered_hash"]);
}

#[test]
fn schema_prints_the_reference() {
    bin()
        .args(["plan", "schema"])
        .assert()
        .success()
        .stdout(contains("artefacto.plan/1"));
}

#[test]
fn no_command_output_mentions_loadout() {
    // artefacto is a separate project. Someone using it will not have loadout
    // and should never see its name.
    for args in [vec!["plan", "schema"], vec!["--help"], vec!["plan", "--help"]] {
        let out = bin().args(&args).assert().get_output().stdout.clone();
        let text = String::from_utf8_lossy(&out).to_lowercase();
        assert!(!text.contains("loadout"), "`{args:?}` mentioned loadout");
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test --test cli status`
Expected: FAIL, `unrecognized subcommand 'status'`.

- [ ] **Step 5: Add the CLI variants**

In `src/cli.rs`, add to `enum PlanAction`:

```rust
    /// Report whether a rendered file is fresh for a plan.
    Status {
        /// The plan file.
        file: PathBuf,
        /// The rendered HTML to compare against. Defaults to `plan.html`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the plan schema reference.
    Schema,
```

- [ ] **Step 6: Implement the actions**

In `src/commands/plan.rs`, extend the `match` in `run`:

```rust
        PlanAction::Status { file, out, json } => status(file, out.as_deref(), *json),
        PlanAction::Schema => {
            print!("{}", include_str!("../../skills/artefacto-plan/reference.md"));
            Ok(())
        }
```

and add:

```rust
fn status(file: &Path, out: Option<&Path>, json: bool) -> Result<()> {
    let checked = match check_one(file, false)? {
        Ok(ok) => ok,
        Err(errors) => {
            for e in &errors {
                eprintln!("error[{}] {}: {}", e.code, e.path, e.message);
            }
            return Err(PlanInvalid.into());
        }
    };
    let plan_hash = model::plan_hash(&checked.plan);

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let target = crate::paths::resolve_relative(&cwd, out.unwrap_or(Path::new("plan.html")));

    let rendered = std::fs::read_to_string(&target)
        .ok()
        .and_then(|c| crate::marker::extract_hash(&c));

    let state = match &rendered {
        Some(h) if *h == plan_hash => "fresh",
        Some(_) => "stale",
        None => "none",
    };

    if json {
        let doc = serde_json::json!({
            "state": state,
            "path": file.display().to_string(),
            "out": target.display().to_string(),
            "plan_hash": plan_hash,
            "rendered_hash": rendered,
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        match state {
            "fresh" => println!("render: fresh ({})", target.display()),
            "stale" => println!("render: stale — re-run `artefacto plan render`"),
            _ => println!("render: none — run `artefacto plan render`"),
        }
    }

    if state == "fresh" {
        Ok(())
    } else {
        Err(PlanInvalid.into())
    }
}
```

`PlanInvalid` gives exit code 1 here, which is what a script branching on freshness wants. Note in the completion report that `status` returning 1 for "stale" is deliberate, not an error condition.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --test cli`
Expected: PASS, 15 tests, including `schema_prints_the_reference`.

- [ ] **Step 8: Verify the gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 9: Commit**

```bash
git add src/cli.rs src/commands/plan.rs tests/cli.rs skills/
git commit -m "feat: add plan status, schema and the skill package"
```

The step numbers above shift by two because Steps 2 and 2a were added; renumber them sequentially as you go and keep the order.

---

### Task 10: The skill example test and CI

**Files:**
- Create: `tests/skill_examples.rs`, `.github/workflows/ci.yml`
- Modify: `README.md`

**Interfaces:**
- Consumes: `plan::model::parse`, and the skill package created in Task 9.
- Produces: a test that keeps the skill reference honest, and a green CI workflow.

**Spec reference:** sections 4.5 and 10. The skill is two files that reference each other, which is why plan 5 will emit it as a manifest rather than one text stream. This task keeps its examples tested.

- [ ] **Step 1: Write the failing test**

Create `tests/skill_examples.rs`:

```rust
//! Every fenced JSON example in the skill reference must parse and validate
//! under the real deserializer. The document is the schema, so a drifting
//! example is a bug in the schema's documentation.

use artefacto::plan::model;

#[test]
fn reference_json_examples_are_valid() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/skills/artefacto-plan/reference.md"
    ))
    .expect("read reference.md");

    let mut checked = 0;
    let mut in_block = false;
    let mut buf = String::new();

    for line in md.lines() {
        if !in_block {
            if line.trim_start().starts_with("```json") {
                in_block = true;
                buf.clear();
            }
            continue;
        }
        if line.trim_start().starts_with("```") {
            in_block = false;
            // Only whole plan documents are checked; fragments such as the
            // feedback example do not carry the plan format string.
            if buf.contains("\"artefacto.plan/1\"") {
                let parsed = model::parse(&buf, false)
                    .unwrap_or_else(|e| panic!("example #{checked} did not parse: {e:?}"));
                let issues = model::validate(&parsed.plan);
                assert!(issues.is_empty(), "example #{checked} failed validation: {issues:?}");
                checked += 1;
            }
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }

    assert!(checked > 0, "found no plan examples in reference.md — the extractor is broken");
}
```

- [ ] **Step 2: Run the test to verify it fails, then passes**

Run: `cargo test --test skill_examples`
Expected on a first run before the file exists: the test binary does not exist. Once written, it must PASS. If `reference_json_examples_are_valid` fails, an example in the copied reference drifted from the model. Fix the example, not the model.

- [ ] **Step 2a: Sweep the last stale references out of the ported source**

Two leftovers were found during earlier task reviews. Both are in `src/markdown.rs`,
which was copied verbatim, and neither is a constraint violation — one is test input, the
other a doc comment. Clean them anyway: this is the task that keeps the repo honest, and
they are the last places the word survives outside deliberate prose.

1. The module's top doc comment describes the renderer as "shared by studio and
   `load plan`". Neither exists here. Rewrite the sentence to describe what the module
   does in artefacto: it renders untrusted markdown for the plan page.
2. The test `leading_generated_comments_are_stripped` uses `<!-- loadout:generated x -->`
   as its sample input, and asserts the output does not contain `loadout:generated`.
   Change both to artefacto's own marker. `strip_leading_comments` does not inspect the
   comment's content, so this cannot change behaviour — run the test to confirm.

- [ ] **Step 2b: Add the repo-wide naming check**

Append to `tests/skill_examples.rs`:

```rust
/// The owner's rule for this project: someone using artefacto will not have loadout
/// installed and should never encounter its name. This walks the code and the skill
/// package — the things artefacto ships and emits.
///
/// It deliberately does NOT walk `tests/fixtures/`. Those are sample plan documents,
/// and one of them is a real plan *about* loadout, so its prose names the product on
/// almost every line. That is input data, not anything artefacto writes. Output is
/// covered where it belongs: `render.rs`'s `a_rendered_page_never_mentions_loadout`
/// renders a fixture and asserts the resulting page is clean, which is the property
/// that actually matters.
#[test]
fn no_source_file_mentions_loadout_outside_the_deprecated_alias() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for dir in ["src", "skills"] {
        let mut stack = vec![root.join(dir)];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("read dir") {
                    stack.push(entry.expect("dir entry").path());
                }
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue; // binary asset, such as an embedded font
            };
            for (n, line) in text.lines().enumerate() {
                if !line.to_lowercase().contains("loadout") {
                    continue;
                }
                // Sanctioned occurrences, each deliberate:
                //  * the deprecated format string the parser accepts on read;
                //  * the tests that enforce this very rule, which must name what
                //    they are looking for in order to look for it.
                if line.contains("LEGACY_FORMAT")
                    || line.contains("loadout.plan/1")
                    || line.contains("never_mentions_loadout")
                    || line.contains("the rendered page mentions loadout")
                {
                    continue;
                }
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "artefacto must not carry loadout's name:\n{}",
        offenders.join("\n")
    );
}
```

- [ ] **Step 2c: Run it and watch it hold**

Run: `cargo test --test skill_examples no_source_file_mentions_loadout`
Expected: PASS after Step 2a. If it fails, it is naming you the exact file and line still
carrying the name — fix that rather than widening the allowlist.

The allowlist is deliberately tiny: the deprecated format string, and the assertions in
the tests that enforce this rule. If you find yourself wanting to add a third entry,
that is a signal the code should change instead. One exception you may legitimately hit:
comments in the ported renderer that reference the old product's own serving concepts
without naming it. Those do not trip this test; rewrite them anyway if you see them,
since they describe machinery artefacto does not have.

- [ ] **Step 3: Write the CI workflow**

Create `.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always

jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --all-targets -- -D warnings
      - name: Test
        run: cargo test --all
```

- [ ] **Step 4: Update the README status section**

Replace the `## Status` section of `README.md` with:

```markdown
## Status

Early. The static renderer works: `artefacto plan check`, `render`, and
`status` validate an `artefacto.plan/1` document and produce a self-contained
HTML page. The interactive server, the page rewrite, and the artifact index
are not built yet.

The design spec is in `docs/superpowers/specs/`, and the implementation plans
are in `docs/superpowers/plans/`.
```

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean, every test in `cli.rs`, `marker_contract.rs`, `skill_examples.rs`, and the unit tests.

- [ ] **Step 6: Verify the binary end to end by hand**

```bash
cargo run --quiet -- plan render tests/fixtures/plan/kitchen-sink.json --out /tmp/artefacto-check.html --no-open
head -c 120 /tmp/artefacto-check.html
cargo run --quiet -- plan status tests/fixtures/plan/kitchen-sink.json --out /tmp/artefacto-check.html
```

Expected: the first line of the file is the marker line, and `status` prints `render: fresh`. Open `/tmp/artefacto-check.html` in a browser and confirm it looks like the loadout plan viewer, with phases, the dependency graph, and the theme toggle. Report what you saw; do not claim it works without looking.

- [ ] **Step 7: Commit**

```bash
git add tests/skill_examples.rs .github/workflows/ci.yml README.md
git commit -m "feat: keep the skill examples tested and add CI"
```

---

## What this plan deliberately leaves out

- Any server, socket, or event log. Plan 2.
- Any change to `plan.js` behaviour. It moves byte-identical here; plan 3 rewrites it.
- `artefacto skill --print` and `--install`. Plan 5. Tasks 9 and 10 only put the files on disk and keep them tested.
- Deleting the plan module from loadout. Plan 6, so the two coexist until the dispatcher lands.
- cargo-dist release configuration. Plan 5.
- The headless-Chromium browser smoke. It moves with the page, in plan 3. It drives the page's `#selftest` harness through identifiers Task 6 renames, and its `file://` plus sandboxed-iframe harness cannot test the served page plan 2 introduces, so porting it here would mean writing it twice.

  **What this leaves uncovered, stated plainly:** plan 1 ships with no browser-level test. The golden fixture pins the rendered HTML byte for byte, including the whole embedded script, so any change to the page's markup or behaviour fails a test here. What is *not* covered is whether the page still runs correctly in a real browser. That gap closes in plan 3.

## Verification for the whole plan

When all ten tasks are done:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Then confirm the cross-repo contract still holds by checking that loadout can read what artefacto writes:

```bash
cargo run --quiet -- plan render tests/fixtures/plan/minimal.json --out /tmp/x.html --no-open
cargo run --quiet -- plan status 2>/dev/null || true
```

The second command is informational: loadout's `status` looks in its own repo paths. The binding assertion is `tests/marker_contract.rs`, which pins the exact line both repos agree on.
