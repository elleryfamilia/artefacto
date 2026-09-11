//! Command implementations.

pub mod agent;
pub mod plan;
pub mod serve;

use crate::cli::{Cli, Command};

/// An error that names the exit code the caller owes the agent.
///
/// Spec 5 fixes these: 4 when there is no server, 6 when the lease is held or
/// the session token was superseded, 7 for a stale `--base-revision`, 2 for
/// usage. Agents branch on them, so they are part of the contract rather than
/// an implementation detail, and carrying the code on the error keeps every
/// command from having to call `std::process::exit` itself.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl Exit {
    pub fn new(code: i32, message: impl Into<String>) -> Exit {
        Exit {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// Route a parsed command line to its implementation.
pub fn dispatch(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Plan(args) => plan::run(args),
        Command::Serve(args) => serve::serve(args),
        Command::Stop => serve::stop(),
        Command::Status { json } => serve::status(*json),
        Command::Await(args) => agent::await_cmd(args),
        Command::Events(args) => agent::events(args),
        Command::Ack(args) => agent::ack_cmd(args),
        Command::Reply(args) => agent::reply(args),
        Command::Resolve(args) => agent::resolve(args),
    }
}
