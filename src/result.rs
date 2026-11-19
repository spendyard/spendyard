//! The result of reading one document (§6 of the design): the model's
//! answer, normalized, with derived fields, checked evidence, flags and
//! metadata.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::document::Document;
use crate::extract::Usage;
use crate::extract::schema::{
    Amount, BillingModel, DocumentType, Interval, Line, LineKind, ModelOutput, Payment, Status,
};
use crate::money;

#[derive(Debug, Clone, Serialize)]
pub struct ReadResult {
    pub document: DocumentSummary,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<Vendor>,

    pub payments: Vec<Payment>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<Amount>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing: Option<Billing>,

    pub lines: Vec<Line>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_number: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub issued: Option<String>,

    /// The quote behind each value, keyed by field path, and whether it was
    /// found in the document.
    pub evidence: BTreeMap<String, Evidence>,

    /// What the checks found wrong or doubtful. Empty is good.
    pub flags: Vec<Flag>,

    /// Problems reading the input, such as parts that couldn't be sent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,

    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentSummary {
    #[serde(rename = "type")]
    pub kind: DocumentType,

    /// Money was paid: a receipt, or a paid invoice.
    pub is_receipt: bool,

    pub is_invoice: bool,

    pub status: Status,

    /// The label of the part the details came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    /// The model's own confidence. Not calibrated.
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Vendor {
    /// The vendor's domain, the key the store and the cloud group by. The
    /// vendor catalog makes this canonical later (M6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor: Option<String>,

    #[serde(skip_serializing_if = "Account::is_empty")]
    pub account: Account,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Account {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
}

impl Account {
    fn is_empty(&self) -> bool {
        self.email.is_none() && self.workspace.is_none() && self.customer_id.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Billing {
    /// Worked out from the lines when there are any; otherwise the model's
    /// reading.
    pub model: BillingModel,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<Interval>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_renewal: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Period {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub quote: String,
    pub part: u32,

    /// The quote was found in the document's text. False when it wasn't,
    /// or when the part has no text to check against (an image).
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flag {
    /// A stable code, such as `totals_dont_add_up`.
    pub code: &'static str,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,

    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub spendyard_version: &'static str,
    pub schema_version: &'static str,

    /// `provider/model` as configured.
    pub extractor: String,

    /// The model that answered.
    pub model: String,

    pub tokens: Usage,
    pub ms: u64,
}

/// Puts the result together from the model's answer. Validation runs
/// afterwards and adds flags.
pub fn assemble(
    document: &Document,
    output: ModelOutput,
    extractor: String,
    model: String,
    tokens: Usage,
    ms: u64,
) -> ReadResult {
    let is_invoice = output.document_type == DocumentType::Invoice;

    let is_receipt = output.document_type == DocumentType::Receipt
        || (is_invoice && output.status == Status::Paid);

    let source = output
        .source_part
        .and_then(|index| document.parts.get(index as usize))
        .map(|part| part.label.clone());

    let summary = DocumentSummary {
        kind: output.document_type,
        is_receipt,
        is_invoice,
        status: output.status,
        source,
        confidence: output.confidence.clamp(0.0, 1.0),
    };

    let lines: Vec<Line> = output.lines.into_iter().map(normalize_line).collect();

    let billing = billing(output.billing, &lines);

    let evidence = output
        .evidence
        .into_iter()
        .map(|item| {
            let evidence = Evidence {
                quote: item.quote,
                part: item.part,
                checked: false,
            };

            (item.field, evidence)
        })
        .collect();

    ReadResult {
        document: summary,
        vendor: vendor(output.vendor),
        payments: output.payments.into_iter().map(normalize_payment).collect(),
        amount: normalize_amount(output.amount),
        billing,
        lines,
        invoice_number: present(output.invoice_number),
        issued: present(output.issued),
        evidence,
        flags: Vec::new(),
        warnings: document.warnings.clone(),
        meta: Meta {
            spendyard_version: crate::VERSION,
            schema_version: crate::SCHEMA_VERSION,
            extractor,
            model,
            tokens,
            ms,
        },
    }
}

/// Subscription lines only → subscription; usage only → usage; both →
/// mixed. `None` when the lines don't say.
pub fn billing_model_from_lines(lines: &[Line]) -> Option<BillingModel> {
    let recurring = lines
        .iter()
        .any(|line| matches!(line.kind, LineKind::Seat | LineKind::Plan));

    let usage = lines.iter().any(|line| line.kind == LineKind::Usage);

    let one_time = lines.iter().any(|line| line.kind == LineKind::OneTime);

    match (recurring, usage, one_time) {
        (true, true, _) => Some(BillingModel::Mixed),
        (true, false, _) => Some(BillingModel::Subscription),
        (false, true, _) => Some(BillingModel::Usage),
        (false, false, true) => Some(BillingModel::OneTime),
        (false, false, false) => None,
    }
}

fn billing(billing: crate::extract::schema::Billing, lines: &[Line]) -> Option<Billing> {
    let start = present(billing.period_start);
    let end = present(billing.period_end);
    let next_renewal = present(billing.next_renewal);

    let has_dates = start.is_some() || end.is_some();

    let model = billing_model_from_lines(lines).unwrap_or(billing.model);

    let nothing_known = model == BillingModel::Unknown
        && billing.interval.is_none()
        && !has_dates
        && next_renewal.is_none();

    if nothing_known {
        return None;
    }

    let billing = Billing {
        model,
        interval: billing.interval,
        period: has_dates.then_some(Period { start, end }),
        next_renewal,
    };

    Some(billing)
}

/// `None` when the model found no vendor.
fn vendor(vendor: crate::extract::schema::Vendor) -> Option<Vendor> {
    let name = present(Some(vendor.name))?;

    let vendor = Vendor {
        id: present(vendor.domain).as_deref().and_then(vendor_id),
        name,
        processor: present(vendor.processor),
        account: Account {
            email: present(vendor.account_email),
            workspace: present(vendor.account_workspace),
            customer_id: present(vendor.account_customer_id),
        },
    };

    Some(vendor)
}

/// `https://www.Cursor.com/` → `cursor.com`.
fn vendor_id(domain: &str) -> Option<String> {
    let lower = domain.trim().to_lowercase();

    let without_scheme = lower
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let host = without_scheme.split('/').next().unwrap_or_default();

    let id = host.trim_start_matches("www.");

    (!id.is_empty() && id.contains('.')).then(|| id.to_string())
}

/// Small models sometimes write "null", "none" or "n/a" as text instead of
/// leaving a value out.
fn present(value: Option<String>) -> Option<String> {
    let text = value?;

    let trimmed = text.trim();

    let empty = trimmed.is_empty()
        || ["null", "none", "n/a", "na", "unknown", "-"]
            .iter()
            .any(|word| trimmed.eq_ignore_ascii_case(word));

    (!empty).then(|| trimmed.to_string())
}

/// Amounts that can't be parsed are kept as written; validation flags them.
fn normalize_money(value: Option<String>) -> Option<String> {
    present(value).map(|text| money::normalize(&text).unwrap_or(text))
}

/// `None` when the model found no amounts.
fn normalize_amount(amount: Amount) -> Option<Amount> {
    let amount = Amount {
        total: normalize_money(amount.total),
        subtotal: normalize_money(amount.subtotal),
        tax: normalize_money(amount.tax),
        currency: present(amount.currency).map(|code| code.to_uppercase()),
    };

    let empty = amount.total.is_none()
        && amount.subtotal.is_none()
        && amount.tax.is_none()
        && amount.currency.is_none();

    (!empty).then_some(amount)
}

fn normalize_payment(payment: Payment) -> Payment {
    Payment {
        brand: present(payment.brand).map(|brand| brand.to_lowercase()),
        last4: present(payment.last4),
        amount: normalize_money(payment.amount),
        ..payment
    }
}

fn normalize_line(line: Line) -> Line {
    Line {
        product: present(line.product),
        plan: present(line.plan),
        unit: present(line.unit),
        unit_price: normalize_money(line.unit_price),
        amount: normalize_money(line.amount),
        ..line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind) -> Line {
        Line {
            kind,
            ..Line::default()
        }
    }

    #[test]
    fn billing_model_comes_from_lines() {
        assert_eq!(
            billing_model_from_lines(&[line(LineKind::Seat), line(LineKind::Usage)]),
            Some(BillingModel::Mixed)
        );

        assert_eq!(
            billing_model_from_lines(&[line(LineKind::Plan), line(LineKind::Discount)]),
            Some(BillingModel::Subscription)
        );

        assert_eq!(billing_model_from_lines(&[line(LineKind::Other)]), None);
    }

    #[test]
    fn words_for_nothing_are_nothing() {
        assert_eq!(present(Some("null".to_string())), None);
        assert_eq!(present(Some(" N/A ".to_string())), None);
        assert_eq!(
            present(Some(" Stripe ".to_string())).as_deref(),
            Some("Stripe")
        );
    }

    #[test]
    fn vendor_ids_are_bare_domains() {
        assert_eq!(
            vendor_id("https://www.Cursor.com/pricing").as_deref(),
            Some("cursor.com")
        );
        assert_eq!(vendor_id("anthropic.com").as_deref(), Some("anthropic.com"));
        assert_eq!(vendor_id("Cursor"), None);
    }
}
