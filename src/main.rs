use artefacto::cli::Cli;
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = artefacto::commands::dispatch(&cli) {
        // A plan that failed validation already printed its errors; anything
        // else is a usage or IO problem and belongs on stderr.
        if err
            .downcast_ref::<artefacto::commands::plan::PlanInvalid>()
            .is_some()
        {
            std::process::exit(1);
        }
        eprintln!("error: {err:#}");
        std::process::exit(2);
    }
}
