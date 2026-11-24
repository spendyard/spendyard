//! What a store says about spending, for `spendyard receipts`, `report` and
//! `subscriptions` and for the cloud alike: the rows (one per document read,
//! from [`ROWS_SQL`], which both stores run, theirs being the same tables),
//! vendors grouped through the catalog, what counts as spend, and the
//! recurring vendors with what each costs.
//!
//! Spend counts money paid: receipts and paid invoices, not failed or due
//! ones, not those marked personal or ignored, and each receipt once even
//! when it was forwarded or saved twice.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, name_key};
use crate::dates::shift_date;
use crate::payments::{Piece, group, piece_of};
use crate::rates::{Rates, convert};
use crate::transactions::Matching;

/// The query for the rows, newest first, over the store's documents,
/// receipts and edits: its columns are [`StoredRow`]'s.
pub const ROWS_SQL: &str = "SELECT d.id AS id,
       coalesce(r.date, substr(d.received, 1, 10), '') AS date,
       coalesce(r.type, '') AS kind,
       coalesce(r.status, '') AS status,
       coalesce(r.is_receipt, 0) AS is_receipt,
       r.vendor_id AS vendor_id,
       coalesce(r.vendor_name, '') AS vendor_name,
       e.vendor_name AS edited_vendor_name,
       r.total AS total,
       r.currency AS currency,
       r.billing_model AS billing_model,
       r.interval AS interval,
       r.next_renewal AS next_renewal,
       coalesce(r.invoice_number, r.receipt_number) AS number,
       CASE WHEN r.card_last4 IS NOT NULL THEN coalesce(r.card_brand, 'card') || ' ' || r.card_last4 END AS card,
       r.seats AS seats,
       coalesce(r.flags, 0) AS flags,
       coalesce(e.personal, 0) AS personal,
       coalesce(e.ignored, 0) AS ignored,
       d.name AS subject,
       d.source AS source,
       r.account_workspace AS account_workspace,
       r.account_email AS account_email,
       (SELECT l.product FROM lines l
         WHERE l.document_id = d.id AND l.product IS NOT NULL AND coalesce(l.kind, '') NOT IN ('discount', 'credit')
         ORDER BY abs(coalesce(CAST(l.amount AS REAL), 0)) DESC, l.position
         LIMIT 1) AS product
FROM documents d
JOIN receipts r ON r.document_id = d.id
LEFT JOIN edits e ON e.document_id = d.id
ORDER BY 2 DESC, d.id DESC";

/// One row of [`ROWS_SQL`], as SQLite gives it: flags as 0 and 1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredRow {
    pub id: i64,
    pub date: String,
    pub kind: String,
    pub status: String,
    pub is_receipt: i64,
    pub vendor_id: Option<String>,
    pub vendor_name: String,
    pub edited_vendor_name: Option<String>,
    pub total: Option<String>,
    pub currency: Option<String>,
    pub billing_model: Option<String>,
    pub interval: Option<String>,
    pub next_renewal: Option<String>,
    pub number: Option<String>,
    pub card: Option<String>,
    pub seats: Option<f64>,
    pub flags: i64,
    pub personal: i64,
    pub ignored: i64,
    pub subject: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub account_workspace: Option<String>,
    #[serde(default)]
    pub account_email: Option<String>,
    /// The product of its largest line, not a discount or a credit.
    #[serde(default)]
    pub product: Option<String>,
}

/// One document's row, its vendor grouped.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: i64,
    pub date: String,
    pub kind: String,
    pub status: String,
    pub is_receipt: bool,
    /// What the vendor's rows group by: its catalog ID, else its name
    /// without legal suffixes.
    pub vendor_key: String,
    pub vendor_name: String,
    /// The catalog's category, when the catalog knows the vendor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The vendor's domain: its catalog ID when the catalog knows it, else
    /// the domain its reading gave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_domain: Option<String>,
    pub total: Option<String>,
    pub currency: Option<String>,
    pub billing_model: Option<String>,
    pub interval: Option<String>,
    pub next_renewal: Option<String>,
    pub number: Option<String>,
    pub card: Option<String>,
    pub seats: Option<f64>,
    pub flags: i64,
    pub personal: bool,
    pub ignored: bool,
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The vendor account it's for: its workspace, else its email; as
    /// written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The product of its largest line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// A payment from a statement with no receipt, standing in for one: its
    /// transaction's ID ([`crate::transactions::Charge::key`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment: Option<String>,
}

