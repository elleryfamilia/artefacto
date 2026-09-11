use artefacto::cli::Cli;
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = artefacto::commands::dispatch(&cli) {
        // A plan that failed validation, or a batch with an unreadable file,
        // has already printed what it needs to; anything else is a fresh
        // usage or IO problem and belongs on stderr.
        if err
            .downcast_ref::<artefacto::commands::plan::ReportedFailure>()
            .is_some()
        {
            std::process::exit(1);
        }
        if err
            .downcast_ref::<artefacto::commands::plan::UsageReported>()
            .is_some()
        {
            std::process::exit(2);
        }
        // A command that named its own exit code owns the message too: an
        // agent branches on the code, so it must not be flattened to 2.
        if let Some(exit) = err.downcast_ref::<artefacto::commands::Exit>() {
            eprintln!("error: {exit}");
            std::process::exit(exit.code);
        }
        eprintln!("error: {err:#}");
        std::process::exit(2);
    }
}
