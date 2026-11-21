//! The rows a store keeps from a result: `receipts`, `lines`, `payments` and
//! a card statement's `charges`, the columns reports use (design §8). Worked
//! out here rather than where they're written, so the CLI's store
//! (`~/.spendyard/spendyard.db`) and Spendyard Cloud's (one per
//! organisation, the same tables) fill them the same way, and a push moves
//! rows that mean the same on both sides.
//!
//! Field names are the columns'; values are what's written, words for enums
//! included.

use serde::Serialize;

use crate::catalog::catalog;
use crate::extract::schema::LineKind;
use crate::result::ReadResult;

/// A document's `receipts` row: its newest result, in the columns reports use.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceiptRow {
    pub r#type: Option<String>,
    pub status: Option<String>,
    pub is_receipt: bool,
    pub is_invoice: bool,
    pub vendor_id: Option<String>,
    pub vendor_name: Option<String>,
    pub processor: Option<String>,
    pub account_email: Option<String>,
    pub account_workspace: Option<String>,

    /// When it was issued, or the day it arrived when it doesn't say.
    pub date: Option<String>,

    pub total: Option<String>,
    pub tax: Option<String>,
    pub currency: Option<String>,
    pub billing_model: Option<String>,
    pub interval: Option<String>,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    pub next_renewal: Option<String>,
    pub invoice_number: Option<String>,
    pub receipt_number: Option<String>,

    /// The first payment that names a card.
    pub card_brand: Option<String>,
    pub card_last4: Option<String>,

    /// Seats paid for: the seat lines' quantities added up.
    pub seats: Option<f64>,

    /// How many flags the result has.
    pub flags: i64,
}

/// One of a document's `lines`, at its position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineRow {
    pub position: i64,
    pub kind: Option<String>,
    pub product: Option<String>,
    pub plan: Option<String>,
    pub description: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub unit_price: Option<String>,
    pub amount: Option<String>,
    pub interval: Option<String>,
}

/// One of a document's `payments`, at its position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaymentRow {
    pub position: i64,
    pub method: Option<String>,
    pub brand: Option<String>,
    pub last4: Option<String>,
    pub amount: Option<String>,
}

/// One of a card statement's `charges`, at its position. Whether another
/// document has it already is the store's to decide (the same charge on two
/// statements is kept once).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeRow {
    pub position: i64,
    pub issuer: Option<String>,
    pub row: Option<u32>,
    pub date: String,
    pub posted: Option<String>,
    pub merchant: String,
    pub card_last4: Option<String>,
    pub name_on_card: Option<String>,
    pub amount: String,

    /// The statement's currency.
    pub currency: Option<String>,

    pub original_amount: Option<String>,
    pub original_currency: Option<String>,
    pub rate: Option<String>,
    pub kind: String,

    /// The vendor the catalog knows the merchant as, if any.
    pub vendor_id: Option<String>,
    pub vendor_name: Option<String>,
}

/// Everything a result puts in a store, but the result itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rows {
    pub receipt: ReceiptRow,
    pub lines: Vec<LineRow>,
    pub payments: Vec<PaymentRow>,

    /// A card statement's charges; `None` when it isn't one, so the store
    /// leaves the document's charges as they were.
    pub charges: Option<Vec<ChargeRow>>,
}

/// An enum's word, as it serializes: `"seat"`, `"monthly"`.
fn word<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// The rows `result` puts in a store. `fallback_date` is when the document
/// arrived (an email's date, a file's), for a result that doesn't say when it
/// was issued; only its day is kept.
pub fn rows(result: &ReadResult, fallback_date: Option<&str>) -> Rows {
    Rows {
        receipt: receipt_row(result, fallback_date),
        lines: line_rows(result),
        payments: payment_rows(result),
        charges: charge_rows(result),
    }
}

/// The `receipts` row.
pub fn receipt_row(result: &ReadResult, fallback_date: Option<&str>) -> ReceiptRow {
    let vendor = result.vendor.as_ref();
    let amount = result.amount.as_ref();
    let billing = result.billing.as_ref();
    let period = billing.and_then(|billing| billing.period.as_ref());

    let card = result
        .payments
        .iter()
        .find(|payment| payment.last4.is_some());

    let date = result
        .issued
        .clone()
        .or_else(|| fallback_date.map(|date| date.chars().take(10).collect()));

    let seat_quantities: Vec<f64> = result
        .lines
        .iter()
        .filter(|line| line.kind == LineKind::Seat)
        .filter_map(|line| line.quantity)
        .collect();

    let seats = (!seat_quantities.is_empty()).then(|| seat_quantities.iter().sum());

    ReceiptRow {
        r#type: word(&result.document.kind),
        status: word(&result.document.status),
        is_receipt: result.document.is_receipt,
        is_invoice: result.document.is_invoice,
        vendor_id: vendor.and_then(|vendor| vendor.id.clone()),
        vendor_name: vendor.map(|vendor| vendor.name.clone()),
        processor: vendor.and_then(|vendor| vendor.processor.clone()),
        account_email: vendor.and_then(|vendor| vendor.account.email.clone()),
        account_workspace: vendor.and_then(|vendor| vendor.account.workspace.clone()),
        date,
        total: amount.and_then(|amount| amount.total.clone()),
        tax: amount.and_then(|amount| amount.tax.clone()),
        currency: amount.and_then(|amount| amount.currency.clone()),
        billing_model: billing.and_then(|billing| word(&billing.model)),
        interval: billing
            .and_then(|billing| billing.interval)
            .and_then(|interval| word(&interval)),
        period_start: period.and_then(|period| period.start.clone()),
        period_end: period.and_then(|period| period.end.clone()),
        next_renewal: billing.and_then(|billing| billing.next_renewal.clone()),
        invoice_number: result.invoice_number.clone(),
        receipt_number: result.receipt_number.clone(),
        card_brand: card.and_then(|payment| payment.brand.clone()),
        card_last4: card.and_then(|payment| payment.last4.clone()),
        seats,
        flags: result.flags.len() as i64,
    }
}