impl Row {
    /// A stored row with its vendor grouped: an edit wins; then the
    /// catalog, by ID or name; then the name.
    pub fn from_stored(stored: StoredRow, catalog: &Catalog) -> Row {
        let found = match &stored.edited_vendor_name {
            Some(edited) => catalog.find(None, Some(edited)),
            None => catalog.find(stored.vendor_id.as_deref(), Some(&stored.vendor_name)),
        };

        let (vendor_key, vendor_name, category) = match (found, stored.edited_vendor_name) {
            (Some(vendor), None) => (
                vendor.id.clone(),
                vendor.name.clone(),
                Some(vendor.category.clone()),
            ),
            (Some(vendor), Some(edited)) => {
                (vendor.id.clone(), edited, Some(vendor.category.clone()))
            }
            (None, Some(edited)) => (name_key(&edited), edited, None),
            (None, None) => (name_key(&stored.vendor_name), stored.vendor_name, None),
        };

        // The catalog's ID is a domain; else the domain the document shows, when it's one.
        let vendor_domain = found.map(|vendor| vendor.id.clone()).or_else(|| {
            stored
                .vendor_id
                .clone()
                .filter(|id| id.contains('.') && !id.contains(' '))
        });

        Row {
            id: stored.id,
            date: stored.date,
            kind: stored.kind,
            status: stored.status,
            is_receipt: stored.is_receipt != 0,
            vendor_key,
            vendor_name,
            category,
            vendor_domain,
            total: stored.total,
            currency: stored.currency,
            billing_model: stored.billing_model,
            interval: stored.interval,
            next_renewal: stored.next_renewal,
            number: stored.number,
            card: stored.card,
            seats: stored.seats,
            flags: stored.flags,
            personal: stored.personal != 0,
            ignored: stored.ignored != 0,
            subject: stored.subject,
            source: stored.source,
            account: account_of(stored.account_workspace, stored.account_email),
            product: stored.product.filter(|product| !product.trim().is_empty()),
            payment: None,
        }
    }
}

/// The account a document is for: its workspace, else its email; neither
/// when both are empty.
fn account_of(workspace: Option<String>, email: Option<String>) -> Option<String> {
    let given = |value: Option<String>| {
        value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    given(workspace).or_else(|| given(email))
}

/// A stored amount, as a decimal.
pub fn amount(text: &str) -> Option<Decimal> {
    text.parse().ok()
}

/// Paid receipts that count as spend, each payment once
/// ([`crate::payments`]): its receipt (else its first document) with what
/// its other documents say and it doesn't, such as its invoice's interval,
/// seats and next renewal; payments from statements with no receipt as
/// they are. In the rows' order.
pub fn spend(rows: &[Row]) -> Vec<Row> {
    let counted: Vec<&Row> = rows.iter().filter(|row| counts_as_paid(row)).collect();

    let groups = group(rows, &Matching::default());

    let payment_of: BTreeMap<Piece, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(at, evidence)| evidence.pieces().map(move |piece| (piece, at)))
        .collect();

    let mut documents: Vec<Vec<&Row>> = vec![Vec::new(); groups.len()];

    for &row in &counted {
        if let Some(&at) = payment_of.get(&piece_of(row)) {
            documents[at].push(row);
        }
    }

    // Each payment where its chosen row is.
    counted
        .iter()
        .filter_map(|&row| {
            let at = *payment_of.get(&piece_of(row))?;
            let chosen = chosen(&documents[at]);

            std::ptr::eq(chosen, row).then(|| with_facts(chosen, &documents[at]))
        })
        .collect()
}

/// The row that stands for a payment: its first receipt, else its first
/// document (a paid invoice with no receipt).
fn chosen<'a>(documents: &[&'a Row]) -> &'a Row {
    documents
        .iter()
        .find(|row| row.kind != "invoice")
        .unwrap_or(&documents[0])
}

