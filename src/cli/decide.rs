//! `spendyard decide`: only the cheap first model's answers.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use serde_json::json;
use spendyard_core::Decider;
use spendyard_core::decide::{Answer, Decision};

use super::config;
use super::input::{for_each, read_input};

pub struct Options {
    pub inputs: Vec<PathBuf>,
    pub decider: Option<String>,
    pub json: bool,
    pub concurrency: usize,
}

pub async fn run(options: Options) -> Result<usize> {
    let config = config::load()?;

    let Some(decider) = super::models::decider(&config, options.decider.as_deref())? else {
        bail!(
            "no first model: pass --decider (jev, clef, clef-flash or systemone), or set provider under [decider] in the config"
        );
    };

    let results = for_each(options.inputs, options.concurrency, |input| {
        let decider = Arc::clone(&decider);

        async move { decide_one(&input, decider.as_ref()).await }
    })
    .await?;

    let mut errors = 0;

    for (input, result) in results {
        let name = input.display().to_string();

        match result {
            Ok(decision) if options.json => {
                let line = json!({
                    "input": name,
                    "decider": decider.name(),
                    "model": decision.model,
                    "answers": decision.answers,
                    "tokens": decision.usage,
                });

                println!("{}", serde_json::to_string(&line)?);
            }

            Ok(decision) => print_decision(&name, &decision),

            Err(error) => {
                errors += 1;

                eprintln!("spendyard: {name}: {error:#}");
            }
        }
    }

    Ok(errors)
}

async fn decide_one(input: &std::path::Path, decider: &dyn Decider) -> Result<Decision> {
    let (bytes, name) = read_input(input)?;

    let document = spendyard_core::read(&bytes, name.as_deref())?;

    match spendyard_core::decide::decide(&document, decider).await {
        Some(decision) => Ok(decision?),
        None => bail!("nothing this model can read: no text, and no images it accepts"),
    }
}

fn print_decision(name: &str, decision: &Decision) {
    println!("{name}");

    for (question, answer) in &decision.answers {
        let text = match answer {
            Answer::Choice { .. } => {
                let (choice, probability) = answer.chosen().unwrap_or(("?", 0.0));

                format!("{choice} ({probability:.2})")
            }

            Answer::YesNo { yes } => {
                let word = if *yes >= 0.5 { "yes" } else { "no" };

                format!("{word} ({:.2})", answer.certainty())
            }
        };

        println!("  {question:<14} {text}");
    }

    println!("  {} · {} tokens", decision.model, decision.usage.input);
    println!();
}
