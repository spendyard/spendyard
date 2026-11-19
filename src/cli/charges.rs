//! `spendyard charges`: card charges from statements, matched to receipts.
//!
//! `read` fills the `charges` table from card statements. Matching pairs
//! each purchase with the receipt or paid invoice for it: the same vendor,
//! the same amount (in the receipt's currency, or converted within a
//! margin for card currency fees), a date close by, and the same card when
//! both say. What is left is the point: purchases with no receipt, and
//! receipts on the same cards with no charge.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use rusqlite::params;
use rust_decimal::Decimal;
use serde::Serialize;

use super::store::rates::amount;
use super::store::{Store, civil_from_days};

/// How many days after a receipt's date its charge may be: some vendors
/// send the receipt when the invoice is made and charge the card later.
const DAYS_AFTER: i64 = 10;

/// How many days before a receipt's date its charge may be: the receipt
/// sent a day or two after the card was charged, or another time zone.
const DAYS_BEFORE: i64 = 3;

/// How far a converted amount may be off: the card's currency fee and the
/// difference between the bank's rate and the ECB's.
const CONVERTED_MARGIN: f64 = 0.04;

/// What `charges list` shows.
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

/// A receipt as matching sees it.
#[derive(Debug, Clone)]
struct Receipt {
    document_id: i64,
    vendor_id: Option<String>,
    vendor_name: Option<String>,
    date: String,
    total: Decimal,
    currency: String,
    card_last4: Option<String>,
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

/// `spendyard charges list`.
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
            "No charges. `spendyard read statement.pdf` reads a card statement into the store."
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
    println!("{} charges, {unmatched} without a receipt", shown.len());

    Ok(())
}

/// `spendyard charges match`.
pub async fn match_command(json: bool) -> Result<()> {
    let store = Store::open()?;

    let summary = match_all(&store).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);

        return Ok(());
    }

    print_summary(&summary);

    Ok(())
}

/// Matches every charge in the store again, from scratch.
pub async fn match_all(store: &Store) -> Result<MatchSummary> {
    let mut charges = load_charges(store)?;

    refresh_vendors(store, &mut charges)?;

    let receipts = load_receipts(store)?;

    ensure_rates(store, &charges, &receipts).await;

    let mut candidates = Vec::new();

    for charge in charges.iter().filter(|charge| charge.needs_receipt()) {
        for receipt in &receipts {
            if let Some(score) = score(store, charge, receipt)? {
                candidates.push((score, charge.id, receipt.document_id));
            }
        }
    }

    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut used_charges = HashSet::new();
    let mut used_receipts = HashSet::new();
    let mut pairs = Vec::new();

    for (_, charge_id, receipt_id) in candidates {
        if used_charges.contains(&charge_id) || used_receipts.contains(&receipt_id) {
            continue;
        }

        used_charges.insert(charge_id);
        used_receipts.insert(receipt_id);

        pairs.push((charge_id, receipt_id));
    }

    store
        .connection
        .execute("UPDATE charges SET receipt_id = NULL", [])?;

    for (charge_id, receipt_id) in &pairs {
        store.connection.execute(
            "UPDATE charges SET receipt_id = ?1 WHERE id = ?2",
            params![receipt_id, charge_id],
        )?;
    }

    let summary = summarize(&charges, &receipts, &used_charges, &used_receipts);

    Ok(summary)
}

/// Looks each charge's vendor up again from its merchant, so additions to
/// the catalog reach charges read before them.
fn refresh_vendors(store: &Store, charges: &mut [StoredCharge]) -> Result<()> {
    let catalog = spendyard::catalog::catalog();

    for charge in charges.iter_mut() {
        let vendor = catalog.by_merchant(&charge.merchant);

        let id = vendor.map(|vendor| vendor.id.clone());

        if id == charge.vendor_id {
            continue;
        }

        charge.vendor_id = id;
        charge.vendor_name = vendor.map(|vendor| vendor.name.clone());

        store.connection.execute(
            "UPDATE charges SET vendor_id = ?1, vendor_name = ?2 WHERE id = ?3",
            params![charge.vendor_id, charge.vendor_name, charge.id],
        )?;
    }

    Ok(())
}

