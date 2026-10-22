mod commands;

use clap::Parser;
use commands::{audit, completion, coverage, limits, release_verify, Cli, Commands};

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Coverage(args) => coverage::run(args),
        Commands::Limits(args) => limits::run(args),
        Commands::Audit(args) => audit::run(args),
        Commands::Completion(args) => completion::run(args),
        Commands::ReleaseVerify(args) => release_verify::run(args),
        Commands::LimitsProbe(args) => limits::run_probe(args),
    };
    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