/// `chosen` with what the payment's other documents say and it doesn't: a
/// receipt rarely names the interval, seats or next renewal its invoice
/// does.
fn with_facts(chosen: &Row, documents: &[&Row]) -> Row {
    let mut row = chosen.clone();

    for other in documents {
        fill(&mut row.billing_model, &other.billing_model);
        fill(&mut row.interval, &other.interval);
        fill(&mut row.next_renewal, &other.next_renewal);
        fill(&mut row.seats, &other.seats);
        fill(&mut row.number, &other.number);
        fill(&mut row.card, &other.card);
        fill(&mut row.account, &other.account);
        fill(&mut row.product, &other.product);
    }

    row
}

fn fill<T: Clone>(field: &mut Option<T>, from: &Option<T>) {
    if field.is_none() {
        field.clone_from(from);
    }
}

/// Whether two rows are for the same vendor, amount and currency: what a
/// paid invoice and its receipt share.
pub(crate) fn same_vendor_and_amount(a: &Row, b: &Row) -> bool {
    let total = |row: &Row| row.total.as_deref().and_then(amount);

    a.vendor_key == b.vendor_key && a.currency == b.currency && total(a) == total(b)
}

/// Whether a row is money paid that counts: a receipt or paid invoice, not
/// personal or ignored, with a total above nothing.
pub(crate) fn counts_as_paid(row: &Row) -> bool {
    let positive = row
        .total
        .as_deref()
        .and_then(amount)
        .is_some_and(|total| total > Decimal::ZERO);

    row.is_receipt && !row.personal && !row.ignored && positive
}

/// What one payment's documents share when it was saved twice: its vendor,
/// its number in letters and digits only (`B085EFDD-0038` on the email is
/// `B085EFDD 0038` on its PDF) and its total. `None` with no number.
pub(crate) fn number_key(row: &Row) -> Option<String> {
    let plain: String = row
        .number
        .as_deref()?
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_uppercase();

    if plain.is_empty() {
        return None;
    }

    let total = row.total.as_deref().unwrap_or("");

    Some(format!("{}|{plain}|{total}", row.vendor_key))
}

/// The currency to report in: the one asked for, or the most common one
/// paid in, or USD.
pub fn report_currency(asked: Option<String>, rows: &[Row]) -> String {
    if let Some(currency) = asked.filter(|currency| !currency.is_empty()) {
        return currency.to_uppercase();
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();

    for row in rows {
        if let Some(currency) = &row.currency {
            *counts.entry(currency).or_default() += 1;
        }
    }

    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(currency, _)| currency.to_string())
        .unwrap_or_else(|| "USD".to_string())
}

/// What converting `rows` into `target` needs: the currencies, and rates
/// from which day on (a week before the earliest row, so it has a rate on
/// or before it). `None` when there's nothing to convert.
pub fn rates_needed(rows: &[Row], target: &str) -> Option<(BTreeSet<String>, String)> {
    let mut currencies: BTreeSet<String> =
        rows.iter().filter_map(|row| row.currency.clone()).collect();

    currencies.insert(target.to_string());

    if currencies.len() <= 1 {
        return None;
    }

    let earliest = rows
        .iter()
        .map(|row| row.date.as_str())
        .filter(|date| date.len() == 10)
        .min()?;

    let since = shift_date(earliest, 0, -7);

    Some((currencies, since))
}