fn summarize(
    charges: &[StoredCharge],
    receipts: &[Receipt],
    used_charges: &HashSet<i64>,
    used_receipts: &HashSet<i64>,
) -> MatchSummary {
    let mut summary = MatchSummary {
        charges: charges.len(),
        ..MatchSummary::default()
    };

    let mut missing_by_currency: std::collections::BTreeMap<String, Decimal> =
        std::collections::BTreeMap::new();

    for charge in charges {
        if !charge.needs_receipt() {
            summary.no_receipt_needed += 1;
        } else if used_charges.contains(&charge.id) {
            summary.matched += 1;
        } else {
            summary.missing += 1;

            let currency = charge.currency.clone().unwrap_or_default();

            let value = amount(&charge.amount).unwrap_or_default();

            *missing_by_currency.entry(currency).or_default() += value;
        }
    }

    summary.missing_amount = missing_by_currency
        .into_iter()
        .map(|(currency, value)| (currency, value.to_string()))
        .collect();

    let covered = coverage(charges);

    summary.receipts_without_charge = receipts
        .iter()
        .filter(|receipt| !used_receipts.contains(&receipt.document_id))
        .filter(|receipt| {
            receipt
                .card_last4
                .as_deref()
                .is_some_and(|card| is_covered(&covered, card, &receipt.date))
        })
        .map(|receipt| {
            (
                receipt.document_id,
                receipt
                    .vendor_name
                    .clone()
                    .or_else(|| receipt.vendor_id.clone())
                    .unwrap_or_default(),
                receipt.date.clone(),
                receipt.total.to_string(),
                receipt.currency.clone(),
            )
        })
        .collect();

    summary
}

/// The days each statement covers, per card: from its first charge to its
/// last, as day numbers.
fn coverage(charges: &[StoredCharge]) -> Vec<(String, i64, i64)> {
    let mut ranges: std::collections::BTreeMap<(i64, String), (i64, i64)> =
        std::collections::BTreeMap::new();

    for charge in charges {
        let (Some(card), Some(day)) = (&charge.card_last4, days(&charge.date)) else {
            continue;
        };

        let range = ranges
            .entry((charge.document_id, card.clone()))
            .or_insert((day, day));

        range.0 = range.0.min(day);
        range.1 = range.1.max(day);
    }

    ranges
        .into_iter()
        .map(|((_, card), (first, last))| (card, first, last))
        .collect()
}

/// Whether a receipt on `card` dated `date` falls in a statement's days,
/// and so do the days after it in which its charge could come: a receipt
/// near the end of the last statement may be charged on the next one.
fn is_covered(ranges: &[(String, i64, i64)], card: &str, date: &str) -> bool {
    let Some(day) = days(date) else {
        return false;
    };

    let within = |day: i64| {
        ranges
            .iter()
            .any(|(range_card, first, last)| range_card == card && *first <= day && day <= *last)
    };

    within(day) && within(day + DAYS_AFTER)
}

/// How well `receipt` fits `charge`: lower is better, `None` when it
/// doesn't fit at all. An exact amount beats a converted one; then the
/// closer date wins.
fn score(store: &Store, charge: &StoredCharge, receipt: &Receipt) -> Result<Option<f64>> {
    if !same_vendor(charge, receipt) {
        return Ok(None);
    }

    if let (Some(card), Some(receipt_card)) = (&charge.card_last4, &receipt.card_last4)
        && card != receipt_card
    {
        return Ok(None);
    }

    let (Some(charge_day), Some(receipt_day)) = (days(&charge.date), days(&receipt.date)) else {
        return Ok(None);
    };

    let after = charge_day - receipt_day;

    if !(-DAYS_BEFORE..=DAYS_AFTER).contains(&after) {
        return Ok(None);
    }

    let apart = after.abs();

    let Some(closeness) = amount_closeness(store, charge, receipt)? else {
        return Ok(None);
    };

    Ok(Some(closeness * 100.0 + apart as f64))
}

