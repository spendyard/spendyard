//! `spendyard read`: what each document says about money.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use spendyard::ReadResult;

use super::input::{for_each, read_input};
use super::models::Models;
use super::{config, pretty};

pub struct Options {
    pub inputs: Vec<PathBuf>,
    pub extractor: Option<String>,
    pub decider: Option<String>,
    pub pretty: bool,
    pub jsonl: bool,
    pub concurrency: usize,
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

    let results = for_each(options.inputs, options.concurrency, |input| {
        let models = Arc::clone(&models);

        async move { read_one(&input, &models).await }
    })
    .await?;

    let mut outcome = Outcome {
        found: 0,
        errors: 0,
    };

    for (input, result) in results {
        let name = input.display().to_string();

        match result {
            Ok(result) => {
                if result.document.is_receipt || result.document.is_invoice {
                    outcome.found += 1;
                }

                print(&name, &result, options.pretty, jsonl)?;
            }

            Err(error) => {
                outcome.errors += 1;

                eprintln!("spendyard: {name}: {error:#}");
            }
        }
    }

    Ok(outcome)
}

async fn read_one(input: &std::path::Path, models: &Models) -> Result<ReadResult> {
    let (bytes, name) = read_input(input)?;

    let document = spendyard::read(&bytes, name.as_deref())?;

    let result = spendyard::understand(&document, &models.pipeline()).await?;

    Ok(result)
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
