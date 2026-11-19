//! `spendyard parts`: what's in a document, without any model.

use std::path::Path;

use anyhow::Result;
use spendyard::{Document, Part};

use super::input::read_input;

pub fn run(input: &Path, json: bool, with_text: bool) -> Result<()> {
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
