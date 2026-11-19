//! Reading an input file, or standard input for `-`.

use std::io::Read;
use std::path::Path;

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
