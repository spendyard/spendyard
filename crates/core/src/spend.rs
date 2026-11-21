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
use crate::dates::{SAME_PAYMENT_DAYS, days_apart, shift_date};
use crate::rates::{Rates, convert};

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
       d.source AS source
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

        Row {
            id: stored.id,
            date: stored.date,
            kind: stored.kind,
            status: stored.status,
            is_receipt: stored.is_receipt != 0,
            vendor_key,
            vendor_name,
            category,
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
        }
    }
}

/// A stored amount, as a decimal.
pub fn amount(text: &str) -> Option<Decimal> {
    text.parse().ok()
}

/// Paid receipts that count as spend, each once.
pub fn spend(rows: &[Row]) -> Vec<Row> {
    let once = once_by_number(rows);

    without_invoice_copies(once)
}

/// Drops a paid invoice when a receipt for the same payment is kept: same
/// vendor, amount and currency, a few days apart. Plain's invoice PDFs
/// carry only the invoice number and its receipt emails only the receipt
/// number, so numbers can't link them. Each receipt pairs once.
fn without_invoice_copies(rows: Vec<Row>) -> Vec<Row> {
    let mut paired = BTreeSet::new();
    let mut dropped = BTreeSet::new();

    for invoice in rows.iter().filter(|row| row.kind == "invoice") {
        let partner = rows.iter().find(|row| {
            row.kind == "receipt"
                && !paired.contains(&row.id)
                && row.vendor_key == invoice.vendor_key
                && row.currency == invoice.currency
                && row.total.as_deref().and_then(amount)
                    == invoice.total.as_deref().and_then(amount)
                && days_apart(&row.date, &invoice.date)
                    .is_some_and(|apart| apart <= SAME_PAYMENT_DAYS)
        });

        if let Some(partner) = partner {
            paired.insert(partner.id);
            dropped.insert(invoice.id);
        }
    }

    rows.into_iter()
        .filter(|row| !dropped.contains(&row.id))
        .collect()
}

/// Each payment once by its number: the same receipt saved twice.
fn once_by_number(rows: &[Row]) -> Vec<Row> {
    let mut seen = BTreeSet::new();

    rows.iter()
        .filter(|row| row.is_receipt && !row.personal && !row.ignored)
        .filter(|row| {
            row.total
                .as_deref()
                .and_then(amount)
                .is_some_and(|total| total > Decimal::ZERO)
        })
        .filter(|row| {
            // Letters and digits only: the same receipt as an email
            // (`B085EFDD-0038`) and as its PDF (`B085EFDD 0038`).
            let plain = row.number.as_deref().map(|number| {
                number
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_uppercase()
            });

            let key = match plain.filter(|number| !number.is_empty()) {
                Some(number) => format!(
                    "{}|{number}|{}",
                    row.vendor_key,
                    row.total.as_deref().unwrap_or("")
                ),
                None => format!("document {}", row.id),
            };

            seen.insert(key)
        })
        .cloned()
        .collect()
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

/// One recurring vendor: what it costs, how often, on which card, and
/// whether the last payment went through.
#[derive(Debug, Clone, Serialize)]
pub struct Subscription {
    pub vendor: String,
    /// What the vendor's rows group by: its catalog ID (its domain), else
    /// its name's key.
    pub vendor_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `month`, `year`, `quarter`, `week`, or `?` when no document says.
    pub interval: String,
    pub last_paid: Option<String>,
    /// The last payment as it was paid: "20.00 USD".
    pub last_amount: Option<String>,
    pub last_total: Option<String>,
    pub last_currency: Option<String>,
    /// The document of the last payment, or of the newest when none was paid.
    pub last_document: i64,
    /// Its newest document: a failed payment's notice, an invoice due.
    pub newest_document: i64,
    /// What it costs a month in the report's currency, from the last payment.
    pub per_month: Option<Decimal>,
    pub seats: Option<f64>,
    pub card: Option<String>,
    pub next_renewal: Option<String>,
    /// `active`, `payment failed`, `due`, `lapsed?` or `unpaid`.
    pub status: String,
    /// How many of its documents there are.
    pub documents: usize,
    /// Its documents, newest first.
    pub document_ids: Vec<i64>,
}

/// The recurring vendors, and what the active ones cost a month.
#[derive(Debug, Clone, Serialize)]
pub struct Subscriptions {
    pub currency: String,
    pub subscriptions: Vec<Subscription>,
    pub monthly: Decimal,
}

/// How to work out the subscriptions.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionOptions {
    /// The currency to report in; the most common one paid in when unset.
    #[serde(default)]
    pub currency: Option<String>,
    /// Today, `YYYY-MM-DD`: the host's clock, not the core's.
    pub today: String,
    /// Also those lapsed for more than a year.
    #[serde(default)]
    pub all: bool,
}

