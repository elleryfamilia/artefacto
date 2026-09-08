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
        PlanAction::Check {
            files,
            json,
            lenient,
        } => check(files, *json, *lenient),
    }
}

/// One file's verdict, shared by `check` and (later) `render` and `status`.
struct Checked {
    plan: model::Plan,
    warnings: Vec<model::Issue>,
}

/// Read and validate one file. Returns the plan plus warnings, or the errors.
fn check_one(
    path: &Path,
    lenient: bool,
) -> Result<std::result::Result<Checked, Vec<model::Issue>>> {
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
    Ok(Ok(Checked {
        plan: parsed.plan,
        warnings,
    }))
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
