//! `spendyard payments`: every payment once, money paid or back, with its
//! evidence (spendyard_core::payments), worked out from the store as the
//! cloud works it out, and kept in the store's payment tables: each one's
//! number, so it stays the payment's when its evidence changes, and those
//! retired, whose numbers aren't given again.

use std::collections::BTreeSet;

use anyhow::Result;
use spendyard_core::payments::{Record, Records, records};
use spendyard_core::spend::{Row, amount};
use spendyard_core::transactions::{
    CHARGES_SQL, Charge, MATCH_RECEIPTS_SQL, MatchReceipt, Matching, match_transactions,
    payment_rows,
};

use super::reports::{clip, money, rows};
use super::store::Store;
use super::transactions::{ensure_rates, query};

/// Every payment in the store, worked out again and kept: newest first.
pub async fn regroup(store: &Store) -> Result<Records> {
    let charges: Vec<Charge> = query(store, CHARGES_SQL)?;
    let receipts: Vec<MatchReceipt> = query(store, MATCH_RECEIPTS_SQL)?;

    ensure_rates(store, &charges, &receipts).await;

    let matched = match_transactions(charges, receipts, &[], store);

    let rows = with_payments(rows(store)?, &matched);

    let (stored, retired) = store.stored_payments()?;

    let found = records(&rows, &matched, &stored, &retired);

    store.save_payments(&found)?;

    Ok(found)
}

/// The store's rows as payments are worked out from: an invoice a card
/// payment paid counts as paid, as one read as paid does; and the
/// statements' payments with no receipt, and refunds, as rows of their own.
fn with_payments(rows: Vec<Row>, matched: &Matching) -> Vec<Row> {
    let paid_by_card: BTreeSet<i64> = matched
        .transactions
        .iter()
        .filter(|found| found.state == "matched")
        .filter_map(|found| found.receipt_id)
        .collect();

    let mut rows: Vec<Row> = rows
        .into_iter()
        .map(|mut row| {
            if paid_by_card.contains(&row.id) {
                row.is_receipt = true;
            }

            row
        })
        .collect();

    rows.extend(payment_rows(matched));
    rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));

    rows
}

pub struct PaymentsOptions {
    /// Only those with no receipt or invoice: a statement's line alone.
    pub missing: bool,
    /// Only this vendor (part of its name or key).
    pub vendor: Option<String>,
    /// Also those marked personal or not spend.
    pub all: bool,
    pub limit: usize,
    pub json: bool,
}

/// `spendyard payments`: works them out, keeps them, and lists them.
pub async fn list(options: PaymentsOptions) -> Result<()> {
    let store = Store::open()?;

    let found = regroup(&store).await?;

    let vendor = options.vendor.as_deref().map(str::to_lowercase);

    let picked: Vec<&Record> = found
        .records
        .iter()
        .filter(|record| options.all || !(record.row.personal || record.row.ignored))
        .filter(|record| !options.missing || record.evidence.documents.is_empty())
        .filter(|record| {
            vendor.as_deref().is_none_or(|vendor| {
                record.row.vendor_key.contains(vendor)
                    || record.row.vendor_name.to_lowercase().contains(vendor)
            })
        })
        .take(options.limit)
        .collect();

    if options.json {
        println!("{}", serde_json::to_string_pretty(&picked)?);

        return Ok(());
    }

    if picked.is_empty() {
        println!(
            "No payments. `spendyard read` reads receipts and card statements into the store."
        );

        return Ok(());
    }

    for record in picked {
        println!("{}", line_of(record));
    }

    Ok(())
}

/// One payment as a line: its number, day, vendor, amount, and what shows it.
fn line_of(record: &Record) -> String {
    let row = &record.row;

    let total = row
        .total
        .as_deref()
        .and_then(amount)
        .map(money)
        .unwrap_or_default();

    let currency = row.currency.as_deref().unwrap_or("");

    format!(
        "{:>5}  {}  {:<24} {:>12} {:<3}  {}",
        record.id,
        row.date,
        clip(&row.vendor_name, 24),
        total,
        currency,
        evidence_of(record),
    )
}

/// What a payment has to show for it, in words: "refund, 2 documents, on a
/// statement"; "no receipt" for money paid with no document.
fn evidence_of(record: &Record) -> String {
    let documents = record.evidence.documents.len();
    let lines = record.evidence.charges.len();

    let mut words: Vec<String> = Vec::new();

    if record.row.personal {
        words.push("personal".into());
    }

    if record.row.ignored {
        words.push("not spend".into());
    }

    if record.kind != "payment" {
        words.push(record.kind.clone());
    }

    match documents {
        0 if record.kind == "payment" => words.push("no receipt".into()),
        0 => {}
        1 => words.push("1 document".into()),
        n => words.push(format!("{n} documents")),
    }

    match lines {
        0 => {}
        1 => words.push("on a statement".into()),
        n => words.push(format!("{n} lines on statements")),
    }

    if let Some(payment) = record.corrects {
        words.push(format!("credits payment {payment}"));
    }

    words.join(", ")
}