/// Whether a row is about something recurring.
fn recurring(row: &Row) -> bool {
    let billed = matches!(
        row.billing_model.as_deref(),
        Some("subscription" | "mixed" | "usage")
    );
    let repeats = matches!(
        row.interval.as_deref(),
        Some("month" | "year" | "quarter" | "week")
    );

    !row.personal && !row.ignored && (billed || repeats)
}

/// An amount with two decimals, as money is shown: 16 is 16.00.
fn cents(value: Decimal) -> Decimal {
    let mut rounded = value.round_dp(2);

    rounded.rescale(2);

    rounded
}

/// A payment's cost a month, by how often it's paid.
fn monthly_of(value: Decimal, interval: &str) -> Decimal {
    let monthly = match interval {
        "year" => value / Decimal::from(12),
        "quarter" => value / Decimal::from(3),
        "week" => value * Decimal::new(433, 2),
        _ => value,
    };

    cents(monthly)
}

/// When it renews next, from its last payment, when the interval says.
fn estimated_renewal(last_paid: &str, interval: &str) -> Option<String> {
    match interval {
        "month" => Some(shift_date(last_paid, 1, 0)),
        "year" => Some(shift_date(last_paid, 12, 0)),
        "quarter" => Some(shift_date(last_paid, 3, 0)),
        _ => None,
    }
}

/// Where a vendor's payments stand, from its newest document and its last payment.
fn status_of(latest: &Row, last_paid: Option<&Row>, interval: &str, today: &str) -> &'static str {
    if latest.kind == "payment_failed" || latest.status == "failed" {
        return "payment failed";
    }

    if latest.status == "due" && !latest.is_receipt {
        return "due";
    }

    let Some(paid) = last_paid else {
        return "unpaid";
    };

    // Months past the last payment before it looks lapsed.
    let grace = match interval {
        "year" => 14,
        "quarter" => 4,
        _ => 2,
    };

    if shift_date(&paid.date, grace, 0).as_str() < today {
        "lapsed?"
    } else {
        "active"
    }
}

