mod cli;

use std::path::PathBuf;
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
    /// Read what documents say about money: vendor, amounts, card, seats,
    /// usage and billing. Exits 0 when a receipt or invoice was found, 1
    /// when none was, 2 on errors.
    Read {
        /// Emails (.eml, .msg), PDFs, images, HTML or text files; `-` reads
        /// standard input.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,

        /// `provider` or `provider/model`: anthropic, openai, gemini or
        /// openai-compatible. Default: the config, then anthropic.
        #[arg(long, short)]
        extractor: Option<String>,

        /// The cheap first model: jev, clef, clef-flash, systemone or none.
        /// Default: the config, then none.
        #[arg(long, short)]
        decider: Option<String>,

        /// Print a readable summary instead of JSON.
        #[arg(long)]
        pretty: bool,

        /// One JSON object per line. The default for several inputs.
        #[arg(long)]
        jsonl: bool,

        /// How many documents to read at once.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },

    /// Ask only the cheap first model what documents are: receipt or not,
    /// paid or due, seats, usage, card. Exits 2 on errors.
    Decide {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,

        /// jev, clef, clef-flash or systemone, optionally with /model.
        #[arg(long, short)]
        decider: Option<String>,

        /// One JSON object per line.
        #[arg(long)]
        json: bool,

        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },

    /// List what's in a document: the email body, attachments, forwarded
    /// emails. No model calls.
    Parts {
        /// An email (.eml, .msg), PDF, image, HTML or text file; `-` reads
        /// standard input.
        input: PathBuf,

        /// Print JSON instead of a summary.
        #[arg(long)]
        json: bool,

        /// Also print each part's text.
        #[arg(long)]
        text: bool,
    },

    /// Show where the config lives, the models in effect and which keys
    /// are set.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Print version details.
    Version,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Write a commented starter config to ~/.spendyard/config.toml.
    Init,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Read {
            inputs,
            extractor,
            decider,
            pretty,
            jsonl,
            concurrency,
        } => {
            let options = cli::read::Options {
                inputs,
                extractor,
                decider,
                pretty,
                jsonl,
                concurrency,
            };

            cli::read::run(options).await.map(|outcome| {
                if outcome.errors > 0 {
                    ExitCode::from(2)
                } else if outcome.found > 0 {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            })
        }

        Command::Decide {
            inputs,
            decider,
            json,
            concurrency,
        } => {
            let options = cli::decide::Options {
                inputs,
                decider,
                json,
                concurrency,
            };

            cli::decide::run(options).await.map(|errors| {
                if errors > 0 {
                    ExitCode::from(2)
                } else {
                    ExitCode::SUCCESS
                }
            })
        }

        Command::Parts { input, json, text } => {
            cli::parts::run(&input, json, text).map(|()| ExitCode::SUCCESS)
        }

        Command::Config { action } => {
            let done = match action {
                Some(ConfigAction::Init) => cli::config_command::init(),
                None => cli::config_command::show(),
            };

            done.map(|()| ExitCode::SUCCESS)
        }

        Command::Version => {
            print_version();

            Ok(ExitCode::SUCCESS)
        }
    };

    match result {
        Ok(code) => code,

        Err(error) => {
            eprintln!("spendyard: {error:#}");

            ExitCode::from(2)
        }
    }
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
