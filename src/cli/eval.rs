//! `spendyard label` and `spendyard eval`: expected answers, and scoring
//! model combinations against them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use spendyard::eval::{self, FIELDS, Outcome, Tally};
use spendyard::{Decider, Extractor, Pipeline, ReadResult};

use super::config;
use super::input::for_each;

/// The model `label` drafts expected answers with, unless told otherwise:
/// the most careful one, since its answers become the yardstick.
const LABEL_MODEL: &str = "anthropic/claude-opus-5-5";

/// Documents in `dir`: every file except expected answers, hidden files
/// and READMEs, in name order.
fn documents(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();

        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        let skip = !path.is_file()
            || name.starts_with('.')
            || name.ends_with(".json")
            || name.ends_with(".tsv")
            || name.eq_ignore_ascii_case("README.md");

        if !skip {
            files.push(path);
        }
    }

    files.sort();

    Ok(files)
}

fn expected_path(document: &Path) -> PathBuf {
    let mut name = document.file_name().unwrap_or_default().to_os_string();

    name.push(".expected.json");

    document.with_file_name(name)
}

fn read_expected(document: &Path) -> Result<Option<Map<String, Value>>> {
    let path = expected_path(document);

    if !path.exists() {
        return Ok(None);
    }

    let text = std::fs::read_to_string(&path)?;

    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("in {}", path.display()))?;

    match value {
        Value::Object(map) => Ok(Some(map)),
        _ => bail!("{} is not a JSON object", path.display()),
    }
}

async fn run_pipeline(
    document_path: &Path,
    decider: Option<&dyn Decider>,
    extractor: &dyn Extractor,
    threshold: f64,
) -> Result<ReadResult> {
    let bytes = std::fs::read(document_path)?;

    let name = document_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());

    let document = spendyard::read(&bytes, name.as_deref())?;

    let pipeline = Pipeline {
        decider,
        extractor,
        threshold,
    };

    let result = spendyard::understand(&document, &pipeline).await?;

    Ok(result)
}

pub struct LabelOptions {
    pub dir: PathBuf,
    pub extractor: Option<String>,
    pub limit: Option<usize>,
    pub concurrency: usize,
}

/// Drafts an expected answer for every document that has none. Drafts say
/// `"reviewed": false` until a person has checked them.
pub async fn label(options: LabelOptions) -> Result<()> {
    let config = config::load()?;

    let flag = options.extractor.as_deref().unwrap_or(LABEL_MODEL);

    let settings = config::extractor_settings(&config, Some(flag));

    let extractor: Arc<dyn Extractor> = Arc::from(spendyard::extract::build(settings)?);

    let mut todo: Vec<PathBuf> = documents(&options.dir)?
        .into_iter()
        .filter(|document| !expected_path(document).exists())
        .collect();

    if let Some(limit) = options.limit {
        todo.truncate(limit);
    }

    if todo.is_empty() {
        println!(
            "every document in {} has an expected answer",
            options.dir.display()
        );

        return Ok(());
    }

    println!(
        "drafting {} expected answers with {}",
        todo.len(),
        extractor.name()
    );

    let results = for_each(todo, options.concurrency, |document| {
        let extractor = Arc::clone(&extractor);

        async move { run_pipeline(&document, None, extractor.as_ref(), 1.0).await }
    })
    .await?;

    let mut written = 0;

    for (document, result) in results {
        match result {
            Ok(result) => {
                let draft = eval::draft_expected(&result, &extractor.name());

                std::fs::write(
                    expected_path(&document),
                    serde_json::to_string_pretty(&draft)? + "\n",
                )?;

                written += 1;
            }

            Err(error) => eprintln!("spendyard: {}: {error:#}", document.display()),
        }
    }

    println!("wrote {written}; review them and set \"reviewed\": true");

    Ok(())
}

pub struct EvalOptions {
    pub dir: PathBuf,
    pub deciders: Option<String>,
    pub extractors: Option<String>,
    pub limit: Option<usize>,
    pub concurrency: usize,
    pub wrong: bool,
    pub json: bool,
}

/// What one model combination did over the set.
struct Run {
    decider: Option<String>,
    extractor: String,
    tally: Tally,
    documents: usize,
    errors: usize,
    skipped: usize,
    missed: usize,
    flags: usize,
    cost: Option<f64>,
    ms: u64,
    wrong: Vec<(String, String, Outcome)>,
}

pub async fn run(options: EvalOptions) -> Result<()> {
    let config = config::load()?;

    let mut documents: Vec<(PathBuf, Map<String, Value>)> = Vec::new();

    let mut unreviewed = 0;

    for document in super::eval::documents(&options.dir)? {
        if let Some(expected) = read_expected(&document)? {
            if expected.get("reviewed") != Some(&json!(true)) {
                unreviewed += 1;
            }

            documents.push((document, expected));
        }
    }

    if let Some(limit) = options.limit {
        documents.truncate(limit);
    }

    if documents.is_empty() {
        bail!(
            "no expected answers in {}; `spendyard label {}` drafts them",
            options.dir.display(),
            options.dir.display()
        );
    }

    let decider_flags: Vec<Option<String>> = match &options.deciders {
        Some(list) => list
            .split(',')
            .map(|item| Some(item.trim().to_string()))
            .collect(),
        None => vec![None],
    };

    let extractor_flags: Vec<Option<String>> = match &options.extractors {
        Some(list) => list
            .split(',')
            .map(|item| Some(item.trim().to_string()))
            .collect(),
        None => vec![None],
    };

    let documents = Arc::new(documents);

    let mut runs = Vec::new();

    for decider_flag in &decider_flags {
        for extractor_flag in &extractor_flags {
            let decider: Option<Arc<dyn Decider>> =
                match config::decider_settings(&config, decider_flag.as_deref()) {
                    Some(settings) => Some(Arc::from(spendyard::decide::build(settings)?)),
                    None => None,
                };

            let extractor_settings = config::extractor_settings(&config, extractor_flag.as_deref());

            let extractor: Arc<dyn Extractor> =
                Arc::from(spendyard::extract::build(extractor_settings)?);

            let run = run_combination(
                Arc::clone(&documents),
                decider,
                extractor,
                config::threshold(&config),
                options.concurrency,
            )
            .await?;

            runs.push(run);
        }
    }

    if options.json {
        print_json(&runs)?;
    } else {
        print_report(&runs, documents.len(), unreviewed, options.wrong);
    }

    Ok(())
}

