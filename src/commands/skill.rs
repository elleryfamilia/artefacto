//! `artefacto skill`: the skill package, as a manifest or as files.
//!
//! Spec 4.5: the skill is a package, not a file. `SKILL.md` points at
//! `reference.md` and both are installed together, so the binary exposes two
//! forms. `--print` emits a JSON manifest of relative paths and contents,
//! which is what a lifecycle that installs skills into agent directories
//! consumes. `--install DIR` writes the same files under a directory for
//! anything that would rather copy them. One flat text stream could not
//! preserve the package, and the pointer from one file to the other would
//! break.
//!
//! The files are compiled in, so a binary is a complete distribution of the
//! skill that matches it. `plan schema` prints the same reference.

use crate::cli::SkillArgs;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const SKILL_FORMAT: &str = "artefacto.skill/1";

/// One file of the package, at its path relative to a skills directory.
pub struct SkillFile {
    pub path: &'static str,
    pub contents: &'static str,
}

/// The one skill this version ships. Spec 4.5: one skill per artifact kind,
/// each small and describing one schema.
pub const SKILL_NAME: &str = "artefacto-plan";

pub const FILES: &[SkillFile] = &[
    SkillFile {
        path: "artefacto-plan/SKILL.md",
        contents: include_str!("../../skills/artefacto-plan/SKILL.md"),
    },
    SkillFile {
        path: "artefacto-plan/reference.md",
        contents: include_str!("../../skills/artefacto-plan/reference.md"),
    },
];

pub fn run(args: &SkillArgs) -> Result<()> {
    if !args.agents.is_empty() {
        let chosen = resolve(&args.agents)?;
        if chosen.is_empty() {
            println!("no agent found on this machine; nothing installed");
            return Ok(());
        }
        for agent in chosen {
            let dir = agent.skills_dir()?;
            install(&dir)?;
            println!("{}: {}", agent.name, dir.join(SKILL_NAME).display());
        }
    }
    if args.print {
        println!("{}", manifest());
    }
    if let Some(dir) = &args.install {
        for written in install(dir)? {
            println!("{}", written.display());
        }
    }
    Ok(())
}

/// An agent this build knows how to install into.
///
/// `home` is the agent's own configuration directory. Its existence is how
/// we know the agent is installed: creating it would leave a stray directory
/// for a tool the person does not use.
pub struct Agent {
    pub name: &'static str,
    home: &'static str,
    /// An environment variable that moves this agent's whole configuration
    /// directory. Claude Code has one; where an agent has none this is
    /// `None` rather than a guessed name, because an install is a write into
    /// somebody's home and a guess there is not free.
    root_env: Option<&'static str>,
}

impl Agent {
    /// This agent's configuration directory: the environment override when
    /// it is set, else the default under the home.
    fn root(&self) -> Option<PathBuf> {
        if let Some(var) = self.root_env {
            if let Some(set) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                let moved = PathBuf::from(set);
                if moved.is_absolute() {
                    return Some(moved);
                }
            }
        }
        home_dir().map(|h| h.join(self.home))
    }

    /// Whether this agent keeps a configuration directory here.
    pub fn present(&self) -> bool {
        self.root().is_some_and(|r| r.is_dir())
    }

    pub fn skills_dir(&self) -> Result<PathBuf> {
        Ok(self
            .root()
            .context("no absolute HOME to install into")?
            .join("skills"))
    }
}

/// Every agent this version knows. Each reads skills from a `skills`
/// directory under its own configuration directory; a name that is not here
/// is refused rather than guessed at, because guessing writes files into
/// somebody's home.
pub const AGENTS: &[Agent] = &[
    Agent {
        name: "claude",
        home: ".claude",
        root_env: Some("CLAUDE_CONFIG_DIR"),
    },
    Agent {
        name: "codex",
        home: ".codex",
        root_env: None,
    },
    Agent {
        name: "cursor",
        home: ".cursor",
        root_env: None,
    },
    Agent {
        name: "gemini",
        home: ".gemini",
        root_env: None,
    },
    Agent {
        name: "opencode",
        home: ".config/opencode",
        root_env: None,
    },
];

/// The home to install under. Absolute only: with `HOME=.` a repository that
/// happens to contain a `.claude` directory would be read as the person's
/// configuration and written into.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
}

/// The agents a `--for` list names. `all` is every one found here; a name
/// given outright is honoured whether or not its directory exists, because
/// asking for it by name is the person saying it is there.
pub fn resolve(names: &[String]) -> Result<Vec<&'static Agent>> {
    let mut out: Vec<&'static Agent> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.eq_ignore_ascii_case("all") {
            for agent in AGENTS {
                if agent.present() && !out.iter().any(|a| a.name == agent.name) {
                    out.push(agent);
                }
            }
            continue;
        }
        let agent = AGENTS
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no agent called {name}; this build knows {}",
                    AGENTS.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
                )
            })?;
        if !out.iter().any(|a| a.name == agent.name) {
            out.push(agent);
        }
    }
    Ok(out)
}

