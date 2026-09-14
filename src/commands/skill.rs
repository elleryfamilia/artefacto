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
    skills: &'static str,
}

impl Agent {
    /// Whether this agent keeps a configuration directory here.
    pub fn present(&self) -> bool {
        home_dir()
            .map(|h| h.join(self.home).is_dir())
            .unwrap_or(false)
    }

    pub fn skills_dir(&self) -> Result<PathBuf> {
        Ok(home_dir()
            .context("no HOME to install into")?
            .join(self.skills))
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
        skills: ".claude/skills",
    },
    Agent {
        name: "codex",
        home: ".codex",
        skills: ".codex/skills",
    },
    Agent {
        name: "cursor",
        home: ".cursor",
        skills: ".cursor/skills",
    },
    Agent {
        name: "gemini",
        home: ".gemini",
        skills: ".gemini/skills",
    },
    Agent {
        name: "opencode",
        home: ".config/opencode",
        skills: ".config/opencode/skills",
    },
];

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
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
pub fn sync_into_agents() -> Vec<String> {
    let mut changed = Vec::new();
    for agent in AGENTS.iter().filter(|a| a.present()) {
        let Ok(dir) = agent.skills_dir() else {
            continue;
        };
        if FILES.iter().all(|f| {
            std::fs::read_to_string(dir.join(f.path)).is_ok_and(|on_disk| on_disk == f.contents)
        }) {
            continue;
        }
        if install(&dir).is_ok() {
            changed.push(agent.name.to_string());
        }
    }
    changed
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

/// Write the package under `dir`, replacing what is there: an install over an
/// older version is the way to update it. Returns the paths written.
pub fn install(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::with_capacity(FILES.len());
    for file in FILES {
        let target = dir.join(file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&target, file.contents)
            .with_context(|| format!("writing {}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}