async fn run_combination(
    documents: Arc<Vec<(PathBuf, Map<String, Value>)>>,
    decider: Option<Arc<dyn Decider>>,
    extractor: Arc<dyn Extractor>,
    threshold: f64,
    concurrency: usize,
) -> Result<Run> {
    let paths: Vec<PathBuf> = documents.iter().map(|(path, _)| path.clone()).collect();

    let results = for_each(paths, concurrency, |path| {
        let decider = decider.clone();
        let extractor = Arc::clone(&extractor);

        async move { run_pipeline(&path, decider.as_deref(), extractor.as_ref(), threshold).await }
    })
    .await?;

    let mut run = Run {
        decider: decider.as_ref().map(|decider| decider.name()),
        extractor: extractor.name(),
        tally: Tally::default(),
        documents: documents.len(),
        errors: 0,
        skipped: 0,
        missed: 0,
        flags: 0,
        cost: Some(0.0),
        ms: 0,
        wrong: Vec::new(),
    };

    for ((path, result), (_, expected)) in results.into_iter().zip(documents.iter()) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let result = match result {
            Ok(result) => result,

            Err(error) => {
                run.errors += 1;

                eprintln!("spendyard: {name}: {error:#}");

                continue;
            }
        };

        let skipped = result.meta.extractor.is_none();

        if skipped {
            run.skipped += 1;

            if expected.get("about_money") == Some(&json!(true)) {
                run.missed += 1;
            }
        }

        run.flags += result.flags.len();
        run.ms += result.meta.ms;

        run.cost = match (run.cost, eval::cost(&result)) {
            (Some(total), Some(cost)) => Some(total + cost),
            _ => None,
        };

        let outcomes = eval::score(expected, &result);

        run.tally.add(&outcomes);

        for (field, outcome) in outcomes {
            if !outcome.is_right() {
                run.wrong.push((name.clone(), field, outcome));
            }
        }
    }

    Ok(run)
}

fn print_report(runs: &[Run], documents: usize, unreviewed: usize, show_wrong: bool) {
    println!("{documents} documents with expected answers ({unreviewed} not reviewed yet)");

    for run in runs {
        println!();

        let decider = run.decider.as_deref().unwrap_or("no first model");

        println!("{decider} → {}", run.extractor);

        for field in FIELDS {
            let (right, scored) = run.tally.get(field);

            if scored == 0 {
                continue;
            }

            let percent = right as f64 / scored as f64 * 100.0;

            println!("  {field:<15} {right:>4}/{scored:<4} {percent:>5.1}%");
        }

        let done = (run.documents - run.errors).max(1);

        println!(
            "  routing         {} skipped the extractor, {} of them receipts or invoices (missed)",
            run.skipped, run.missed
        );

        println!(
            "  flags           {} on {} documents",
            run.flags, run.documents
        );

        let cost = run
            .cost
            .map(|cost| format!("${:.4} per document", cost / done as f64))
            .unwrap_or_else(|| "cost unknown (no price for a model)".to_string());

        println!(
            "  cost and time   {cost}, {:.1} s per document",
            run.ms as f64 / done as f64 / 1000.0
        );

        if run.errors > 0 {
            println!("  errors          {}", run.errors);
        }

        if show_wrong && !run.wrong.is_empty() {
            println!("  wrong:");

            for (document, field, outcome) in &run.wrong {
                if let Outcome::Wrong { expected, got } = outcome {
                    println!("    {document}  {field}: expected {expected}, got {got}");
                }
            }
        }
    }
}

fn print_json(runs: &[Run]) -> Result<()> {
    for run in runs {
        let fields: Map<String, Value> = FIELDS
            .iter()
            .filter_map(|field| {
                let (right, scored) = run.tally.get(field);

                (scored > 0).then(|| {
                    (
                        field.to_string(),
                        json!({ "right": right, "scored": scored }),
                    )
                })
            })
            .collect();

        let wrong: Vec<Value> = run
            .wrong
            .iter()
            .map(|(document, field, outcome)| json!({ "document": document, "field": field, "detail": outcome }))
            .collect();

        let line = json!({
            "decider": run.decider,
            "extractor": run.extractor,
            "documents": run.documents,
            "errors": run.errors,
            "fields": fields,
            "skipped": run.skipped,
            "missed": run.missed,
            "flags": run.flags,
            "cost_usd": run.cost,
            "ms": run.ms,
            "wrong": wrong,
        });

        println!("{}", serde_json::to_string(&line)?);
    }

    Ok(())
}
