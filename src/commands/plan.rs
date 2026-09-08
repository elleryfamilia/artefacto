//! `artefacto plan` — validate, render and inspect plan artifacts.

use crate::cli::{PlanAction, PlanArgs};
use crate::plan::model;
use anyhow::Result;
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

/// A usage or IO problem the command already reported. Exit 2.
#[derive(Debug)]
pub struct UsageReported;

impl std::fmt::Display for UsageReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "usage or IO error")
    }
}

impl std::error::Error for UsageReported {}

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
///
/// An unreadable file becomes a per-file issue (code `unreadable`) rather
/// than a propagated error, so one bad path in a batch doesn't stop the
/// files around it from being checked.
fn check_one(path: &Path, lenient: bool) -> std::result::Result<Checked, Vec<model::Issue>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            return Err(vec![model::Issue::new(
                "/",
                "unreadable",
                format!("could not read {}: {e}", path.display()),
            )])
        }
    };
    let parsed = model::parse(&raw, lenient)?;
    let errors = model::validate(&parsed.plan);
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut warnings = parsed.warnings;
    warnings.extend(model::advisories(&parsed.plan));
    Ok(Checked {
        plan: parsed.plan,
        warnings,
    })
}

fn task_count(plan: &model::Plan) -> usize {
    plan.phases.iter().map(|p| p.tasks.len()).sum()
}

/// `check_one`'s only IO-error shape: a single `unreadable` issue.
fn is_unreadable(errors: &[model::Issue]) -> bool {
    errors.len() == 1 && errors[0].code == "unreadable"
}

fn check(files: &[std::path::PathBuf], json: bool, lenient: bool) -> Result<()> {
    let mut entries = Vec::with_capacity(files.len());
    let mut any_unreadable = false;
    let mut any_invalid = false;

    for path in files {
        match check_one(path, lenient) {
            Ok(ok) => {
                let hash = model::plan_hash(&ok.plan);
                entries.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "ok": true,
                    "plan_hash": hash,
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
                        crate::hash::short(&hash)
                    );
                    for w in &ok.warnings {
                        println!("  warning[{}] {}: {}", w.code, w.path, w.message);
                    }
                }
            }
            Err(errors) => {
                entries.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "ok": false,
                    "errors": errors,
                    "warnings": [],
                }));
                if is_unreadable(&errors) {
                    any_unreadable = true;
                    for e in &errors {
                        eprintln!("error: {}", e.message);
                    }
                } else {
                    any_invalid = true;
                    if !json {
                        println!("{}: INVALID", path.display());
                        for e in &errors {
                            println!("  error[{}] {}: {}", e.code, e.path, e.message);
                        }
                    }
                }
            }
        }
    }

    if json {
        let doc = serde_json::json!({ "ok": !any_unreadable && !any_invalid, "files": entries });
        println!("{}", serde_json::to_string(&doc)?);
    }

    if any_unreadable {
        return Err(UsageReported.into());
    }
    if any_invalid {
        return Err(PlanInvalid.into());
    }
    Ok(())
}
