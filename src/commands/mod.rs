//! Command implementations.

pub mod plan;

use crate::cli::{Cli, Command};

/// Route a parsed command line to its implementation.
pub fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan(args) => plan::run(args),
    }
}
