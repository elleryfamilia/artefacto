//! `artefacto plan` — validate, render and inspect plan artifacts.

use crate::cli::{PlanAction, PlanArgs};
use crate::plan::model;
use anyhow::{Context, Result};
use std::path::Path;

/// A failure the command has already reported to the user. Exit code 1.
#[derive(Debug)]
pub struct ReportedFailure;

impl std::fmt::Display for ReportedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reported failure")
    }
}

impl std::error::Error for ReportedFailure {}

/// A usage or IO problem the command has already reported. Exit code 2.
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
        PlanAction::Render {
            file,
            out,
            no_open,
            json,
        } => render(file, out.as_deref(), *no_open, *json),
        PlanAction::Status { file, out, json } => status(file, out.as_deref(), *json),
        PlanAction::Schema => {
            print!(
                "{}",
                include_str!("../../skills/artefacto-plan/reference.md")
            );
            Ok(())
        }
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
                        eprintln!("  warning[{}] {}: {}", w.code, w.path, w.message);
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
                            eprintln!("  error[{}] {}: {}", e.code, e.path, e.message);
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
        return Err(ReportedFailure.into());
    }
    Ok(())
}

/// A directory-creation or write failure while rendering. Reports an
/// envelope in `--json` mode (rather than letting a bare OS error reach
/// `main`'s fallback path with nothing on stdout) and always returns
/// `UsageReported`, since this is an invocation problem, not a problem with
/// the plan document itself.
fn report_write_failure(file: &Path, target: &Path, message: String, json: bool) -> Result<()> {
    if json {
        let issue = model::Issue::new("/", "write_failed", message);
        let doc = serde_json::json!({
            "ok": false,
            "path": file.display().to_string(),
            "out": target.display().to_string(),
            "errors": [issue],
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        eprintln!("error: {message}");
    }
    Err(UsageReported.into())
}

fn render(file: &Path, out: Option<&Path>, no_open: bool, json: bool) -> Result<()> {
    let checked = match check_one(file, false) {
        Ok(ok) => ok,
        Err(errors) => {
            // Unreadable files always report on stderr, as `check` does, because
            // the message is about the invocation rather than the document.
            let unreadable = errors.iter().any(|e| e.code == "unreadable");
            if unreadable || !json {
                for e in &errors {
                    eprintln!("error[{}] {}: {}", e.code, e.path, e.message);
                }
            }
            if json {
                let doc = serde_json::json!({
                    "ok": false,
                    "path": file.display().to_string(),
                    "errors": errors,
                });
                println!("{}", serde_json::to_string(&doc)?);
            }
            if unreadable {
                return Err(UsageReported.into());
            }
            return Err(ReportedFailure.into());
        }
    };

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let target = match out {
        Some(p) => crate::paths::resolve_relative(&cwd, p),
        None => crate::paths::resolve_relative(&cwd, Path::new("plan.html")),
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return report_write_failure(
                file,
                &target,
                format!("could not create {}: {e}", parent.display()),
                json,
            );
        }
    }

    let html = crate::plan::render::render(&checked.plan);
    if let Err(e) = std::fs::write(&target, &html) {
        return report_write_failure(
            file,
            &target,
            format!("could not write {}: {e}", target.display()),
            json,
        );
    }

    if json {
        let doc = serde_json::json!({
            "ok": true,
            "path": file.display().to_string(),
            "out": target.display().to_string(),
            "plan_hash": model::plan_hash(&checked.plan),
            "title": checked.plan.meta.title,
            "phases": checked.plan.phases.len(),
            "tasks": task_count(&checked.plan),
            "warnings": checked.warnings,
        });
        println!("{}", serde_json::to_string(&doc)?);
    } else {
        println!("rendered {}", target.display());
        for w in &checked.warnings {
            eprintln!("  warning[{}] {}: {}", w.code, w.path, w.message);
        }
    }

    if !no_open {
        crate::paths::open_browser(&crate::paths::file_url(&target));
    }
    Ok(())
}

fn status(file: &Path, out: Option<&Path>, json: bool) -> Result<()> {
    let checked = match check_one(file, false) {
        Ok(ok) => ok,
        Err(errors) => {
            // Unreadable files always report on stderr, as `check` does, because
            // the message is about the invocation rather than the document.
            let unreadable = errors.iter().any(|e| e.code == "unreadable");
            if unreadable || !json {
                for e in &errors {
                    eprintln!("error[{}] {}: {}", e.code, e.path, e.message);
                }
            }
            if json {
                let doc = serde_json::json!({
                    "ok": false,
                    // The plan couldn't be read or didn't validate, so there's
                    // no way to know whether a previous render matches it.
                    "state": "unknown",
                    "path": file.display().to_string(),
                    "errors": errors,
                });
                println!("{}", serde_json::to_string(&doc)?);
            }
            if unreadable {
                return Err(UsageReported.into());
            }
            return Err(ReportedFailure.into());
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
            "ok": state == "fresh",
            "state": state,
            "path": file.display().to_string(),
            "out": target.display().to_string(),
            "plan_hash": plan_hash,
            "rendered_hash": rendered,
            "title": checked.plan.meta.title,
            "phases": checked.plan.phases.len(),
            "tasks": task_count(&checked.plan),
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
        Err(ReportedFailure.into())
    }
}
