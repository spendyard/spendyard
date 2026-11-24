//! `spendyard read`: what each document says about money.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use spendyard_core::ReadResult;

use super::input::{for_each_as_done, read_input};
use super::models::Models;
use super::store::{Source, Store};
use super::{config, pretty, transactions};

pub struct Options {
    pub inputs: Vec<PathBuf>,
    pub extractor: Option<String>,
    pub decider: Option<String>,
    pub pretty: bool,
    pub jsonl: bool,
    pub concurrency: usize,

    /// Don't keep the results in the store.
    pub no_store: bool,
}

/// A document read, with what's needed to keep it.
struct Read {
    bytes: Vec<u8>,
    document: spendyard_core::Document,
    result: ReadResult,
}

/// What happened across all inputs, for the exit code.
pub struct Outcome {
    pub found: usize,
    pub errors: usize,
}

pub async fn run(options: Options) -> Result<Outcome> {
    let config = config::load()?;

    let models = Arc::new(Models::load(
        &config,
        options.extractor.as_deref(),
        options.decider.as_deref(),
    )?);

    let jsonl = options.jsonl || options.inputs.len() > 1;

    let mut outcome = Outcome {
        found: 0,
        errors: 0,
    };

    let mut statements = 0;

    let mut store = if options.no_store {
        None
    } else {
        Some(Store::open()?)
    };

    // Several inputs are printed and saved as each finishes, in the order
    // they finish; one input prints as before.
    let work = |input: PathBuf| {
        let models = Arc::clone(&models);

        async move { read_one(&input, &models).await }
    };

    let done = |input: PathBuf, read: Result<Read>| -> Result<()> {
        let name = input.display().to_string();

        match read {
            Ok(read) => {
                if let Some(store) = store.as_mut() {
                    save(store, &input, &read)?;

                    if read.result.statement.is_some() {
                        statements += 1;
                    }
                }

                let has_charges = read
                    .result
                    .statement
                    .as_ref()
                    .is_some_and(|statement| !statement.charges.is_empty());

                if read.result.document.is_receipt || read.result.document.is_invoice || has_charges
                {
                    outcome.found += 1;
                }

                print(&name, &read.result, options.pretty, jsonl)?;
            }

            Err(error) => {
                outcome.errors += 1;

                eprintln!("spendyard: {name}: {error:#}");
            }
        }

        Ok(())
    };

    for_each_as_done(options.inputs, options.concurrency, work, done).await?;

    // New charges are matched to the receipts already in the store.
    if statements > 0
        && let Some(store) = &store
    {
        let summary = transactions::match_all(store).await?;

        if options.pretty {
            println!();
            transactions::print_summary(&summary);
        }
    }

    Ok(outcome)
}

fn save(store: &mut Store, input: &Path, read: &Read) -> Result<()> {
    let file_name = input
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());

    let origin = std::fs::canonicalize(input)
        .map(|path| format!("file:{}", path.display()))
        .unwrap_or_else(|_| "stdin".to_string());

    let source = Source {
        origin,
        name: file_name.as_deref(),
        received: None,
    };

    store.save(&read.bytes, &read.document, &source, &read.result)?;

    Ok(())
}

async fn read_one(input: &Path, models: &Models) -> Result<Read> {
    let (bytes, name) = read_input(input)?;

    let document = spendyard_core::read(&bytes, name.as_deref())?;

    let result = spendyard_core::understand(&document, &models.pipeline()).await?;

    Ok(Read {
        bytes,
        document,
        result,
    })
}

fn print(name: &str, result: &ReadResult, as_pretty: bool, jsonl: bool) -> Result<()> {
    if as_pretty {
        pretty::print_result(name, result);

        return Ok(());
    }

    if jsonl {
        let mut line = serde_json::to_value(result)?;

        line["input"] = json!(name);

        println!("{}", serde_json::to_string(&line)?);

        return Ok(());
    }

    println!("{}", serde_json::to_string_pretty(result)?);

    Ok(())
}
