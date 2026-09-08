# artefacto Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `artefacto` binary as a standalone static plan renderer, moving the plan module out of loadout with its full test suite green, plus the `--json`, multi-file, `--lenient`, and generated-marker contract work the server phases depend on.

**Architecture:** A single Rust binary. The plan model, validator, deterministic SVG graph, and HTML renderer move across from the loadout repo essentially unedited; only their `crate::` paths change. Three small helpers loadout owns (hash, markdown sanitizer, generated-marker line) are copied rather than shared, because publishing a crate for three files is not worth it. Nothing in this plan starts a server or touches the browser page's behaviour.

**Tech Stack:** Rust 2021, edition floor 1.85. `clap` (derive) for the CLI, `serde` + `serde_json` for the model, `maud` for HTML, `pulldown-cmark` for markdown, `sha2` for hashing. `assert_cmd` + `predicates` for CLI tests.

**Spec:** `docs/superpowers/specs/2026-09-06-artefacto-design.md`

**Source repo:** loadout lives at `/Users/ellery/_git/rosita`. Referred to below as `$ROSITA`. Nothing in this plan modifies it. Removing loadout's copy happens in a later plan, so the two coexist until then.

## Global Constraints

- Rust edition 2021, `rust-version = "1.85"`, `[toolchain] channel = "stable"` with `rustfmt` and `clippy`.
- License MIT. Repository `https://github.com/elleryfamilia/artefacto`.
- The binary is named `artefacto`.
- The plan format string stays `loadout.plan/1`. Do not rename it in this plan.
- The rendered HTML's first line stays byte-identical to loadout's: the prefix `<!-- loadout:generated` followed by ` context=<plan hash> -->`. This is a cross-repo contract, frozen as a fixture in Task 2.
- The plan hash is `sha256:` + lowercase hex of the SHA-256 of the plan's JSON serialization, unchanged from loadout.
- Golden fixtures move byte-identical. If a golden does not match after a move, the move was wrong. Never regenerate a golden to make a test pass in this plan.
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

**Why this task exists:** loadout's `load plan status` reads the first line of the rendered HTML and compares the hash in it to the plan's hash. loadout's `GENERATED_MARKER` is only the prefix `<!-- loadout:generated`, so testing the prefix alone would let the two repos drift apart while both test suites stay green. A committed fixture of the exact line is the contract.

**Files:**
- Create: `src/marker.rs`, `tests/marker_contract.rs`, `tests/fixtures/marker/first-line.txt`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `hash::context_hash` from Task 1 (only in tests here).
- Produces: `artefacto::marker::MARKER_PREFIX: &str`, `artefacto::marker::line(hash: &str) -> String` returning the complete first line with no trailing newline, and `artefacto::marker::extract_hash(content: &str) -> Option<String>` returning the hash from the last marker line found.

- [ ] **Step 1: Write the frozen fixture**

Create `tests/fixtures/marker/first-line.txt` containing exactly one line and a trailing newline:

```
<!-- loadout:generated context=sha256:0000000000000000000000000000000000000000000000000000000000000000 -->
```

- [ ] **Step 2: Write the failing tests in `tests/marker_contract.rs`**

```rust
//! The generated first line is a contract with loadout's `load plan status`,
//! which parses it to decide whether a render is fresh. Both repos check this
//! same fixture. Changing it means changing it in both, deliberately.

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
    assert_eq!(marker::extract_hash("<!-- loadout:generated -->"), None, "no context= token");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test marker_contract`
Expected: FAIL to compile, `unresolved import \`artefacto::marker\``.

- [ ] **Step 4: Implement `src/marker.rs`**

