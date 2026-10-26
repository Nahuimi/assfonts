use assfonts::{
    cli::{Cli, Commands},
    commands::{SilentReporter, run_build, run_process},
};
use clap::Parser;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Build(options)) => run_build(options, &SilentReporter),
        None => run_process(cli.run, &SilentReporter),
    }
    .map_err(|err| err.into_report())
}
