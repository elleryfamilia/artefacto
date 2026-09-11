//! `artefacto skill`: the package as a manifest and as files, and the
//! promise that what the skill tells an agent to type is something the
//! binary accepts.

use artefacto::cli::Cli;
use assert_cmd::Command;
use clap::Parser;
use std::path::Path;

fn bin() -> Command {
    Command::cargo_bin("artefacto").expect("binary")
}

fn repo_file(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

const PATHS: [&str; 2] = ["artefacto-plan/SKILL.md", "artefacto-plan/reference.md"];

#[test]
fn print_is_a_manifest_of_every_file_in_the_package() {
    let out = bin().args(["skill", "--print"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(manifest["format"], "artefacto.skill/1");
    assert_eq!(manifest["artefacto"], env!("CARGO_PKG_VERSION"));
    let skills = manifest["skills"].as_array().expect("skills");
    assert_eq!(
        skills.len(),
        1,
        "one skill per artifact kind; plan is the only kind"
    );
    assert_eq!(skills[0]["name"], "artefacto-plan");
    let files = skills[0]["files"].as_array().expect("files");
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths, PATHS,
        "both files, at the paths a skills directory expects"
    );
    for file in files {
        let path = file["path"].as_str().unwrap();
        assert_eq!(
            file["contents"].as_str().unwrap(),
            repo_file(&format!("skills/{path}")),
            "{path} is byte-identical to the file in the repository"
        );
    }
}

#[test]
fn install_writes_the_files_the_manifest_carries_and_replaces_an_older_copy() {
    let dir = tempfile::tempdir().unwrap();
    // An older, different version is already there.
    let stale = dir.path().join("artefacto-plan/SKILL.md");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "old").unwrap();

    let out = bin()
        .args(["skill", "--install"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for rel in PATHS {
        let target = dir.path().join(rel);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            repo_file(&format!("skills/{rel}")),
            "{rel} written whole"
        );
        assert!(
            stdout.contains(&target.display().to_string()),
            "the paths written are printed: {stdout}"
        );
    }
    // Running it again is the way to update, not an error.
    bin()
        .args(["skill", "--install"])
        .arg(dir.path())
        .assert()
        .success();
}

#[test]
fn skill_needs_exactly_one_form() {
    bin().arg("skill").assert().code(2);
    bin()
        .args(["skill", "--print", "--install", "/tmp/x"])
        .assert()
        .code(2);
}

#[test]
fn skill_md_carries_the_frontmatter_a_harness_reads() {
    let md = repo_file("skills/artefacto-plan/SKILL.md");
    let mut lines = md.lines();
    assert_eq!(lines.next(), Some("---"), "frontmatter first");
    let front: Vec<&str> = lines.by_ref().take_while(|l| *l != "---").collect();
    assert!(
        front.contains(&"name: artefacto-plan"),
        "the name a lifecycle installs and removes by: {front:?}"
    );
    let description = front
        .iter()
        .find(|l| l.starts_with("description:"))
        .expect("a description, which is what an agent reads to decide whether the skill applies");
    assert!(
        description.len() > 80,
        "the description says when the skill applies, not just what it is: {description}"
    );
    assert!(
        md.contains("reference.md"),
        "SKILL.md points at reference.md; that pointer is why the skill is a package"
    );
}

/// Every command line the skill tells an agent to run, as the agent would
/// run it after filling in the placeholders. Extracted from fenced `bash`
/// blocks in both files: a line starting with `artefacto`, with `\`
/// continuations joined, trailing comments dropped, and the rest of a
/// pipeline ignored. A `text` block is a synopsis, not a command.
fn prescribed_commands(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut is_bash = false;
    let mut pending = String::new();
    for raw in md.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_block = !in_block;
            is_bash = in_block && matches!(line.trim_start_matches('`'), "bash" | "sh");
            continue;
        }
        if !is_bash {
            continue;
        }
        if !in_block {
            continue;
        }
        if !pending.is_empty() {
            pending.push(' ');
        } else if !line.starts_with("artefacto ") {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            continue;
        }
        pending.push_str(line);
        out.push(std::mem::take(&mut pending));
    }
    out.into_iter()
        .map(|cmd| {
            let mut cmd = cmd;
            for stop in [" # ", " | ", " > ", " && ", " ; "] {
                if let Some((head, _)) = cmd.split_once(stop) {
                    cmd = head.to_string();
                }
            }
            cmd.trim().to_string()
        })
        .collect()
}

/// `<seq>` and `"$SESSION"` become a value clap accepts for any type.
fn fill_placeholders(cmd: &str) -> String {
    let mut out = String::new();
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                for inner in chars.by_ref() {
                    if inner == '>' {
                        break;
                    }
                }
                out.push('1');
            }
            '$' => {
                if chars.peek() == Some(&'{') {
                    for inner in chars.by_ref() {
                        if inner == '}' {
                            break;
                        }
                    }
                } else {
                    while chars
                        .peek()
                        .is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_')
                    {
                        chars.next();
                    }
                }
                out.push('1');
            }
            other => out.push(other),
        }
    }
    out
}

/// A shell's word splitting, for the quoting the skill actually uses.
fn shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in cmd.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

#[test]
fn every_command_the_skill_prescribes_is_one_the_binary_accepts() {
    let mut checked = 0;
    for rel in PATHS {
        let md = repo_file(&format!("skills/{rel}"));
        for cmd in prescribed_commands(&md) {
            let words = shell_words(&fill_placeholders(&cmd));
            assert_eq!(
                words.first().map(String::as_str),
                Some("artefacto"),
                "{cmd}"
            );
            if let Err(e) = Cli::try_parse_from(&words) {
                panic!("{rel} prescribes a command the binary refuses:\n  {cmd}\n{e}");
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 10,
        "only {checked} commands found; the extractor is broken or the skill has lost its commands"
    );
}

#[test]
fn the_extractor_reads_the_shapes_the_skill_uses() {
    let md = "\
text `artefacto ignored` outside a block
```text
artefacto plan push <file> [--json]   # a synopsis, not a command
```
```bash
artefacto plan push plan.json --json   # comment
artefacto reply --session \"$SESSION\" --thread <thread> \"a reply with spaces\" | tee out
artefacto await --session \"$SESSION\" \\
  --ack <seq>
not-artefacto
```
";
    let cmds = prescribed_commands(md);
    assert_eq!(
        cmds,
        [
            "artefacto plan push plan.json --json",
            "artefacto reply --session \"$SESSION\" --thread <thread> \"a reply with spaces\"",
            "artefacto await --session \"$SESSION\" --ack <seq>",
        ]
    );
    assert_eq!(
        shell_words(&fill_placeholders(&cmds[1])),
        [
            "artefacto",
            "reply",
            "--session",
            "1",
            "--thread",
            "1",
            "a reply with spaces"
        ]
    );
    assert!(Cli::try_parse_from(shell_words(&fill_placeholders(&cmds[2]))).is_ok());
    assert!(
        Cli::try_parse_from(["artefacto", "reply", "--no-such-flag"]).is_err(),
        "the parser refuses what the skill must not say"
    );
}
