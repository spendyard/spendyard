//! A decision model's answers, as JEV's response format has them:
//! `{ model, answers, usage }`, or wrapped in Cloudflare's
//! `{ result, success }`. Every decider answers in it, so reading them is
//! the core's: the providers (spendyard-models), the CLI's `exec` and the
//! cloud's Worker all come here.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Answer, Decision};
use crate::extract::{ExtractError, Usage};

/// Reads answers in the JEV response format: SystemOne's, Clef's over Workers AI
/// and the CLI's `exec` provider all answer in it.
pub fn parse_answers(response: &Value, asked_for: &str) -> Result<Decision, ExtractError> {
    // Cloudflare's REST API wraps results in { "result": …, "success": … }.
    let body = match response.get("result") {
        Some(result) if result.get("answers").is_some() => result,
        _ => response,
    };

    let Some(raw_answers) = body["answers"].as_object() else {
        return Err(ExtractError::InvalidOutput(
            "no answers in the response".to_string(),
        ));
    };

    let mut answers = BTreeMap::new();

    for (name, raw) in raw_answers {
        let answer = match raw["type"].as_str() {
            Some("choice") => {
                let probabilities: BTreeMap<String, f64> = raw["probabilities"]
                    .as_object()
                    .map(|map| {
                        map.iter()
                            .filter_map(|(option, p)| p.as_f64().map(|p| (option.clone(), p)))
                            .collect()
                    })
                    .unwrap_or_default();

                let choice = raw["choice"].as_str().unwrap_or_default().to_string();

                Answer::Choice {
                    choice,
                    probabilities,
                }
            }

            Some("noul") => Answer::YesNo {
                yes: raw["noul"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0),
            },

            _ => continue,
        };

        answers.insert(name.clone(), answer);
    }

    let usage = Usage {
        input: body
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: body
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };

    let decision = Decision {
        answers,
        model: body["model"].as_str().unwrap_or(asked_for).to_string(),
        usage,
    };

    Ok(decision)
}
