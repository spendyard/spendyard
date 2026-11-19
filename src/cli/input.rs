//! Reading input files, or standard input for `-`, one or many at a time.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use anyhow::{Context, Result};

/// The input's bytes and its file name, if it has one.
pub fn read_input(input: &Path) -> Result<(Vec<u8>, Option<String>)> {
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

/// Runs `work` on every input, at most `concurrency` at a time, and
/// returns the results in the order the inputs were given.
pub async fn for_each<T, F, Fut>(
    inputs: Vec<PathBuf>,
    concurrency: usize,
    work: F,
) -> Result<Vec<(PathBuf, Result<T>)>>
where
    T: Send + 'static,
    F: Fn(PathBuf) -> Fut,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    let limit = Arc::new(Semaphore::new(concurrency.max(1)));

    let mut tasks = JoinSet::new();

    for (position, input) in inputs.into_iter().enumerate() {
        let limit = Arc::clone(&limit);

        let job = work(input.clone());

        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;

            (position, input, job.await)
        });
    }

    let mut finished = Vec::new();

    while let Some(joined) = tasks.join_next().await {
        finished.push(joined?);
    }

    finished.sort_by_key(|(position, _, _)| *position);

    let ordered = finished
        .into_iter()
        .map(|(_, input, result)| (input, result))
        .collect();

    Ok(ordered)
}

/// Like [`for_each`], but hands each result to `done` as soon as it is
/// ready, so long runs show progress and keep what finished even if a
/// later step fails. Results arrive in the order they finish.
pub async fn for_each_as_done<T, F, Fut, D>(
    inputs: Vec<PathBuf>,
    concurrency: usize,
    work: F,
    mut done: D,
) -> Result<()>
where
    T: Send + 'static,
    F: Fn(PathBuf) -> Fut,
    Fut: Future<Output = Result<T>> + Send + 'static,
    D: FnMut(PathBuf, Result<T>) -> Result<()>,
{
    let limit = Arc::new(Semaphore::new(concurrency.max(1)));

    let mut tasks = JoinSet::new();

    for input in inputs {
        let limit = Arc::clone(&limit);

        let job = work(input.clone());

        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;

            (input, job.await)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let (input, result) = joined?;

        done(input, result)?;
    }

    Ok(())
}
