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
                assert!(
                    issues.is_empty(),
                    "example #{checked} failed validation: {issues:?}"
                );
                checked += 1;
            }
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }

    assert!(
        checked > 0,
        "found no plan examples in reference.md — the extractor is broken"
    );
}

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
/// A line carrying this marker is exempt. Every exemption is deliberate and visible.
const ALLOW_MARKER: &str = "naming-check: allow";

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
                continue; // not valid UTF-8; none of these exist under src/ or skills/ today
            };
            for (n, line) in text.lines().enumerate() {
                if !line.to_lowercase().contains("loadout") {
                    continue;
                }
                // A line may opt out explicitly, and only explicitly. Matching on
                // surrounding text instead would quietly widen over time; a marker
                // has to be typed deliberately and shows up in review.
                if line.contains(ALLOW_MARKER) {
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
