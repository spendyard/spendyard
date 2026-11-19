use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Reads receipts and invoices and works out what you're paying for.
#[derive(Parser)]
#[command(name = "spendyard", version = spendyard::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print version details.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Version => print_version(),
    }

    ExitCode::SUCCESS
}

fn print_version() {
    println!("spendyard {}", spendyard::VERSION);
    println!("schema {}", spendyard::SCHEMA_VERSION);
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }
}