fn same_vendor(charge: &StoredCharge, receipt: &Receipt) -> bool {
    if let (Some(a), Some(b)) = (&charge.vendor_id, &receipt.vendor_id) {
        return spendyard::catalog::catalog().same_or_related(a, b);
    }

    let key = |name: &Option<String>| name.as_deref().map(spendyard::catalog::name_key);

    let charge_name = key(&charge.vendor_name);

    charge_name.is_some() && charge_name == key(&receipt.vendor_name)
}

/// 0 for the same amount in the same currency; the relative difference
/// for a converted one within [`CONVERTED_MARGIN`]; `None` otherwise.
fn amount_closeness(
    store: &Store,
    charge: &StoredCharge,
    receipt: &Receipt,
) -> Result<Option<f64>> {
    let cent = Decimal::new(1, 2);

    let exact_pairs = [
        (&charge.original_amount, &charge.original_currency),
        (&Some(charge.amount.clone()), &charge.currency),
    ];

    for (value, currency) in exact_pairs {
        let (Some(value), Some(currency)) = (value, currency) else {
            continue;
        };

        if currency != &receipt.currency {
            continue;
        }

        let Some(value) = amount(value) else {
            continue;
        };

        let close = (value.abs() - receipt.total.abs()).abs() <= cent;

        return Ok(close.then_some(0.0));
    }

    let (Some(currency), Some(charged)) = (&charge.currency, amount(&charge.amount)) else {
        return Ok(None);
    };

    let converted = store.convert(receipt.total, &receipt.currency, currency, &charge.date)?;

    let Some(converted) = converted else {
        return Ok(None);
    };

    use rust_decimal::prelude::ToPrimitive;

    let charged = charged.abs().to_f64().unwrap_or(0.0);
    let expected = converted.abs().to_f64().unwrap_or(0.0);

    if expected == 0.0 {
        return Ok(None);
    }

    let difference = (charged - expected).abs() / expected;

    Ok((difference <= CONVERTED_MARGIN).then_some(difference))
}

