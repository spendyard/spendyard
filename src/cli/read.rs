//! `spendyard read`: what each document says about money.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use spendyard::{Decider, Extractor, Pipeline, ReadResult};

use super::input::{for_each, read_input};
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

/// The models, shared by every task.
struct Models {
    decider: Option<Arc<dyn Decider>>,
    extractor: Arc<dyn Extractor>,
    threshold: f64,
}

pub async fn run(options: Options) -> Result<Outcome> {
    let config = config::load()?;

    let extractor_settings = config::extractor_settings(&config, options.extractor.as_deref());

    let decider = match config::decider_settings(&config, options.decider.as_deref()) {
        Some(settings) => Some(Arc::from(spendyard::decide::build(settings)?)),
        None => None,
    };

    let models = Arc::new(Models {
        decider,
        extractor: Arc::from(spendyard::extract::build(extractor_settings)?),
        threshold: config::threshold(&config),
    });

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

    let pipeline = Pipeline {
        decider: models.decider.as_deref(),
        extractor: models.extractor.as_ref(),
        threshold: models.threshold,
    };

    let result = spendyard::understand(&document, &pipeline).await?;

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
