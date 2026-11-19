//! The Spendyard pipeline: read a document, decide what it is, extract the
//! details and validate them.
//!
//! The library holds everything that works on a single document; `main.rs`
//! is the CLI around it. Keep filesystem, keychain and terminal code out of
//! the library: it moves into its own WebAssembly-ready crate when the cloud
//! work starts.

pub mod catalog;
pub mod decide;
pub mod document;
pub mod eval;
pub mod extract;
pub mod format;
pub mod money;
pub mod read;
pub mod result;
pub mod text;
pub mod validate;

use std::time::Instant;

pub use decide::Decider;
pub use document::{Document, EmailHeaders, Format, Part, PartKind};
pub use extract::{ExtractError, Extractor};

use extract::schema::DocumentType;
pub use read::{ReadError, read};
pub use result::ReadResult;

/// The version of this crate, reported as `meta.spendyard_version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the output format, reported as `meta.schema_version`.
///
/// Change it whenever the output changes.
pub const SCHEMA_VERSION: &str = "2026-11-19";

/// The models to read documents with.
pub struct Pipeline<'a> {
    /// The cheap first model. `None` sends every document to the extractor.
    pub decider: Option<&'a dyn Decider>,

    pub extractor: &'a dyn Extractor,

    /// How sure the first model must be that a document isn't about money
    /// before the extractor is skipped.
    pub threshold: f64,
}

/// How likely the first model thinks it is that a document is not about
/// money: from its yes/no `about_money` answer, or from `document_type`
/// being `other` when there is no such answer.
fn not_about_money(decision: &decide::Decision) -> Option<f64> {
    if let Some(yes) = decision.yes("about_money") {
        return Some(1.0 - yes);
    }

    match decision.choice("document_type") {
        Some(("other", probability)) => Some(probability),
        Some(_) => Some(0.0),
        None => None,
    }
}

/// Reads what `document` says about money, then checks the answer.
///
/// With a first model: it answers its questions first. When it is sure
/// the document isn't about money, that's the result; otherwise its
/// answers go to the extractor as hints, and are compared with the
/// extractor's answer afterwards. If the first model fails, the extractor
/// reads the document alone and a warning says so.
pub async fn understand(
    document: &Document,
    pipeline: &Pipeline<'_>,
) -> Result<ReadResult, ExtractError> {
    let started = Instant::now();

    let mut warnings = Vec::new();

    let decision = match pipeline.decider {
        Some(decider) => match decide::decide(document, decider).await {
            Some(Ok(decision)) => Some(decision),

            Some(Err(error)) => {
                warnings.push(format!("the first model failed ({error}); read without it"));

                None
            }

            None => None,
        },

        None => None,
    };

    if let Some(decision) = &decision
        && let Some(not_money) = not_about_money(decision)
        && not_money >= pipeline.threshold
    {
        let mut result = result::not_about_money(DocumentType::Other, not_money);

        result.warnings = warnings;

        attach_decision(&mut result, decision, pipeline);

        result.meta.ms = started.elapsed().as_millis() as u64;

        return Ok(result);
    }

    // A card statement is many payments, read by `charges read`; the
    // receipt extractor would only make one invoice of it.
    if let Some(decision) = &decision
        && let Some(("card_statement", probability)) = decision.choice("document_type")
        && probability >= pipeline.threshold
    {
        let mut result = result::not_about_money(DocumentType::CardStatement, probability);

        result.warnings = warnings;

        attach_decision(&mut result, decision, pipeline);

        result.meta.ms = started.elapsed().as_millis() as u64;

        return Ok(result);
    }

    let mut prompt = extract::prompt::build(document, pipeline.extractor.accepts());

    if !prompt.has_content() {
        let reason = if prompt.warnings.is_empty() {
            "it has no text".to_string()
        } else {
            prompt.warnings.join("; ")
        };

        return Err(ExtractError::NothingToRead(reason));
    }

    if let Some(note) = known_sender(document) {
        prompt.add_note("known-sender", &note);
    }

    if let Some(decision) = &decision {
        prompt.add_hints(&decision.hints());
    }

    let extraction = pipeline.extractor.extract(&prompt).await?;

    let mut result = result::assemble(
        document,
        extraction.output,
        pipeline.extractor.name(),
        extraction.model,
        extraction.usage,
        0,
    );

    result.warnings.extend(warnings);
    result.warnings.extend(prompt.warnings);

    if let Some(decision) = &decision {
        attach_decision(&mut result, decision, pipeline);
    }

    validate::validate(&mut result, document);

    result.meta.ms = started.elapsed().as_millis() as u64;

    Ok(result)
}

/// "Anthropic (anthropic.com, ai)" when the email's sender is a vendor the
/// catalog knows and not a payment processor.
fn known_sender(document: &Document) -> Option<String> {
    let from = document.headers.as_ref()?.from.as_deref()?;

    let catalog = catalog::catalog();

    if catalog.processor(from).is_some() {
        return None;
    }

    let vendor = catalog.by_domain(from)?;

    Some(format!(
        "{} ({}, {})",
        vendor.name, vendor.id, vendor.category
    ))
}

fn attach_decision(result: &mut ReadResult, decision: &decide::Decision, pipeline: &Pipeline<'_>) {
    result.decisions = Some(decision.answers.clone());

    result.meta.decider = pipeline.decider.map(|decider| decider.name());
    result.meta.decider_tokens = Some(decision.usage);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_a_date() {
        let parts: Vec<&str> = SCHEMA_VERSION.split('-').collect();

        assert_eq!(parts.len(), 3, "{SCHEMA_VERSION} is not YYYY-MM-DD");

        let year = parts[0];

        assert_eq!(year.len(), 4, "{SCHEMA_VERSION} has no four-digit year");

        for part in &parts {
            let is_number = part.chars().all(|c| c.is_ascii_digit());

            assert!(is_number, "{part} in {SCHEMA_VERSION} is not a number");
        }
    }
}
