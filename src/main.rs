use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use spendyard::{Document, Part};

/// Reads receipts and invoices and works out what you're paying for.
#[derive(Parser)]
#[command(name = "spendyard", version = spendyard::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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

    /// Print version details.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Parts { input, json, text } => parts(&input, json, text),

        Command::Version => {
            print_version();

            Ok(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,

        Err(error) => {
            eprintln!("spendyard: {error:#}");

            ExitCode::from(2)
        }
    }
}

fn parts(input: &Path, json: bool, with_text: bool) -> Result<()> {
    let (bytes, name) = read_input(input)?;

    let document = spendyard::read(&bytes, name.as_deref())?;

    if json {
        let output = serde_json::to_string_pretty(&document)?;

        println!("{output}");

        return Ok(());
    }

    print_summary(&document, with_text);

    Ok(())
}

/// The input's bytes and its file name, if it has one.
fn read_input(input: &Path) -> Result<(Vec<u8>, Option<String>)> {
    if input == Path::new("-") {
        let mut bytes = Vec::new();

        std::io::stdin()
            .read_to_end(&mut bytes)
            .context("reading standard input")?;

        return Ok((bytes, None));
    }

    let bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;

    let name = input
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());

    Ok((bytes, name))
}

fn print_summary(document: &Document, with_text: bool) {
    println!("{:?}", document.format);

    if let Some(headers) = &document.headers {
        let fields = [
            ("From", &headers.from),
            ("To", &headers.to),
            ("Subject", &headers.subject),
            ("Date", &headers.date),
        ];

        for (name, value) in fields {
            if let Some(value) = value {
                println!("  {name:<8} {value}");
            }
        }
    }

    println!();

    let label_width = document
        .parts
        .iter()
        .map(|part| part.label.chars().count())
        .max()
        .unwrap_or(0);

    for part in &document.parts {
        print_part(part, label_width, with_text);
    }

    if !document.warnings.is_empty() {
        println!();

        for warning in &document.warnings {
            println!("warning: {warning}");
        }
    }
}

fn print_part(part: &Part, label_width: usize, with_text: bool) {
    let kind = format!("{:?}", part.kind).to_lowercase();

    let mut details = vec![human_size(part.size)];

    if let Some(pages) = part.pages {
        let unit = if pages == 1 { "page" } else { "pages" };

        details.push(format!("{pages} {unit}"));
    }

    match &part.text {
        Some(text) => details.push(format!("{} chars", text.chars().count())),
        None => details.push("no text".to_string()),
    }

    if part.inline {
        details.push("inline".to_string());
    }

    println!(
        "  {:>2}  {:<label_width$}  {:<5}  {}",
        part.index,
        part.label,
        kind,
        details.join(", "),
    );

    if with_text && let Some(text) = &part.text {
        println!();

        for line in text.lines() {
            println!("      │ {line}");
        }

        println!();
    }
}

fn human_size(bytes: usize) -> String {
    let bytes = bytes as f64;

    if bytes < 1024.0 {
        return format!("{bytes} B");
    }

    if bytes < 1024.0 * 1024.0 {
        return format!("{:.1} KB", bytes / 1024.0);
    }

    format!("{:.1} MB", bytes / 1024.0 / 1024.0)
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