/// The `lines` rows, in the result's order.
pub fn line_rows(result: &ReadResult) -> Vec<LineRow> {
    result
        .lines
        .iter()
        .enumerate()
        .map(|(position, line)| LineRow {
            position: position as i64,
            kind: word(&line.kind),
            product: line.product.clone(),
            plan: line.plan.clone(),
            description: line.description.clone(),
            quantity: line.quantity,
            unit: line.unit.clone(),
            unit_price: line.unit_price.clone(),
            amount: line.amount.clone(),
            interval: line.interval.and_then(|interval| word(&interval)),
        })
        .collect()
}

/// The `payments` rows, in the result's order.
pub fn payment_rows(result: &ReadResult) -> Vec<PaymentRow> {
    result
        .payments
        .iter()
        .enumerate()
        .map(|(position, payment)| PaymentRow {
            position: position as i64,
            method: word(&payment.method),
            brand: payment.brand.clone(),
            last4: payment.last4.clone(),
            amount: payment.amount.clone(),
        })
        .collect()
}

/// A card statement's `charges` rows, each with the vendor the catalog knows
/// its merchant as; `None` when the result isn't a statement.
pub fn charge_rows(result: &ReadResult) -> Option<Vec<ChargeRow>> {
    let statement = result.statement.as_ref()?;

    let catalog = catalog();

    let rows = statement
        .charges
        .iter()
        .enumerate()
        .map(|(position, charge)| {
            let vendor = catalog.by_merchant(&charge.merchant);

            let kind = word(&charge.kind).unwrap_or_else(|| "purchase".to_string());

            ChargeRow {
                position: position as i64,
                issuer: statement.issuer.clone(),
                row: charge.row,
                date: charge.date.clone(),
                posted: charge.posted.clone(),
                merchant: charge.merchant.clone(),
                card_last4: charge.card_last4.clone(),
                name_on_card: charge.name_on_card.clone(),
                amount: charge.amount.clone(),
                currency: statement.currency.clone(),
                original_amount: charge.original_amount.clone(),
                original_currency: charge.original_currency.clone(),
                rate: charge.rate.clone(),
                kind,
                vendor_id: vendor.map(|vendor| vendor.id.clone()),
                vendor_name: vendor.map(|vendor| vendor.name.clone()),
            }
        })
        .collect();

    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::schema::{DocumentType, Line, Payment};
    use crate::result::not_about_money;
    use crate::statement::{ChargeOutput, StatementOutput, assemble};

    #[test]
    fn a_receipt_without_a_date_takes_the_day_it_arrived() {
        let result = not_about_money(DocumentType::Other, 0.9);

        let rows = rows(&result, Some("2026-11-21T09:14:00Z"));

        assert_eq!(rows.receipt.date.as_deref(), Some("2026-11-21"));
        assert_eq!(rows.receipt.r#type.as_deref(), Some("other"));
        assert_eq!(rows.charges, None);
    }

    #[test]
    fn seats_add_up_and_the_card_comes_from_the_first_payment_naming_one() {
        let mut result = not_about_money(DocumentType::Other, 0.9);

        let seat = |quantity: f64| Line {
            kind: LineKind::Seat,
            quantity: Some(quantity),
            description: "Pro seat".to_string(),
            ..Line::default()
        };

        result.lines = vec![seat(3.0), seat(2.0), Line::default()];

        result.payments = vec![
            Payment::default(),
            Payment {
                brand: Some("visa".to_string()),
                last4: Some("3776".to_string()),
                ..Payment::default()
            },
        ];

        let rows = rows(&result, None);

        assert_eq!(rows.receipt.seats, Some(5.0));
        assert_eq!(rows.receipt.card_last4.as_deref(), Some("3776"));
        assert_eq!(rows.receipt.card_brand.as_deref(), Some("visa"));
        assert_eq!(rows.lines.len(), 3);
        assert_eq!(rows.lines[1].position, 1);
        assert_eq!(rows.lines[0].kind.as_deref(), Some("seat"));
        assert_eq!(rows.payments[1].position, 1);
    }

    #[test]
    fn a_statement_gives_charges_in_its_currency() {
        let mut result = not_about_money(DocumentType::CardStatement, 0.9);

        let charge = ChargeOutput {
            row: Some(1),
            date: "2026-04-14".to_string(),
            merchant: "VERCEL INC., COVINA, US".to_string(),
            card_last4: Some("3776".to_string()),
            amount: "192.07".to_string(),
            ..ChargeOutput::default()
        };

        result.statement = Some(assemble(vec![StatementOutput {
            is_card_statement: true,
            currency: Some("NOK".to_string()),
            charges: vec![charge],
            ..StatementOutput::default()
        }]));

        let charges = rows(&result, None).charges.unwrap();

        assert_eq!(charges.len(), 1);
        assert_eq!(charges[0].kind, "purchase");
        assert_eq!(charges[0].currency.as_deref(), Some("NOK"));
        assert_eq!(charges[0].amount, "192.07");
    }
}
