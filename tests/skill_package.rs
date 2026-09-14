//! `artefacto skill`: the package as a manifest and as files, and the
//! promise that what the skill tells an agent to type is something the
//! binary accepts.

use artefacto::cli::Cli;
use assert_cmd::Command;
use clap::Parser;
use std::path::Path;

fn bin() -> Command {
    let mut c = Command::cargo_bin("artefacto").expect("binary");
    // Every test names the home it means. A variable inherited from the
    // developer's own shell must not send a test's writes to their real
    // agent configuration.
    c.env_remove("CLAUDE_CONFIG_DIR");
    c
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

/// The binary on its own does nothing for anybody: the skill is what teaches
/// an agent the loop. `--for` puts it where the agents on this machine read
/// from, so installing artefacto and using it is the whole setup.
#[test]
fn for_all_installs_into_every_agent_found_and_no_others() {
    let home = tempfile::tempdir().expect("home");
    // Two of the five are here. The other three are not, and a directory is
    // never created for a tool the person does not use.
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".config/opencode")).unwrap();

    let out = bin()
        .args(["skill", "--for", "all"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(said.contains("claude:"), "{said}");
    assert!(said.contains("opencode:"), "{said}");
    assert!(!said.contains("codex"), "{said}");

    for path in PATHS {
        assert_eq!(
            std::fs::read_to_string(home.path().join(".claude/skills").join(path)).unwrap(),
            repo_file(&format!("skills/{path}")),
            "the file the binary carries, byte for byte"
        );
        assert!(home
            .path()
            .join(".config/opencode/skills")
            .join(path)
            .exists());
    }
    assert!(
        !home.path().join(".codex").exists(),
        "no directory for an agent that is not installed"
    );
}

#[test]
fn an_agent_named_outright_is_installed_into_whether_or_not_it_is_there() {
    let home = tempfile::tempdir().expect("home");
    let out = bin()
        .args(["skill", "--for", "codex"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        home.path()
            .join(".codex/skills/artefacto-plan/SKILL.md")
            .exists(),
        "asking for it by name is the person saying it is there"
    );
}

#[test]
fn an_agent_this_build_does_not_know_is_refused_by_name() {
    let home = tempfile::tempdir().expect("home");
    let out = bin()
        .args(["skill", "--for", "claude,nope"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(said.contains("no agent called nope"), "{said}");
    assert!(
        said.contains("claude") && said.contains("opencode"),
        "it says which ones it knows: {said}"
    );
    assert!(
        !home.path().join(".claude").exists(),
        "and writes nothing: guessing writes files into somebody's home"
    );
}

#[test]
fn with_no_agents_on_the_machine_it_says_so_rather_than_failing() {
    let home = tempfile::tempdir().expect("home");
    let out = bin()
        .args(["skill", "--for", "all"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no agent found"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A skills directory commonly holds links into a shared store. Writing
/// through one truncates whatever it points at, which may not even be in
/// this person's home.
#[test]
fn a_skill_that_is_a_symlink_is_refused_rather_than_written_through() {
    let home = tempfile::tempdir().expect("home");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let real = elsewhere.path().join("artefacto-plan");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("SKILL.md"), "somebody else's file").unwrap();
    let skills = home.path().join(".claude/skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::os::unix::fs::symlink(&real, skills.join("artefacto-plan")).unwrap();

    let out = bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "it must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("symlink"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(real.join("SKILL.md")).unwrap(),
        "somebody else's file",
        "and the file the link pointed at is untouched"
    );
}

/// The directory can be real while a file inside it is the link. Writing in
/// place follows it; replacing the entry does not.
#[test]
fn a_linked_file_inside_the_skill_is_replaced_not_followed() {
    let home = tempfile::tempdir().expect("home");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let target = elsewhere.path().join("somebody-elses.md");
    std::fs::write(&target, "somebody else's file").unwrap();
    let root = home.path().join(".claude/skills/artefacto-plan");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&target, root.join("SKILL.md")).unwrap();

    let out = bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "somebody else's file",
        "the file the link pointed at is untouched"
    );
    assert!(
        !std::fs::symlink_metadata(root.join("SKILL.md"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "and the link itself was replaced by the real file"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("SKILL.md")).unwrap(),
        repo_file("skills/artefacto-plan/SKILL.md")
    );
}

/// A skill with no receipt is not artefacto's to touch, whatever its
/// contents: an older artefacto that left none, or a copy somebody put there
/// by hand, both read the same way.
#[test]
fn a_skill_with_no_receipt_is_left_alone_by_an_automatic_update() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path().join(".claude/skills/artefacto-plan");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("SKILL.md"), "put here by hand").unwrap();
    assert!(matches!(
        artefacto::commands::skill::standing(&home.path().join(".claude/skills")),
        artefacto::commands::skill::Standing::Theirs
    ));
}

/// Every installed file, byte for byte, in every agent selected -- and
/// nothing at all in any agent that was not.
#[test]
fn for_all_writes_the_whole_package_and_touches_no_other_agent() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    bin()
        .args(["skill", "--for", "all"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    for path in PATHS {
        assert_eq!(
            std::fs::read_to_string(home.path().join(".cursor/skills").join(path)).unwrap(),
            repo_file(&format!("skills/{path}")),
        );
    }
    for other in [".claude", ".codex", ".gemini", ".config/opencode"] {
        assert!(
            !home.path().join(other).exists(),
            "{other} was not selected and must not exist"
        );
    }
}

/// The receipt is what lets an automatic update tell its own work from
/// somebody else's.
#[test]
fn an_install_leaves_a_receipt_and_asking_outright_replaces_an_edited_copy() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    let root = home.path().join(".claude/skills/artefacto-plan");
    let receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".artefacto.json")).unwrap())
            .expect("json");
    assert_eq!(receipt["format"], "artefacto.skill/1");
    for path in PATHS {
        assert!(
            receipt["files"][path]
                .as_str()
                .is_some_and(|h| h.starts_with("sha256:")),
            "the receipt records what each file hashed to: {receipt}"
        );
    }

    // Asking outright replaces an edited copy: that is the person's own call.
    std::fs::write(root.join("SKILL.md"), "my own version").unwrap();
    bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("SKILL.md")).unwrap(),
        repo_file("skills/artefacto-plan/SKILL.md")
    );
}

