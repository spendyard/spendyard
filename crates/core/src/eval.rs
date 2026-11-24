//! Scoring results against expected answers.
//!
//! An expected answer is a flat JSON object next to the document
//! (`invoice.pdf.expected.json`), easy to read and correct by hand:
//!
//! ```json
//! {
//!   "reviewed": true,
//!   "about_money": true,
//!   "type": "receipt",
//!   "status": "paid",
//!   "vendor_id": "slack.com",
//!   "vendor_name": "Slack",
//!   "total": "173.25",
//!   "currency": "EUR",
//!   "last4": ["8239"],
//!   "billing_model": "subscription",
//!   "interval": "month",
//!   "invoice_number": "SBIE-3281007",
//!   "receipt_number": null,
//!   "seats": 21
//! }
//! ```
//!
//! A field that is left out isn't scored. A field set to `null` (or `[]`)
//! means "the document doesn't have one", and an answer is then wrong.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::extract::schema::LineKind;
use crate::money;
use crate::result::ReadResult;

/// Keys in an expected file that describe it rather than the document.
const META_KEYS: &[&str] = &["reviewed", "labelled_by", "notes"];

/// Document types that are about money: worth extracting.
const MONEY_TYPES: &[&str] = &[
    "receipt",
    "invoice",
    "renewal_notice",
    "payment_failed",
    "credit_note",
];

/// The fields scored, in report order.
pub const FIELDS: &[&str] = &[
    "about_money",
    "type",
    "status",
    "vendor_id",
    "vendor_name",
    "total",
    "currency",
    "last4",
    "billing_model",
    "interval",
    "invoice_number",
    "receipt_number",
    "seats",
];

/// How one field compared.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Right,
    Wrong { expected: Value, got: Value },
}

impl Outcome {
    pub fn is_right(&self) -> bool {
        matches!(self, Outcome::Right)
    }
}

/// The answer as the flat fields an expected file uses.
pub fn flatten(result: &ReadResult) -> Map<String, Value> {
    let mut fields = Map::new();

    let word = |value: Value| value.as_str().map(str::to_string);

    let kind =
        word(serde_json::to_value(result.document.kind).unwrap_or_default()).unwrap_or_default();

    let status =
        word(serde_json::to_value(result.document.status).unwrap_or_default()).unwrap_or_default();

    let extracted = result.meta.extractor.is_some();

    let about_money = extracted && MONEY_TYPES.contains(&kind.as_str());

    fields.insert("about_money".to_string(), json!(about_money));
    fields.insert("type".to_string(), json!(kind));
    fields.insert("status".to_string(), json!(status));

    let vendor = result.vendor.as_ref();

    fields.insert(
        "vendor_id".to_string(),
        json!(vendor.and_then(|vendor| vendor.id.clone())),
    );
    fields.insert(
        "vendor_name".to_string(),
        json!(vendor.map(|vendor| vendor.name.clone())),
    );

    let amount = result.amount.as_ref();

    fields.insert(
        "total".to_string(),
        json!(amount.and_then(|amount| amount.total.clone())),
    );
    fields.insert(
        "currency".to_string(),
        json!(amount.and_then(|amount| amount.currency.clone())),
    );

    let last4: Vec<String> = result
        .payments
        .iter()
        .filter_map(|payment| payment.last4.clone())
        .collect();

    fields.insert("last4".to_string(), json!(last4));

    let billing = result.billing.as_ref();

    let billing_model =
        billing.and_then(|billing| word(serde_json::to_value(billing.model).unwrap_or_default()));

    let interval = billing
        .and_then(|billing| billing.interval)
        .and_then(|interval| word(serde_json::to_value(interval).unwrap_or_default()));

    fields.insert("billing_model".to_string(), json!(billing_model));
    fields.insert("interval".to_string(), json!(interval));

    fields.insert("invoice_number".to_string(), json!(result.invoice_number));
    fields.insert("receipt_number".to_string(), json!(result.receipt_number));

    let seat_lines: Vec<f64> = result
        .lines
        .iter()
        .filter(|line| line.kind == LineKind::Seat)
        .filter_map(|line| line.quantity)
        .collect();

    let seats = (!seat_lines.is_empty()).then(|| seat_lines.iter().sum::<f64>());

    fields.insert("seats".to_string(), json!(seats));

    fields
}

/// A draft expected answer from a result, for a person to review.
pub fn draft_expected(result: &ReadResult, labelled_by: &str) -> Value {
    let mut fields = flatten(result);

    fields.insert("reviewed".to_string(), json!(false));
    fields.insert("labelled_by".to_string(), json!(labelled_by));

    // Not about money: only the routing and type are worth checking.
    if fields["about_money"] == json!(false) {
        fields.retain(|key, _| {
            ["reviewed", "labelled_by", "about_money", "type"].contains(&key.as_str())
        });
    }

    Value::Object(fields)
}

/// Scores `result` against `expected`, field by field. Fields the expected
/// file leaves out aren't scored.
pub fn score(expected: &Map<String, Value>, result: &ReadResult) -> BTreeMap<String, Outcome> {
    let actual = flatten(result);

    let mut outcomes = BTreeMap::new();

    for (field, want) in expected {
        if META_KEYS.contains(&field.as_str()) {
            continue;
        }

        let got = actual.get(field).cloned().unwrap_or(Value::Null);

        let outcome = if same(field, want, &got) {
            Outcome::Right
        } else {
            Outcome::Wrong {
                expected: want.clone(),
                got,
            }
        };

        outcomes.insert(field.clone(), outcome);
    }

    outcomes
}

