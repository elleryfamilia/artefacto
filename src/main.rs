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
        eprintln!("error: {err:#}");
        std::process::exit(2);
    }
}