/// `HOME=.` would make a repository that happens to hold a `.claude`
/// directory look like the person's configuration.
#[test]
fn a_relative_home_is_not_a_home() {
    let out = bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", ".")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("absolute"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Claude Code moves its whole configuration directory with an environment
/// variable; an install that only knows `~/.claude` writes to a directory
/// nothing reads.
#[test]
fn a_moved_configuration_directory_is_the_one_installed_into() {
    let home = tempfile::tempdir().expect("home");
    let moved = tempfile::tempdir().expect("moved");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(moved.path()).unwrap();

    bin()
        .args(["skill", "--for", "all"])
        .env("HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", moved.path())
        .output()
        .unwrap();
    assert!(
        moved.path().join("skills/artefacto-plan/SKILL.md").exists(),
        "written where the agent actually reads"
    );
    assert!(
        !home.path().join(".claude/skills").exists(),
        "and not to the default it no longer uses"
    );
}

/// An override that points nowhere useful is not a place to write. Falling
/// back to the default would put the skill where the agent is not reading.
#[test]
fn a_relative_configuration_override_means_no_agent_rather_than_the_default() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let out = bin()
        .args(["skill", "--for", "all"])
        .env("HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", "somewhere/relative")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no agent found"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !home.path().join(".claude/skills").exists(),
        "and nothing written to the default it is not using"
    );
}

/// Staging leaves nothing behind. A leftover would sit in the skills
/// directory looking like part of the package.
#[test]
fn an_install_leaves_no_staging_files_behind() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    let root = home.path().join(".claude/skills/artefacto-plan");
    let left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("artefacto-"))
        .collect();
    assert_eq!(left, Vec::<String>::new(), "no staging debris");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec![".artefacto.json", "SKILL.md", "reference.md"]);
}

/// The staging name is not a name anybody can sit on. A fixed one could
/// itself be a link somebody left in the skills directory, and writing the
/// staged copy would follow it before the rename ever happened.
#[test]
fn a_link_left_at_the_obvious_staging_name_is_not_written_through() {
    let home = tempfile::tempdir().expect("home");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let bait = elsewhere.path().join("somebody-elses.md");
    std::fs::write(&bait, "somebody else's file").unwrap();
    let root = home.path().join(".claude/skills/artefacto-plan");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&bait, root.join(".SKILL.md.artefacto-new")).unwrap();

    let out = bin()
        .args(["skill", "--for", "claude"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&bait).unwrap(),
        "somebody else's file",
        "the staged write went to a name of its own, not through the bait"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("SKILL.md")).unwrap(),
        repo_file("skills/artefacto-plan/SKILL.md")
    );
}