/// A row's total in `target`, or `None` when it can't be converted.
pub fn converted(rates: &impl Rates, row: &Row, target: &str) -> Option<Decimal> {
    let total = row.total.as_deref().and_then(amount)?;
    let currency = row.currency.as_deref()?;

    convert(rates, total, currency, target, &row.date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    fn row(id: i64, date: &str, vendor: &str, total: &str, currency: &str) -> StoredRow {
        StoredRow {
            id,
            date: date.into(),
            kind: "receipt".into(),
            status: "paid".into(),
            is_receipt: 1,
            vendor_name: vendor.into(),
            total: Some(total.into()),
            currency: Some(currency.into()),
            ..StoredRow::default()
        }
    }

    fn rows(stored: Vec<StoredRow>) -> Vec<Row> {
        stored
            .into_iter()
            .map(|row| Row::from_stored(row, &catalog()))
            .collect()
    }

    fn invoice(id: i64, date: &str, vendor: &str, total: &str) -> StoredRow {
        StoredRow {
            kind: "invoice".into(),
            ..row(id, date, vendor, total, "USD")
        }
    }

    #[test]
    fn counts_a_paid_invoice_and_its_later_receipt_once() {
        // No numbers link them, and the receipt came nine days later.
        let rows = rows(vec![
            row(2, "2026-11-10", "Fjordbyrå AS", "300.00", "USD"),
            invoice(1, "2026-11-01", "Fjordbyrå AS", "300.00"),
        ]);

        let spent = spend(&rows);

        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].kind, "receipt");
    }

    #[test]
    fn a_receipt_takes_what_its_invoice_says_and_it_doesnt() {
        let receipt = row(2, "2026-11-02", "Figma", "540.00", "USD");

        let paid_invoice = StoredRow {
            interval: Some("year".into()),
            billing_model: Some("subscription".into()),
            seats: Some(12.0),
            next_renewal: Some("2027-11-01".into()),
            ..invoice(1, "2026-11-01", "Figma", "540.00")
        };

        let rows = rows(vec![receipt, paid_invoice]);

        let spent = spend(&rows);

        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].id, 2);
        assert_eq!(spent[0].interval.as_deref(), Some("year"));
        assert_eq!(spent[0].seats, Some(12.0));
        assert_eq!(spent[0].next_renewal.as_deref(), Some("2027-11-01"));
    }

    #[test]
    fn a_statements_payment_counts_once_with_the_invoice_it_pays() {
        let card_line = row(90, "2026-11-02", "Vercel", "20.00", "USD");

        let mut card = Row::from_stored(card_line, &catalog());

        card.payment = Some("90:4".into());

        let mut rows = rows(vec![invoice(1, "2026-11-01", "Vercel", "20.00")]);

        rows.insert(0, card);

        let spent = spend(&rows);

        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].payment.as_deref(), Some("90:4"));
    }

    #[test]
    fn keeps_each_month_of_a_subscription() {
        // October's receipt is missing: its invoice is still October's payment.
        let rows = rows(vec![
            row(3, "2026-11-01", "Vercel", "20.00", "USD"),
            invoice(2, "2026-11-01", "Vercel", "20.00"),
            invoice(1, "2026-10-01", "Vercel", "20.00"),
        ]);

        assert_eq!(spend(&rows).len(), 2);
    }

    #[test]
    fn groups_a_vendor_by_the_catalog_and_counts_each_receipt_once() {
        let mut copy = row(2, "2026-11-19", "Vercel Inc.", "20.00", "USD");

        copy.number = Some("2291-0007".into());

        let mut first = row(1, "2026-11-19", "Vercel", "20.00", "USD");

        first.number = Some("2291 0007".into());

        let rows = rows(vec![copy, first]);

        assert_eq!(rows[0].vendor_key, rows[1].vendor_key);
        assert_eq!(spend(&rows).len(), 1);
    }

    #[test]
    fn takes_the_workspace_as_the_account_else_the_email() {
        let mut both = row(1, "2026-11-19", "Slack", "87.75", "USD");

        both.account_workspace = Some(" fjordtech ".into());
        both.account_email = Some("ada@fjordtech.no".into());

        let mut email = row(2, "2026-11-19", "Slack", "87.75", "USD");

        email.account_email = Some("ada@fjordtech.no".into());

        let rows = rows(vec![
            both,
            email,
            row(3, "2026-11-19", "Slack", "87.75", "USD"),
        ]);

        assert_eq!(rows[0].account.as_deref(), Some("fjordtech"));
        assert_eq!(rows[1].account.as_deref(), Some("ada@fjordtech.no"));
        assert_eq!(rows[2].account, None);
    }

    #[test]
    fn says_which_rates_converting_needs() {
        let rows = rows(vec![row(1, "2026-11-19", "Vercel", "20.00", "USD")]);

        let (currencies, since) = rates_needed(&rows, "NOK").unwrap();

        assert_eq!(currencies.into_iter().collect::<Vec<_>>(), ["NOK", "USD"]);
        assert_eq!(since, "2026-11-12");
        assert!(rates_needed(&rows, "USD").is_none());
    }
}
