//! What the model is asked to return, and the JSON Schema that holds it to
//! that shape.
//!
//! This is the model's side only. The final result (§6 of the design) is
//! put together from it in `result.rs`, with derived fields, checked
//! evidence, flags and metadata added.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Everything the extractor reads from one document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelOutput {
    /// What kind of document this is.
    pub document_type: DocumentType,

    /// Whether money has moved, is due, failed to move or will move.
    pub status: Status,

    /// Index of the part the details were read from, as in `<part index=…>`.
    pub source_part: Option<u32>,

    /// How sure you are of the whole reading, from 0 to 1.
    pub confidence: f64,

    pub vendor: Option<Vendor>,

    /// How it was paid. Empty when the document doesn't say.
    pub payments: Vec<Payment>,

    pub amount: Option<Amount>,

    pub billing: Option<Billing>,

    /// Every line item, including discounts and credits. Tax is not a line.
    pub lines: Vec<Line>,

    /// The invoice or receipt number.
    pub invoice_number: Option<String>,

    /// The date the document was issued, as YYYY-MM-DD.
    pub issued: Option<String>,

    /// A quote from the document for every value you filled in.
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    /// Proof that a payment was made.
    Receipt,

    /// A request for payment, paid or not.
    Invoice,

    /// Says a charge is coming. Future tense; nothing has been paid.
    RenewalNotice,

    /// A payment that didn't go through.
    PaymentFailed,

    /// A price offer.
    Quote,

    /// A trial is about to end.
    TrialEnding,

    /// Anything else, such as a newsletter or a login code.
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Paid,
    Due,
    Failed,
    Upcoming,

    #[default]
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vendor {
    /// The company you pay, not the payment processor.
    pub name: String,

    /// The vendor's own website domain, such as `cursor.com`.
    pub domain: Option<String>,

    /// The payment processor that sent or handled the receipt, such as
    /// Stripe, Paddle, Lemon Squeezy, FastSpring, Chargebee, Apple or
    /// Google Play. Null when the vendor bills directly.
    pub processor: Option<String>,

    /// The billing email the vendor has on file for the customer.
    pub account_email: Option<String>,

    /// The workspace, team or organization name the subscription is for.
    pub account_workspace: Option<String>,

    /// The vendor's customer or account ID.
    pub account_customer_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Payment {
    pub method: PaymentMethod,

    /// Card brand in lowercase, such as `visa` or `mastercard`.
    pub brand: Option<String>,

    /// The last four digits of the card or account.
    pub last4: Option<String>,

    /// The amount paid this way, as a plain decimal such as `112.40`.
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    Card,
    Paypal,
    BankTransfer,
    DirectDebit,
    ApplePay,
    GooglePay,
    Credits,
    Invoice,

    #[default]
    Other,
}

/// All amounts are plain decimals with a dot and no thousands separators:
/// `1234.50`, never `1 234,50` or `$1,234.50`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Amount {
    /// The amount paid or due, tax included.
    pub total: Option<String>,

    /// Before tax.
    pub subtotal: Option<String>,

    /// VAT, GST or sales tax.
    pub tax: Option<String>,

    /// ISO 4217 code, such as `USD`, `EUR` or `NOK`.
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Billing {
    /// How the vendor charges, as far as this document shows.
    pub model: BillingModel,

    pub interval: Option<Interval>,

    /// Start of the billed period, as YYYY-MM-DD.
    pub period_start: Option<String>,

    /// End of the billed period, as YYYY-MM-DD.
    pub period_end: Option<String>,

    /// When it renews or is charged next, as YYYY-MM-DD.
    pub next_renewal: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingModel {
    /// A recurring fee: plans and seats.
    Subscription,

    /// Charged for what was used.
    Usage,

    /// Both subscription and usage charges.
    Mixed,

    OneTime,

    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    Month,
    Year,
    Week,
    Quarter,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Line {
    pub kind: LineKind,

    /// The product, such as `Jira` or `Cursor`.
    pub product: Option<String>,

    /// The plan or tier, such as `Pro` or `Standard`.
    pub plan: Option<String>,

    /// The line as written on the document.
    pub description: String,

    /// Number of seats, units or requests.
    pub quantity: Option<f64>,

    /// What the quantity counts, such as `seat`, `request` or `1M tokens`.
    pub unit: Option<String>,

    /// Price per unit, as a plain decimal.
    pub unit_price: Option<String>,

    /// The line's amount, as a plain decimal. Negative for discounts and
    /// credits.
    pub amount: Option<String>,

    pub interval: Option<Interval>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Priced per seat or user.
    Seat,

    /// A flat recurring fee for a plan.
    Plan,

    /// Charged by use: requests, tokens, minutes, overage.
    Usage,

    Addon,
    Discount,
    Credit,
    OneTime,

    #[default]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// The field, as a path: `amount.total`, `vendor.name`,
    /// `payments[0].last4`, `lines[1].unit_price`.
    pub field: String,

    /// Text copied exactly from the document that shows the value.
    pub quote: String,

    /// Index of the part the quote is from.
    pub part: u32,
}

/// The JSON Schema for [`ModelOutput`], in the strict form the providers'
/// structured outputs accept: every object closed, every property listed
/// as required (optional ones are nullable), no `$ref`s, no numeric or
/// format constraints.
pub fn model_output_schema() -> Value {
    let mut settings = SchemaSettings::draft2020_12();

    settings.inline_subschemas = true;

    let schema = settings
        .into_generator()
        .into_root_schema_for::<ModelOutput>();

    let mut value = schema.to_value();

    strictify(&mut value);

    value
}

/// Keywords structured outputs reject or that add nothing for the model.
const DROPPED_KEYWORDS: &[&str] = &[
    "$schema",
    "title",
    "format",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "default",
];

fn strictify(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for keyword in DROPPED_KEYWORDS {
                object.remove(*keyword);
            }

            if object.contains_key("properties") {
                close_object(object);
            }

            for child in object.values_mut() {
                strictify(child);
            }
        }

        Value::Array(items) => {
            for item in items {
                strictify(item);
            }
        }

        _ => {}
    }
}

fn close_object(object: &mut Map<String, Value>) {
    let names: Vec<Value> = object["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().map(Value::String).collect())
        .unwrap_or_default();

    object.insert("required".to_string(), Value::Array(names));
    object.insert("additionalProperties".to_string(), Value::Bool(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk<'a>(value: &'a Value, found: &mut Vec<&'a Map<String, Value>>) {
        match value {
            Value::Object(object) => {
                found.push(object);

                for child in object.values() {
                    walk(child, found);
                }
            }

            Value::Array(items) => {
                for item in items {
                    walk(item, found);
                }
            }

            _ => {}
        }
    }

    #[test]
    fn every_object_is_closed_and_fully_required() {
        let schema = model_output_schema();

        let mut objects = Vec::new();

        walk(&schema, &mut objects);

        let with_properties: Vec<_> = objects
            .iter()
            .filter(|object| object.contains_key("properties"))
            .collect();

        assert!(
            with_properties.len() >= 7,
            "nested objects were not inlined"
        );

        for object in with_properties {
            let properties = object["properties"].as_object().unwrap();
            let required = object["required"].as_array().unwrap();

            assert_eq!(properties.len(), required.len());
            assert_eq!(object["additionalProperties"], Value::Bool(false));
        }
    }

    #[test]
    fn no_refs_or_constraints_left() {
        let text = model_output_schema().to_string();

        for banned in ["$ref", "$defs", "minimum", "\"format\"", "$schema"] {
            assert!(!text.contains(banned), "{banned} in schema: {text}");
        }
    }

    #[test]
    fn descriptions_reach_the_schema() {
        let text = model_output_schema().to_string();

        assert!(text.contains("not the payment processor"));
        assert!(text.contains("Future tense"));
    }
}