```rust
//! The machine-readable first line of every generated file.
//!
//! This is a cross-repo contract. loadout's `load plan status` parses this
//! line to decide whether a rendered page is fresh for the current plan, and
//! its studio serves and cleans only files that start with the prefix. The
//! exact bytes are frozen in `tests/fixtures/marker/first-line.txt`; changing
//! them requires the same change in loadout.

/// Prefix of the machine-readable first line. Matches loadout's
/// `render::header::GENERATED_MARKER` byte for byte.
pub const MARKER_PREFIX: &str = "<!-- loadout:generated";

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

Temporarily change `MARKER_PREFIX` to `"<!-- artefacto:generated"` and run `cargo test --test marker_contract`. Expected: `emitted_line_matches_the_frozen_fixture` FAILS. Revert the change and confirm the test passes again. Do not commit the temporary change.

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

- [ ] **Step 4: Add a regression test for the fail-closed link behaviour**

Append to the `mod tests` block in `src/markdown.rs`:

```rust
    #[test]
    fn javascript_scheme_links_are_not_emitted_as_anchors() {
        let out = render_markdown("[click](javascript:alert(1))");
        assert!(!out.contains("javascript:"), "javascript: survived: {out}");
        assert!(!out.contains("<a href"), "unsafe destination still became a link: {out}");
    }

    #[test]
    fn raw_html_is_neutralized_to_text() {
        let out = render_markdown("<script>alert(1)</script>");
        assert!(!out.contains("<script"), "raw script tag survived: {out}");
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib markdown`
Expected: PASS, including the two new tests.

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
- Create: `src/plan/mod.rs`, `src/plan/model.rs`
- Create: `tests/fixtures/plan/` (moved fixtures)
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `hash::context_hash` from Task 1.
- Produces: `artefacto::plan::model::{Plan, Issue, Parsed}`, `parse(input: &str, lenient: bool) -> Result<Parsed, Vec<Issue>>`, `validate(&Plan) -> Vec<Issue>`, `advisories(&Plan) -> Vec<Issue>`, `plan_hash(&Plan) -> String`. `Issue` has public fields `path: String`, `code: String`, `message: String`, `hint: Option<String>`. `Parsed` has public fields `plan: Plan`, `warnings: Vec<Issue>`.

- [ ] **Step 1: Copy the model and the fixtures**

```bash
mkdir -p src/plan tests/fixtures/plan
cp "$ROSITA/src/plan/model.rs" src/plan/model.rs
cp "$ROSITA/tests/fixtures/plan/"*.json tests/fixtures/plan/
```

The JSON fixtures are `hostile.json`, `invalid-cycle.json`, `invalid-dangling-ref.json`, `invalid-dup-id.json`, `kitchen-sink.json`, `learning-v0-15.json`, and `minimal.json`. Copy the `.html` and `.svg` goldens in Task 6 and Task 5 respectively, not here.

- [ ] **Step 2: Create `src/plan/mod.rs`**

```rust
//! The `loadout.plan/1` artifact kind: schema, validation, and rendering.

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
git commit -m "feat: port the plan model, validator and fixtures"
```

---

### Task 5: Icons and the deterministic dependency graph

**Files:**
- Create: `src/plan/icons.rs`, `src/plan/svg.rs`, `tests/fixtures/plan/kitchen-sink-p-core.svg`
- Modify: `src/plan/mod.rs`

**Interfaces:**
- Consumes: `plan::model::Plan` from Task 4.
- Produces: `artefacto::plan::icons::is_icon_name(&str) -> bool`, `artefacto::plan::svg::phase_svg(plan: &Plan, phase_id: &str) -> Option<String>` (one phase's graph), and `artefacto::plan::svg::phase_graph_svg(plan: &Plan) -> Option<String>` (the whole-plan graph). Task 6's renderer calls both.

- [ ] **Step 1: Copy the modules and the golden**

```bash
cp "$ROSITA/src/plan/icons.rs" src/plan/icons.rs
cp "$ROSITA/src/plan/svg.rs" src/plan/svg.rs
cp "$ROSITA/tests/fixtures/plan/kitchen-sink-p-core.svg" tests/fixtures/plan/
```

- [ ] **Step 2: Declare the modules**

`src/plan/mod.rs`:

```rust
//! The `loadout.plan/1` artifact kind: schema, validation, and rendering.

pub mod icons;
pub mod model;
pub mod svg;
```

- [ ] **Step 3: Check for crate paths that need rewriting**

```bash
rg -n "crate::" src/plan/icons.rs src/plan/svg.rs
```

Every hit should be `crate::plan::…` or `crate::hash::…`, which resolve unchanged. Anything else means the module depends on something that did not move; stop and report it.

- [ ] **Step 4: Run the moved tests**

Run: `cargo test --lib plan::svg plan::icons`
Expected: PASS, including the golden comparison against `kitchen-sink-p-core.svg`.

- [ ] **Step 5: Prove the golden is load-bearing**

Temporarily change one literal in `svg.rs` that affects output, such as a node's corner radius, and run `cargo test --lib plan::svg`. Expected: the golden test FAILS. Revert and confirm it passes. Do not commit the temporary change.

- [ ] **Step 6: Add a determinism test**

Append to `mod tests` in `src/plan/svg.rs`, using the same graph entry point the existing golden test calls:

```rust
    #[test]
    fn graph_output_is_deterministic_across_calls() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/plan/kitchen-sink.json"
        ))
        .unwrap();
        let plan = crate::plan::model::parse(&raw, false).unwrap().plan;
        let phase_id = plan.phases.first().expect("kitchen sink has a phase").id.clone();
        assert_eq!(
            phase_svg(&plan, &phase_id),
            phase_svg(&plan, &phase_id),
            "one phase's graph must be byte-identical across calls"
        );
        assert_eq!(
            phase_graph_svg(&plan),
            phase_graph_svg(&plan),
            "the whole-plan graph must be byte-identical across calls"
        );
    }
```

The golden test at `svg.rs:568` already compares `phase_svg` against `kitchen-sink-p-core.svg`; this adds the determinism assertion it does not make.

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
//! The `loadout.plan/1` artifact kind: schema, validation, and rendering.

pub mod icons;
pub mod model;
pub mod render;
pub mod svg;
```

- [ ] **Step 4: Run the moved tests to verify the golden still matches**

Run: `cargo test --lib plan::render`
Expected: PASS. The golden `kitchen-sink.html` was generated by loadout and must match byte for byte, which proves the marker rewrite in Step 2 produced identical bytes. If it does not match, the rewrite is wrong. Do not regenerate the golden.

- [ ] **Step 5: Add a test tying the render to the marker contract**

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

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib plan::render`
Expected: PASS.

- [ ] **Step 7: Verify the whole gate**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add src/plan/render.rs src/plan/assets/ src/plan/mod.rs tests/fixtures/plan/kitchen-sink.html tools/build-plan-fonts.py
git commit -m "feat: port the plan renderer and page assets"
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
    assert!(html.starts_with("<!-- loadout:generated context=sha256:"), "first line: {:?}", html.lines().next());
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

- [ ] **Step 2: Retarget the commands in the skill text**

The copied text tells agents to run `load plan …`. Rewrite those to `artefacto plan …`:

- `name:` in the SKILL.md front matter becomes `artefacto-plan`.
- `load plan` becomes `artefacto plan` throughout both files, including `load plan check --json` and `load plan schema`.
- The paths `.loadout/workflow/artifacts/plan.json` and `plan-feedback.json` stay as they are. loadout's dispatcher still supplies them, and changing them is plan 6's job.

Leave the `loadout.plan/1` and `loadout.plan-feedback/1` format strings untouched. They are the wire format, not a command name.

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
        .stdout(contains("loadout.plan/1"));
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
            if buf.contains("\"loadout.plan/1\"") {
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
`status` validate a `loadout.plan/1` document and produce a self-contained
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
