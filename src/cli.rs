//! Command-line surface. Types only — no behaviour lives here.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "artefacto",
    version,
    about = "Interactive artifacts between you and your coding agent"
)]
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
}