/// Fetches ECB rates for receipts in another currency than the charges,
/// when the store lacks them. Matching goes on without them on failure.
async fn ensure_rates(store: &Store, charges: &[StoredCharge], receipts: &[Receipt]) {
    let mut currencies: BTreeSet<String> = charges
        .iter()
        .filter_map(|charge| charge.currency.clone())
        .collect();

    currencies.extend(receipts.iter().map(|receipt| receipt.currency.clone()));

    if currencies.len() <= 1 {
        return;
    }

    let Some(earliest) = charges.iter().map(|charge| charge.date.as_str()).min() else {
        return;
    };

    let Some(day) = days(earliest) else {
        return;
    };

    let (year, month, date) = civil_from_days(day - 7 - DAYS_AFTER);

    let since = format!("{year:04}-{month:02}-{date:02}");

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
         FROM charges ORDER BY date, document_id, position",
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

/// Receipts and invoices with a date, total and currency, not ignored.
/// Vendor names follow edits.
fn load_receipts(store: &Store) -> Result<Vec<Receipt>> {
    let mut statement = store.connection.prepare(
        "SELECT r.document_id, COALESCE(e.vendor_id, r.vendor_id),
                COALESCE(e.vendor_name, r.vendor_name), r.date, r.total, r.currency, r.card_last4
         FROM receipts r LEFT JOIN edits e ON e.document_id = r.document_id
         WHERE (r.is_receipt = 1 OR r.is_invoice = 1)
           AND COALESCE(e.ignored, 0) = 0
           AND r.date IS NOT NULL AND r.total IS NOT NULL AND r.currency IS NOT NULL",
    )?;

    let rows = statement.query_map([], |row| {
        let date: String = row.get(3)?;
        let total: String = row.get(4)?;

        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            date.chars().take(10).collect::<String>(),
            total,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut receipts = Vec::new();

    for row in rows {
        let (document_id, vendor_id, vendor_name, date, total, currency, card_last4) = row?;

        let Some(total) = amount(&total) else {
            continue;
        };

        // Receipts read before the catalog knew their vendor's name.
        let vendor_id = vendor_id.or_else(|| {
            let name = vendor_name.as_deref()?;

            let vendor = spendyard::catalog::catalog().by_name(name)?;

            Some(vendor.id.clone())
        });

        receipts.push(Receipt {
            document_id,
            vendor_id,
            vendor_name,
            date,
            total,
            currency: currency.to_uppercase(),
            card_last4,
        });
    }

    Ok(receipts)
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

/// The summary as `charges match` prints it.
pub fn print_summary(summary: &MatchSummary) {
    if summary.charges == 0 {
        println!(
            "No charges. `spendyard read statement.pdf` reads a card statement into the store."
        );

        return;
    }

    let missing_amount: Vec<String> = summary
        .missing_amount
        .iter()
        .map(|(currency, value)| format!("{value} {currency}"))
        .collect();

    println!("{} charges", summary.charges);
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
            println!("         … and {more} more (`spendyard charges match --json` has them all)");
        }
    }

    if summary.missing > 0 {
        println!();
        println!("`spendyard charges list --missing` lists them.");
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` date (Howard Hinnant's
/// algorithm, the inverse of [`civil_from_days`]).
fn days(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');

    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    let shifted_year = if month <= 2 { year - 1 } else { year };

    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;

    let month_index = if month > 2 { month - 3 } else { month + 9 };

    let day_of_year = (153 * month_index + 2) / 5 + day - 1;

    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip_with_civil_from_days() {
        for date in [
            "1970-01-01",
            "2026-02-28",
            "2026-03-01",
            "2024-02-29",
            "2026-12-31",
        ] {
            let (year, month, day) = civil_from_days(days(date).unwrap());

            assert_eq!(format!("{year:04}-{month:02}-{day:02}"), date);
        }

        assert_eq!(days("2026-04-20").unwrap() - days("2026-04-14").unwrap(), 6);
    }

    fn charge() -> StoredCharge {
        StoredCharge {
            id: 1,
            document_id: 3,
            date: "2026-04-20".to_string(),
            merchant: "ANTHROPIC: CLAUDE TEAM, SAN FRANCISCO, US".to_string(),
            card_last4: Some("3776".to_string()),
            amount: "8237.67".to_string(),
            currency: Some("NOK".to_string()),
            original_amount: Some("843.75".to_string()),
            original_currency: Some("USD".to_string()),
            kind: "purchase".to_string(),
            vendor_id: Some("anthropic.com".to_string()),
            vendor_name: Some("Anthropic".to_string()),
            receipt_id: None,
        }
    }

    fn receipt(total: &str, date: &str, card: Option<&str>) -> Receipt {
        Receipt {
            document_id: 7,
            vendor_id: Some("anthropic.com".to_string()),
            vendor_name: Some("Anthropic".to_string()),
            date: date.to_string(),
            total: amount(total).unwrap(),
            currency: "USD".to_string(),
            card_last4: card.map(str::to_string),
        }
    }

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "spendyard-charges-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let store = Store::open_at(&dir.join("test.db")).unwrap();

        (store, dir)
    }

    #[test]
    fn the_same_amount_in_the_receipts_currency_matches() {
        let (store, dir) = store();

        let exact = score(
            &store,
            &charge(),
            &receipt("843.75", "2026-04-19", Some("3776")),
        );
        let other_card = score(
            &store,
            &charge(),
            &receipt("843.75", "2026-04-19", Some("4242")),
        );
        let charged_later = score(&store, &charge(), &receipt("843.75", "2026-04-12", None));
        let too_late = score(&store, &charge(), &receipt("843.75", "2026-04-30", None));
        let other_amount = score(&store, &charge(), &receipt("843.00", "2026-04-20", None));

        assert_eq!(exact.unwrap(), Some(1.0));
        assert_eq!(other_card.unwrap(), None);
        assert_eq!(charged_later.unwrap(), Some(8.0));
        assert_eq!(too_late.unwrap(), None);
        assert_eq!(other_amount.unwrap(), None);

        std::fs::remove_dir_all(dir).ok();
    }
}
