//! Command implementations.

pub mod plan;
pub mod serve;

use crate::cli::{Cli, Command};

/// Route a parsed command line to its implementation.
pub fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan(args) => plan::run(args),
        Command::Serve(args) => serve::serve(args),
        Command::Stop => serve::stop(),
        Command::Status { json } => serve::status(*json),
    }
}