/// Whether two values for `field` mean the same thing.
fn same(field: &str, expected: &Value, got: &Value) -> bool {
    // "unknown" says no more than leaving the field out.
    let unknown = |value: &Value| field == "billing_model" && value == &json!("unknown");

    if unknown(expected) || unknown(got) {
        let expected_empty = is_empty(expected) || unknown(expected);
        let got_empty = is_empty(got) || unknown(got);

        return expected_empty && got_empty;
    }

    if is_empty(expected) || is_empty(got) {
        return is_empty(expected) && is_empty(got);
    }

    match field {
        "total" => {
            let parse = |value: &Value| value.as_str().and_then(money::parse);

            parse(expected).is_some() && parse(expected) == parse(got)
        }

        "seats" => match (expected.as_f64(), got.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() < 0.01,
            _ => false,
        },

        "last4" => {
            let set = |value: &Value| -> Vec<String> {
                let mut items: Vec<String> = value
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                items.sort();
                items.dedup();

                items
            };

            set(expected) == set(got)
        }

        // Names differ in suffixes: "Slack" and "Slack Technologies Limited".
        "vendor_name" => {
            let a = letters(expected);
            let b = letters(got);

            !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
        }

        "invoice_number" | "receipt_number" | "vendor_id" | "currency" | "type" | "status"
        | "billing_model" | "interval" => letters(expected) == letters(got),

        _ => expected == got,
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Letters and digits only, lowercased.
fn letters(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };

    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Per-field tallies over many documents.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tally {
    pub right: BTreeMap<String, usize>,
    pub scored: BTreeMap<String, usize>,
}

impl Tally {
    pub fn add(&mut self, outcomes: &BTreeMap<String, Outcome>) {
        for (field, outcome) in outcomes {
            *self.scored.entry(field.clone()).or_default() += 1;

            if outcome.is_right() {
                *self.right.entry(field.clone()).or_default() += 1;
            }
        }
    }

    /// `(right, scored)` for a field.
    pub fn get(&self, field: &str) -> (usize, usize) {
        let right = self.right.get(field).copied().unwrap_or(0);
        let scored = self.scored.get(field).copied().unwrap_or(0);

        (right, scored)
    }
}

/// Dollars per million input and output tokens, for the models whose
/// prices are known. Prices change; these are list prices as of 2026-11.
pub fn price_per_million(model: &str) -> Option<(f64, f64)> {
    let price = match model {
        "claude-haiku-5-5" => (0.10, 0.50),
        "claude-sonnet-5-5" | "claude-sonnet-5" => (2.0, 10.0),
        "claude-opus-5-5" => (4.0, 20.0),
        "claude-opus-5" => (5.0, 25.0),
        "clef-flash" => (0.038, 0.0),
        _ => return None,
    };

    Some(price)
}

/// What a result cost, when the prices of its models are known.
pub fn cost(result: &ReadResult) -> Option<f64> {
    let mut total = 0.0;

    if let (Some(decider), Some(tokens)) = (&result.meta.decider, result.meta.decider_tokens) {
        let model = decider.rsplit('/').next().unwrap_or(decider);

        let (input, output) = price_per_million(model)?;

        total += tokens.input as f64 / 1e6 * input + tokens.output as f64 / 1e6 * output;
    }

    if let Some(model) = &result.meta.model {
        let (input, output) = price_per_million(model)?;

        let tokens = result.meta.tokens;

        total += tokens.input as f64 / 1e6 * input + tokens.output as f64 / 1e6 * output;
    }

    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn compares_fields_loosely_where_it_should() {
        assert!(same("total", &json!("173.25"), &json!("173.250")));
        assert!(!same("total", &json!("173.25"), &json!("173.52")));
        assert!(same(
            "vendor_name",
            &json!("Slack"),
            &json!("Slack Technologies Limited")
        ));
        assert!(same(
            "invoice_number",
            &json!("PENNYSYR-0096"),
            &json!("pennysyr 0096")
        ));
        assert!(same(
            "last4",
            &json!(["4242", "8239"]),
            &json!(["8239", "4242"])
        ));
        assert!(same("seats", &json!(21), &json!(21.0)));
        assert!(same("interval", &json!(null), &json!(null)));
        assert!(!same("interval", &json!(null), &json!("month")));
        assert!(!same("last4", &json!([]), &json!(["4242"])));
        assert!(same("billing_model", &json!("unknown"), &json!(null)));
        assert!(!same("billing_model", &json!("unknown"), &json!("usage")));
    }

    #[test]
    fn left_out_fields_are_not_scored() {
        let result =
            crate::result::not_about_money(crate::extract::schema::DocumentType::Other, 0.97);

        let expected = map(json!({ "reviewed": true, "about_money": false, "type": "other" }));

        let outcomes = score(&expected, &result);

        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.values().all(Outcome::is_right));
    }

    #[test]
    fn a_skipped_receipt_is_wrong() {
        let result =
            crate::result::not_about_money(crate::extract::schema::DocumentType::Other, 0.9);

        let expected = map(json!({ "about_money": true, "total": "10.00" }));

        let outcomes = score(&expected, &result);

        assert!(!outcomes["about_money"].is_right());
        assert!(!outcomes["total"].is_right());
    }
}
