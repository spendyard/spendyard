//! `spendyard transactions`: card payments from statements, matched to
//! receipts.
//!
//! `read` fills the `transactions` table from card statements. Matching pairs
//! each purchase with the receipt or paid invoice for it: the same vendor,
//! the same amount (in the receipt's currency, or converted within a
//! margin for card currency fees), a date close by, and the same card when
//! both say. What is left is the point: purchases with no receipt, and
//! receipts on the same cards with no charge.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use spendyard_core::transactions::{
    CHARGES_SQL, Charge, MATCH_RECEIPTS_SQL, MatchReceipt, Matching, match_transactions,
    rates_needed,
};

use super::store::Store;

/// What `transactions list` shows.
pub struct ListOptions {
    pub missing: bool,
    pub card: Option<String>,

    /// `YYYY-MM`.
    pub month: Option<String>,

    pub json: bool,
}

/// One stored charge, with its match.
#[derive(Debug, Clone, Serialize)]
pub struct StoredCharge {
    pub id: i64,

    /// The statement it was read from.
    pub document_id: i64,

    pub date: String,
    pub merchant: String,
    pub card_last4: Option<String>,
    pub amount: String,
    pub currency: Option<String>,
    pub original_amount: Option<String>,
    pub original_currency: Option<String>,
    pub kind: String,
    pub vendor_id: Option<String>,
    pub vendor_name: Option<String>,

    /// The document ID of the matched receipt.
    pub receipt_id: Option<i64>,
}

impl StoredCharge {
    /// Purchases and refunds need a receipt; bank fees, interest and
    /// payments to the card account don't.
    pub fn needs_receipt(&self) -> bool {
        self.kind == "purchase" || self.kind == "refund"
    }
}

/// What a round of matching found.
#[derive(Debug, Default, Serialize)]
pub struct MatchSummary {
    pub charges: usize,
    pub matched: usize,
    pub missing: usize,

    /// Missing charges' amounts, per currency.
    pub missing_amount: Vec<(String, String)>,

    pub no_receipt_needed: usize,

    /// Receipts paid with a card on the statements, within their dates,
    /// that no charge matched: (document ID, vendor, date, total, currency).
    pub receipts_without_charge: Vec<(i64, String, String, String, String)>,
}

/// `spendyard transactions list`.
pub fn list(options: ListOptions) -> Result<()> {
    let store = Store::open()?;

    let charges = load_charges(&store)?;

    let shown: Vec<&StoredCharge> = charges
        .iter()
        .filter(|charge| {
            !options.missing || (charge.needs_receipt() && charge.receipt_id.is_none())
        })
        .filter(|charge| {
            options
                .card
                .as_deref()
                .is_none_or(|card| charge.card_last4.as_deref() == Some(card))
        })
        .filter(|charge| {
            options
                .month
                .as_deref()
                .is_none_or(|month| charge.date.starts_with(month))
        })
        .collect();

    if options.json {
        println!("{}", serde_json::to_string_pretty(&shown)?);

        return Ok(());
    }

    if shown.is_empty() {
        println!(
            "No transactions. `spendyard read statement.pdf` reads a card statement into the store."
        );

        return Ok(());
    }

    for charge in &shown {
        print_charge(charge);
    }

    let unmatched = shown
        .iter()
        .filter(|charge| charge.needs_receipt() && charge.receipt_id.is_none())
        .count();

    println!();
    println!(
        "{} transactions, {unmatched} without a receipt",
        shown.len()
    );

    Ok(())
}

/// `spendyard transactions match`.
pub async fn match_command(json: bool) -> Result<()> {
    let store = Store::open()?;

    let summary = match_all(&store).await?;

    // Which receipt a payment has changed: its payment, and so the payments kept, with it.
    super::payments::regroup(&store).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);

        return Ok(());
    }

    print_summary(&summary);

    Ok(())
}

/// Matches every charge in the store again, from scratch, with the core's
/// matching (as the cloud's): writes each charge's vendor and receipt.
pub async fn match_all(store: &Store) -> Result<MatchSummary> {
    let charges: Vec<Charge> = query(store, CHARGES_SQL)?;
    let receipts: Vec<MatchReceipt> = query(store, MATCH_RECEIPTS_SQL)?;

    ensure_rates(store, &charges, &receipts).await;

    let matched = match_transactions(charges, receipts.clone(), &[], store);

    let transaction = store.connection.unchecked_transaction()?;

    transaction.execute("UPDATE transactions SET receipt_id = NULL", [])?;

    for found in &matched.transactions {
        transaction.execute(
            "UPDATE transactions SET receipt_id = ?1, vendor_id = ?2, vendor_name = ?3 WHERE id = ?4",
            params![
                found.receipt_id,
                found.charge.vendor_id,
                found.charge.vendor_name,
                found.charge.id
            ],
        )?;
    }

    transaction.commit()?;

    Ok(summary_of(&matched, &receipts))
}

/// The rows of a core query, by their columns' names.
pub(crate) fn query<T: serde::de::DeserializeOwned>(store: &Store, sql: &str) -> Result<Vec<T>> {
    let mut statement = store.connection.prepare(sql)?;
    let names: Vec<String> = statement.column_names().map(String::from).collect();

    let rows = statement.query_map([], |row| {
        let mut object = serde_json::Map::new();

        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                rusqlite::types::ValueRef::Integer(n) => n.into(),
                rusqlite::types::ValueRef::Real(f) => f.into(),
                rusqlite::types::ValueRef::Text(t) => {
                    String::from_utf8_lossy(t).into_owned().into()
                }
                rusqlite::types::ValueRef::Blob(_) => serde_json::Value::Null,
            };

            object.insert(name.clone(), value);
        }

        Ok(serde_json::Value::Object(object))
    })?;

    let mut out = Vec::new();

    for row in rows {
        out.push(serde_json::from_value(row?)?);
    }

    Ok(out)
}

