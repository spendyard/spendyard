//! The cheap first model ("system 1"): a decision model such as JEV or
//! Clef answers every multiple-choice and yes/no question about a document
//! in one call, each with a calibrated probability.
//!
//! Decision models choose from options; they never write text. So they
//! decide what a document is and where to look, and the extractor reads
//! names, amounts and dates.

mod answers;
mod questions;

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

pub use answers::parse_answers;
pub use questions::{Question, images_for, questions_for, state_for};

use crate::document::Document;
use crate::extract::{ExtractError, Usage};

/// One answer from a decision model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        /// The most likely option.
        choice: String,

        /// The probability of every option.
        probabilities: BTreeMap<String, f64>,
    },

    YesNo {
        /// The probability of yes.
        yes: f64,
    },
}

impl Answer {
    /// The chosen option and its probability.
    pub fn chosen(&self) -> Option<(&str, f64)> {
        match self {
            Answer::Choice {
                choice,
                probabilities,
            } => {
                let probability = probabilities.get(choice).copied().unwrap_or(0.0);

                Some((choice.as_str(), probability))
            }

            Answer::YesNo { .. } => None,
        }
    }

    /// How sure the model is of its answer: the chosen option's
    /// probability, or the larger of yes and no.
    pub fn certainty(&self) -> f64 {
        match self {
            Answer::Choice { .. } => self.chosen().map(|(_, p)| p).unwrap_or(0.0),
            Answer::YesNo { yes } => yes.max(1.0 - yes),
        }
    }
}

/// Every answer for one document.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Decision {
    pub answers: BTreeMap<String, Answer>,

    #[serde(skip)]
    pub model: String,

    #[serde(skip)]
    pub usage: Usage,
}

impl Decision {
    /// The chosen option for a choice question, with its probability.
    pub fn choice(&self, question: &str) -> Option<(&str, f64)> {
        self.answers.get(question).and_then(Answer::chosen)
    }

    /// The probability of yes for a yes/no question.
    pub fn yes(&self, question: &str) -> Option<f64> {
        match self.answers.get(question) {
            Some(Answer::YesNo { yes }) => Some(*yes),
            _ => None,
        }
    }

    /// A short summary for the extractor, so it knows what to look for.
    pub fn hints(&self) -> String {
        let mut lines = Vec::new();

        for (question, answer) in &self.answers {
            let line = match answer {
                Answer::Choice { .. } => {
                    let (choice, probability) = answer.chosen().unwrap_or(("?", 0.0));

                    format!("{question}: {choice} ({probability:.2})")
                }

                Answer::YesNo { yes } => format!("{question}: yes {yes:.2}"),
            };

            lines.push(line);
        }

        lines.join("\n")
    }
}

/// A model that answers multiple-choice and yes/no questions.
#[async_trait]
pub trait Decider: Send + Sync {
    /// `provider/model`, as reported in results.
    fn name(&self) -> String;

    /// Whether it can look at images (Clef can; JEV is text only).
    fn accepts_images(&self) -> bool;

    async fn decide(
        &self,
        state: &Value,
        questions: &[Question],
        images: &[Image],
    ) -> Result<Decision, ExtractError>;
}

/// An image to look at, for documents without text.
#[derive(Debug, Clone)]
pub struct Image {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Settings for building a decider. Keys are passed in; the library never
/// reads the environment.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// `jev`, `clef`, `clef-flash` or `systemone`.
    pub provider: String,

    pub model: Option<String>,

    pub api_key: Option<String>,

    /// Cloudflare account, for `clef` and `clef-flash`.
    pub account_id: Option<String>,

    pub base_url: Option<String>,
}

/// Every decider's name, for help text and errors.
pub const PROVIDERS: &[&str] = &["jev", "clef", "clef-flash", "systemone"];

/// The environment variable each provider's key is read from.
pub fn key_variable(provider: &str) -> Option<&'static str> {
    match provider {
        "jev" => Some("TYPESAFE_API_KEY"),
        "clef" | "clef-flash" => Some("CLOUDFLARE_API_TOKEN"),
        "systemone" => Some("SYSTEMONE_API_KEY"),
        _ => None,
    }
}

/// How sure the first model must be that a document is not about money
/// before the extractor is skipped.
pub const DEFAULT_THRESHOLD: f64 = 0.85;

/// Asks the first model about `document`. `None` when there's nothing it
/// can read (an image-only document and a text-only model).
pub async fn decide(
    document: &Document,
    decider: &dyn Decider,
) -> Option<Result<Decision, ExtractError>> {
    let state = state_for(document);

    let images = if decider.accepts_images() {
        images_for(document)
    } else {
        Vec::new()
    };

    let has_text = document.parts.iter().any(|part| part.text.is_some());

    if !has_text && images.is_empty() {
        return None;
    }

    let questions = questions_for(document);

    Some(decider.decide(&state, &questions, &images).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(choice: &str, probabilities: &[(&str, f64)]) -> Answer {
        Answer::Choice {
            choice: choice.to_string(),
            probabilities: probabilities
                .iter()
                .map(|(option, p)| (option.to_string(), *p))
                .collect(),
        }
    }

    #[test]
    fn certainty_and_hints() {
        let mut decision = Decision::default();

        decision.answers.insert(
            "document_type".to_string(),
            choice("invoice", &[("invoice", 0.9), ("other", 0.1)]),
        );

        decision
            .answers
            .insert("paid_by_card".to_string(), Answer::YesNo { yes: 0.2 });

        assert_eq!(decision.choice("document_type"), Some(("invoice", 0.9)));
        assert_eq!(decision.yes("paid_by_card"), Some(0.2));
        assert!((decision.answers["paid_by_card"].certainty() - 0.8).abs() < 1e-9);

        let hints = decision.hints();

        assert!(hints.contains("document_type: invoice (0.90)"));
        assert!(hints.contains("paid_by_card: yes 0.20"));
    }
}