/// The recurring vendors in `rows` (newest first, as [`ROWS_SQL`] gives
/// them): live ones first, most expensive first; long-lapsed ones only
/// with `all`.
pub fn subscriptions(
    rows: &[Row],
    options: &SubscriptionOptions,
    rates: &impl Rates,
) -> Subscriptions {
    let paid = spend(rows);
    let currency = report_currency(options.currency.clone(), &paid);
    let today = options.today.as_str();

    let mut by_vendor: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();

    for row in rows.iter().filter(|row| recurring(row)) {
        by_vendor
            .entry(row.vendor_key.as_str())
            .or_default()
            .push(row);
    }

    let mut list = Vec::new();

    for rows in by_vendor.values() {
        // Rows come newest first.
        let latest = rows[0];

        let last_paid = rows
            .iter()
            .copied()
            .find(|row| row.is_receipt && row.total.is_some());

        let interval = rows
            .iter()
            .find_map(|row| row.interval.clone())
            .unwrap_or_else(|| "?".to_string());

        let per_month = last_paid
            .and_then(|row| converted(rates, row, &currency))
            .map(|value| monthly_of(value, &interval));

        let next_renewal = rows
            .iter()
            .find_map(|row| row.next_renewal.clone())
            .filter(|date| date.as_str() >= today)
            .or_else(|| last_paid.and_then(|row| estimated_renewal(&row.date, &interval)));

        let status = status_of(latest, last_paid, &interval, today);

        let last_amount = last_paid.and_then(|row| {
            let total = row.total.as_deref()?;
            let currency = row.currency.as_deref().unwrap_or("");

            Some(format!("{total} {currency}").trim_end().to_string())
        });

        list.push(Subscription {
            vendor: latest.vendor_name.clone(),
            vendor_key: latest.vendor_key.clone(),
            category: rows.iter().find_map(|row| row.category.clone()),
            interval,
            last_paid: last_paid.map(|row| row.date.clone()),
            last_amount,
            last_total: last_paid.and_then(|row| row.total.clone()),
            last_currency: last_paid.and_then(|row| row.currency.clone()),
            last_document: last_paid.unwrap_or(latest).id,
            newest_document: latest.id,
            per_month,
            seats: rows.iter().find_map(|row| row.seats),
            card: rows.iter().find_map(|row| row.card.clone()),
            next_renewal,
            status: status.to_string(),
            documents: rows.len(),
            document_ids: rows.iter().map(|row| row.id).collect(),
        });
    }

    // Long-gone tools only when asked for.
    if !options.all {
        let year_ago = shift_date(today, -12, 0);

        list.retain(|subscription| {
            subscription.status != "lapsed?"
                || subscription
                    .last_paid
                    .as_deref()
                    .is_some_and(|date| date >= year_ago.as_str())
        });
    }

    // Live ones first, most expensive first.
    let live = |subscription: &Subscription| {
        subscription.status == "active" || subscription.status == "payment failed"
    };

    list.sort_by(|a, b| {
        live(b).cmp(&live(a)).then(
            b.per_month
                .unwrap_or_default()
                .cmp(&a.per_month.unwrap_or_default()),
        )
    });

    let monthly: Decimal = list
        .iter()
        .filter(|subscription| subscription.status == "active")
        .filter_map(|subscription| subscription.per_month)
        .sum();

    let monthly = cents(monthly);

    Subscriptions {
        currency,
        subscriptions: list,
        monthly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;
    use crate::rates::{Rate, RateTable};

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
            billing_model: Some("subscription".into()),
            interval: Some("month".into()),
            ..StoredRow::default()
        }
    }

    fn rows(stored: Vec<StoredRow>) -> Vec<Row> {
        stored
            .into_iter()
            .map(|row| Row::from_stored(row, catalog()))
            .collect()
    }

    fn options(today: &str) -> SubscriptionOptions {
        SubscriptionOptions {
            currency: None,
            today: today.into(),
            all: false,
        }
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
    fn works_out_what_each_costs_a_month_and_where_it_stands() {
        let mut yearly = row(3, "2026-02-01", "Figma", "1440.00", "USD");

        yearly.interval = Some("year".into());

        let mut failed = row(4, "2026-11-20", "Notion", "96.00", "USD");

        failed.kind = "payment_failed".into();
        failed.is_receipt = 0;
        failed.status = "failed".into();

        let rows = rows(vec![
            failed,
            row(2, "2026-11-19", "Vercel", "20.00", "USD"),
            row(5, "2026-11-02", "Notion", "96.00", "USD"),
            row(1, "2026-10-19", "Vercel", "20.00", "USD"),
            yearly,
            row(6, "2025-06-01", "Heroku", "25.00", "USD"),
        ]);

        let found = subscriptions(&rows, &options("2026-11-21"), &RateTable::default());

        let by_vendor: BTreeMap<&str, &Subscription> = found
            .subscriptions
            .iter()
            .map(|s| (s.vendor.as_str(), s))
            .collect();

        assert_eq!(found.currency, "USD");

        assert_eq!(by_vendor["Vercel"].status, "active");
        assert_eq!(by_vendor["Vercel"].per_month, Some(Decimal::new(2000, 2)));
        assert_eq!(
            by_vendor["Vercel"].next_renewal.as_deref(),
            Some("2026-12-19")
        );
        assert_eq!(by_vendor["Vercel"].documents, 2);
        assert_eq!(by_vendor["Vercel"].document_ids, [2, 1]);

        assert_eq!(by_vendor["Figma"].per_month, Some(Decimal::new(12000, 2)));
        assert_eq!(by_vendor["Notion"].status, "payment failed");

        // Lapsed over a year ago: left out.
        assert!(!by_vendor.contains_key("Heroku"));

        // The active ones a month: Figma's 120 and Vercel's 20.
        assert_eq!(found.monthly, Decimal::new(14000, 2));
    }

    #[test]
    fn converts_into_the_currency_asked_for() {
        let rows = rows(vec![row(1, "2026-11-19", "Vercel", "20.00", "USD")]);
        let rates = RateTable::new([Rate {
            date: "2026-11-18".into(),
            currency: "USD".into(),
            per_eur: 1.25,
        }]);

        let options = SubscriptionOptions {
            currency: Some("eur".into()),
            ..options("2026-11-21")
        };
        let found = subscriptions(&rows, &options, &rates);

        assert_eq!(found.currency, "EUR");
        assert_eq!(
            found.subscriptions[0].per_month,
            Some(Decimal::new(1600, 2))
        );
        assert_eq!(
            found.subscriptions[0].last_amount.as_deref(),
            Some("20.00 USD")
        );
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