/// The summary `transactions match` prints, from the core's matching.
fn summary_of(matched: &Matching, receipts: &[MatchReceipt]) -> MatchSummary {
    let receipts_without_charge = matched
        .receipts_without_charge
        .iter()
        .filter_map(|id| receipts.iter().find(|receipt| receipt.document_id == *id))
        .map(|receipt| {
            let vendor = receipt
                .vendor_name
                .clone()
                .or_else(|| receipt.vendor_id.clone())
                .unwrap_or_default();

            (
                receipt.document_id,
                vendor,
                receipt.date.clone(),
                receipt.total.clone(),
                receipt.currency.clone(),
            )
        })
        .collect();

    MatchSummary {
        charges: matched.transactions.len(),
        matched: matched.matched,
        missing: matched.missing,
        missing_amount: matched.missing_amount.clone(),
        no_receipt_needed: matched.no_receipt_needed,
        receipts_without_charge,
    }
}

/// Fetches ECB rates for receipts in another currency than the charges,
/// when the store lacks them. Matching goes on without them on failure.
pub(crate) async fn ensure_rates(store: &Store, charges: &[Charge], receipts: &[MatchReceipt]) {
    let Some((currencies, since)) = rates_needed(charges, receipts) else {
        return;
    };

    match store.update_rates(&currencies, &since).await {
        Ok(added) if added > 0 => {
            eprintln!("spendyard: fetched {added} exchange rates from the ECB")
        }
        Ok(_) => {}
        Err(error) => eprintln!("spendyard: no new exchange rates ({error:#})"),
    }
}

fn load_charges(store: &Store) -> Result<Vec<StoredCharge>> {
    let mut statement = store.connection.prepare(
        "SELECT id, date, merchant, card_last4, amount, currency, original_amount,
                original_currency, kind, vendor_id, vendor_name, receipt_id, document_id
         FROM transactions ORDER BY date, document_id, position",
    )?;

    let rows = statement.query_map([], |row| {
        Ok(StoredCharge {
            id: row.get(0)?,
            date: row.get(1)?,
            merchant: row.get(2)?,
            card_last4: row.get(3)?,
            amount: row.get(4)?,
            currency: row.get(5)?,
            original_amount: row.get(6)?,
            original_currency: row.get(7)?,
            kind: row.get(8)?,
            vendor_id: row.get(9)?,
            vendor_name: row.get(10)?,
            receipt_id: row.get(11)?,
            document_id: row.get(12)?,
        })
    })?;

    let charges = rows.collect::<Result<Vec<_>, _>>()?;

    Ok(charges)
}

fn print_charge(charge: &StoredCharge) {
    let vendor = charge.vendor_name.as_deref().unwrap_or("(vendor unknown)");

    let original = match (&charge.original_amount, &charge.original_currency) {
        (Some(value), Some(code)) => format!(" ({value} {code})"),
        _ => String::new(),
    };

    let status = if !charge.needs_receipt() {
        format!("no receipt needed ({})", charge.kind)
    } else {
        match charge.receipt_id {
            Some(id) => format!("receipt {id}"),
            None => "MISSING".to_string(),
        }
    };

    let card = charge
        .card_last4
        .as_deref()
        .map(|card| format!("•••• {card}"))
        .unwrap_or_default();

    println!(
        "{}  {:<38} {:<18} {:>10} {}{original}  {card}  {status}",
        charge.date,
        truncate(&charge.merchant, 38),
        truncate(vendor, 18),
        charge.amount,
        charge.currency.as_deref().unwrap_or(""),
    );
}

/// How many receipts without a charge the summary lists.
const SHOWN: usize = 10;

/// The summary as `transactions match` prints it.
pub fn print_summary(summary: &MatchSummary) {
    if summary.charges == 0 {
        println!(
            "No transactions. `spendyard read statement.pdf` reads a card statement into the store."
        );

        return;
    }

    let missing_amount: Vec<String> = summary
        .missing_amount
        .iter()
        .map(|(currency, value)| format!("{value} {currency}"))
        .collect();

    println!("{} transactions", summary.charges);
    println!("  {:>4} matched to a receipt", summary.matched);

    if summary.missing > 0 {
        println!(
            "  {:>4} missing a receipt: {}",
            summary.missing,
            missing_amount.join(", ")
        );
    }

    if summary.no_receipt_needed > 0 {
        println!(
            "  {:>4} fees, interest or payments, no receipt needed",
            summary.no_receipt_needed
        );
    }

    if !summary.receipts_without_charge.is_empty() {
        println!(
            "  {:>4} receipts on these cards with no charge:",
            summary.receipts_without_charge.len()
        );

        let shown = summary.receipts_without_charge.iter().take(SHOWN);

        for (id, vendor, date, total, currency) in shown {
            println!("         {date}  {vendor}  {total} {currency}  (receipt {id})");
        }

        let more = summary.receipts_without_charge.len().saturating_sub(SHOWN);

        if more > 0 {
            println!(
                "         … and {more} more (`spendyard transactions match --json` has them all)"
            );
        }
    }

    if summary.missing > 0 {
        println!();
        println!("`spendyard transactions list --missing` lists them.");
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}