/// Put the skill in front of every agent found here, and say which ones
/// changed.
///
/// Run from `plan push` as well as from `skill --for`, because a binary that
/// nobody can drive is no use: the skill is what teaches an agent the loop,
/// and it is compiled into this binary so the two can never drift. Writing
/// only where the contents differ keeps it quiet on every push after the
/// first, and makes an artefacto upgrade update the skill by itself.
pub fn sync_into_agents() -> Synced {
    let mut done = Synced::default();
    for agent in AGENTS.iter().filter(|a| a.present()) {
        let Ok(dir) = agent.skills_dir() else {
            continue;
        };
        match standing(&dir) {
            /* Somebody's own copy, or one this did not write. Left exactly
            as it is: an automatic update that reverts a person's edits is
            worse than one that never runs. */
            Standing::Theirs => {
                if !current(&dir) {
                    done.left.push(agent.name.to_string());
                }
            }
            Standing::Ours if current(&dir) => {}
            _ => match install(&dir) {
                Ok(_) => done.installed.push(agent.name.to_string()),
                /* Said, not swallowed. A half-written package is worth
                knowing about, and the push itself already succeeded, so this
                cannot fail the command. */
                Err(e) => done.failed.push(format!("{}: {e:#}", agent.name)),
            },
        }
    }
    done
}

/// Whether what is on disk already is what this binary would write.
fn current(dir: &Path) -> bool {
    FILES.iter().all(|f| {
        std::fs::read_to_string(dir.join(f.path)).is_ok_and(|on_disk| on_disk == f.contents)
    })
}

/// What one sync did, per agent.
#[derive(Default)]
pub struct Synced {
    pub installed: Vec<String>,
    /// Left alone because the copy there is not one artefacto wrote, or has
    /// been edited since it did.
    pub left: Vec<String>,
    pub failed: Vec<String>,
}

/// The manifest: every file's relative path and contents, under the skill's
/// name so a consumer installs and removes by one id.
pub fn manifest() -> serde_json::Value {
    serde_json::json!({
        "format": SKILL_FORMAT,
        "artefacto": env!("CARGO_PKG_VERSION"),
        "skills": [{
            "name": SKILL_NAME,
            "files": FILES
                .iter()
                .map(|f| serde_json::json!({ "path": f.path, "contents": f.contents }))
                .collect::<Vec<_>>(),
        }],
    })
}

/// The receipt artefacto leaves beside the skill it wrote: which version, and
/// what each file's contents hashed to.
///
/// It is what makes an automatic update safe. Without it there is no way to
/// tell a copy artefacto wrote from one somebody edited, and "replace
/// anything that differs" silently reverts a person's own changes on the
/// next push.
pub const RECEIPT: &str = "artefacto-plan/.artefacto.json";

fn receipt() -> serde_json::Value {
    serde_json::json!({
        "format": SKILL_FORMAT,
        "artefacto": env!("CARGO_PKG_VERSION"),
        "files": FILES
            .iter()
            .map(|f| (f.path.to_string(), crate::hash::bytes_hash(f.contents.as_bytes())))
            .collect::<std::collections::BTreeMap<_, _>>(),
    })
}

/// Whether the copy under `dir` is one artefacto wrote and nobody has
/// touched since: the receipt is there, and every file still hashes to what
/// the receipt recorded.
///
/// A missing skill is not "ours" and not "theirs" -- `Fresh` says so, because
/// writing where there is nothing is safe and replacing somebody's work is
/// not.
pub enum Standing {
    Fresh,
    Ours,
    Theirs,
}

pub fn standing(dir: &Path) -> Standing {
    if !dir.join(SKILL_NAME).exists() {
        return Standing::Fresh;
    }
    /* No receipt, or one this cannot read: not artefacto's to touch. One
    rule rather than two, because two that reach the same answer cannot be
    told apart by a test. */
    let Some(seen) = std::fs::read_to_string(dir.join(RECEIPT))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return Standing::Theirs;
    };
    for file in FILES {
        let Ok(on_disk) = std::fs::read(dir.join(file.path)) else {
            return Standing::Theirs;
        };
        if seen["files"][file.path] != serde_json::json!(crate::hash::bytes_hash(&on_disk)) {
            return Standing::Theirs;
        }
    }
    Standing::Ours
}

/// Write the package under `dir`, replacing what is there: an install over an
/// older version is the way to update it. Returns the paths written.
///
/// Never through a symlink. A skills directory commonly holds links into a
/// shared store, and `fs::write` follows one and truncates whatever it points
/// at -- somebody else's file, possibly outside the home entirely. Each file
/// goes to a temporary sibling and is renamed over the target, which replaces
/// the entry rather than following it, and a skill directory that is itself a
/// link is refused outright.
pub fn install(dir: &Path) -> Result<Vec<PathBuf>> {
    let root = dir.join(SKILL_NAME);
    if std::fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
        anyhow::bail!(
            "{} is a symlink; artefacto will not write through one",
            root.display()
        );
    }
    let mut written = Vec::with_capacity(FILES.len());
    for file in FILES {
        let target = dir.join(file.path);
        let Some(parent) = target.parent() else {
            anyhow::bail!("{} has no parent", target.display());
        };
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        let staged = parent.join(format!(
            ".{}.artefacto-new",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::write(&staged, file.contents)
            .with_context(|| format!("writing {}", staged.display()))?;
        std::fs::rename(&staged, &target)
            .with_context(|| format!("replacing {}", target.display()))?;
        written.push(target);
    }
    let target = dir.join(RECEIPT);
    let staged = target.with_file_name(".artefacto.json.artefacto-new");
    std::fs::write(&staged, format!("{}\n", receipt()))
        .with_context(|| format!("writing {}", staged.display()))?;
    std::fs::rename(&staged, &target).with_context(|| format!("replacing {}", target.display()))?;
    Ok(written)
}
