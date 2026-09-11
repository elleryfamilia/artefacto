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
