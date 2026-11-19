//! `spendyard read`: what each document says about money.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use spendyard::{Extractor, ReadResult};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::input::read_input;
use super::{config, pretty};

pub struct Options {
    pub inputs: Vec<PathBuf>,
    pub extractor: Option<String>,
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

    let settings = config::extractor_settings(&config, options.extractor.as_deref());

    let extractor: Arc<dyn Extractor> = Arc::from(spendyard::extract::build(settings)?);

    let jsonl = options.jsonl || options.inputs.len() > 1;

    let limit = Arc::new(Semaphore::new(options.concurrency.max(1)));

    let mut tasks = JoinSet::new();

    for (position, input) in options.inputs.into_iter().enumerate() {
        let extractor = Arc::clone(&extractor);
        let limit = Arc::clone(&limit);

        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;

            let result = read_one(&input, extractor.as_ref()).await;

            (position, input, result)
        });
    }

    let mut finished = Vec::new();

    while let Some(joined) = tasks.join_next().await {
        finished.push(joined?);
    }

    // Results in the order the inputs were given.
    finished.sort_by_key(|(position, _, _)| *position);

    let mut outcome = Outcome {
        found: 0,
        errors: 0,
    };

    for (_, input, result) in finished {
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

async fn read_one(input: &std::path::Path, extractor: &dyn Extractor) -> Result<ReadResult> {
    let (bytes, name) = read_input(input)?;

    let document = spendyard::read(&bytes, name.as_deref())?;

    let result = spendyard::understand(&document, extractor).await?;

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
