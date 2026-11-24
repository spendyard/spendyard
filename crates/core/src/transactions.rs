//! Transactions: payments from card statements (the store's `charges`),
//! each matched to the receipt or paid invoice for it, for `spendyard
//! charges` and the cloud alike.
//!
//! Matching pairs each purchase with a receipt: the same vendor, the same
//! amount (in the receipt's currency, or converted within a margin for card
//! currency fees), a date close by, and the same card when both say. What
//! is left is the point: payments with no receipt, and receipts on the same
//! cards with no payment.
//!
//! Links made by hand come first ([`ChargeLink`]): this receipt is that
//! payment's, or that payment needs none. A transaction is known by its
//! statement and its line on it, so a link holds when the statement is
//! read again.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::catalog::{catalog, name_key};
use crate::dates::{date_of, day_number};
use crate::pairing::{Paper, pair_invoices};
use crate::rates::{Rates, convert};
use crate::spend::{Row, StoredRow, amount};

/// How many days after a receipt's date its charge may be: some vendors
/// send the receipt when the invoice is made and charge the card later.
pub const DAYS_AFTER: i64 = 10;

/// How many days before a receipt's date its charge may be: the receipt
/// sent a day or two after the card was charged, or another time zone.
pub const DAYS_BEFORE: i64 = 3;

/// How far a converted amount may be off: the card's currency fee and the
/// difference between the bank's rate and the ECB's.
const CONVERTED_MARGIN: f64 = 0.04;

/// How far around a payment receipts are suggested for it, either way.
const SUGGEST_DAYS: i64 = 45;

/// The query for the charges, newest first: its columns are [`Charge`]'s.
pub const CHARGES_SQL: &str = "SELECT c.id AS id,
       c.document_id AS document_id,
       c.position AS position,
       c.date AS date,
       c.posted AS posted,
       c.merchant AS merchant,
       c.card_last4 AS card_last4,
       c.name_on_card AS name_on_card,
       c.amount AS amount,
       c.currency AS currency,
       c.original_amount AS original_amount,
       c.original_currency AS original_currency,
       c.kind AS kind,
       c.vendor_id AS vendor_id,
       c.vendor_name AS vendor_name,
       d.source AS source
FROM charges c
JOIN documents d ON d.id = c.document_id
ORDER BY c.date DESC, c.document_id DESC, c.position";

/// The query for the receipts payments are matched to: receipts and
/// invoices with a date, total and currency, not set aside; vendors as
/// edited. Its columns are [`MatchReceipt`]'s.
pub const MATCH_RECEIPTS_SQL: &str = "SELECT r.document_id AS document_id,
       coalesce(e.vendor_id, r.vendor_id) AS vendor_id,
       coalesce(e.vendor_name, r.vendor_name) AS vendor_name,
       substr(r.date, 1, 10) AS date,
       r.total AS total,
       upper(r.currency) AS currency,
       r.card_last4 AS card_last4,
       r.card_brand AS card_brand,
       r.invoice_number AS invoice_number,
       r.receipt_number AS receipt_number,
       coalesce(r.type, '') AS kind
FROM receipts r
LEFT JOIN edits e ON e.document_id = r.document_id
WHERE (r.is_receipt = 1 OR r.is_invoice = 1)
  AND coalesce(e.ignored, 0) = 0
  AND r.date IS NOT NULL AND r.total IS NOT NULL AND r.currency IS NOT NULL";

/// One charge as [`CHARGES_SQL`] gives it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Charge {
    pub id: i64,
    /// The statement it was read from.
    pub document_id: i64,
    /// Its line on the statement.
    pub position: i64,
    pub date: String,
    #[serde(default)]
    pub posted: Option<String>,
    pub merchant: String,
    #[serde(default)]
    pub card_last4: Option<String>,
    #[serde(default)]
    pub name_on_card: Option<String>,
    pub amount: String,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub original_amount: Option<String>,
    #[serde(default)]
    pub original_currency: Option<String>,
    /// `purchase`, `refund`, `fee`, `interest`, `payment`, …
    pub kind: String,
    #[serde(default)]
    pub vendor_id: Option<String>,
    #[serde(default)]
    pub vendor_name: Option<String>,
    /// Its statement's source: `upload:<user>`, `forward:<address>`.
    #[serde(default)]
    pub source: Option<String>,
}

impl Charge {
    /// Purchases and refunds need a receipt; bank fees, interest and
    /// payments to the card account don't.
    pub fn needs_receipt(&self) -> bool {
        self.kind == "purchase" || self.kind == "refund"
    }

    /// Its ID: its statement and its line, `<document>:<position>`.
    pub fn key(&self) -> String {
        key_of(self.document_id, self.position)
    }

    /// Its vendor, looked up again from its merchant, so additions to the
    /// catalog reach charges read before them.
    pub fn with_vendor(mut self) -> Charge {
        if let Some(vendor) = catalog().by_merchant(&self.merchant) {
            self.vendor_id = Some(vendor.id.clone());
            self.vendor_name = Some(vendor.name.clone());
        }

        self
    }
}

/// A transaction's ID: its statement and its line on it.
pub fn key_of(document_id: i64, position: i64) -> String {
    format!("{document_id}:{position}")
}

/// One receipt as [`MATCH_RECEIPTS_SQL`] gives it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchReceipt {
    pub document_id: i64,
    #[serde(default)]
    pub vendor_id: Option<String>,
    #[serde(default)]
    pub vendor_name: Option<String>,
    pub date: String,
    pub total: String,
    pub currency: String,
    #[serde(default)]
    pub card_last4: Option<String>,
    /// The card's brand as the receipt names it: "visa", "Mastercard".
    #[serde(default)]
    pub card_brand: Option<String>,
    #[serde(default)]
    pub invoice_number: Option<String>,
    #[serde(default)]
    pub receipt_number: Option<String>,
    /// `receipt` or `invoice`.
    #[serde(default)]
    pub kind: String,
}

impl MatchReceipt {
    /// Its vendor, from the catalog by name when the reading had no ID:
    /// receipts read before the catalog knew it.
    fn with_vendor(mut self) -> MatchReceipt {
        if self.vendor_id.is_none()
            && let Some(vendor) = self
                .vendor_name
                .as_deref()
                .and_then(|name| catalog().by_name(name))
        {
            self.vendor_id = Some(vendor.id.clone());
        }

        self
    }

    fn total(&self) -> Option<Decimal> {
        amount(&self.total)
    }
}

/// What someone decided about one payment, by its ID: this receipt is its,
/// or it needs none.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChargeLink {
    /// The payment: [`Charge::key`].
    pub id: String,
    #[serde(default)]
    pub receipt_id: Option<i64>,
    #[serde(default)]
    pub no_receipt: bool,
}

/// One payment, with its receipt or why it has none.
#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    /// Its statement and line: `<document>:<position>`.
    pub id: String,
    /// The payment, as its statement gave it.
    pub charge: Charge,
    /// `matched`, `missing`, or `no receipt needed`.
    pub state: String,
    /// The receipt's document, when matched.
    pub receipt_id: Option<i64>,
    /// Matched by hand, not by matching.
    pub by_hand: bool,
}

/// Every payment matched, and what's left.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Matching {
    pub transactions: Vec<Transaction>,
    pub matched: usize,
    pub missing: usize,
    pub no_receipt_needed: usize,
    /// Missing payments' amounts, per currency: `(currency, amount)`.
    pub missing_amount: Vec<(String, String)>,
    /// Receipts paid with a card the statements cover, within their dates,
    /// that no payment matched.
    pub receipts_without_charge: Vec<i64>,
}

/// How well `receipt` fits `charge`: lower is better, `None` when it
/// doesn't fit at all. An exact amount beats a converted one; then the
/// closer date wins.
fn score(
    charge: &Charge,
    receipt: &MatchReceipt,
    rates: &impl Rates,
    before: i64,
    after_days: i64,
    need_vendor: bool,
) -> Option<f64> {
    let vendor = same_vendor(charge, receipt);

    if need_vendor && !vendor {
        return None;
    }

    if let (Some(card), Some(receipt_card)) = (&charge.card_last4, &receipt.card_last4)
        && card != receipt_card
    {
        return None;
    }

    let after = day_number(&charge.date)? - day_number(&receipt.date)?;

    if !(-before..=after_days).contains(&after) {
        return None;
    }

    let closeness = amount_closeness(charge, receipt, rates)?;

    // Another vendor's receipt only fits when the amount is exact.
    if !vendor && closeness > 0.0 {
        return None;
    }

    let unlike = if vendor { 0.0 } else { 1000.0 };

    Some(unlike + closeness * 100.0 + after.abs() as f64)
}

fn same_vendor(charge: &Charge, receipt: &MatchReceipt) -> bool {
    if let (Some(a), Some(b)) = (&charge.vendor_id, &receipt.vendor_id) {
        return catalog().same_or_related(a, b);
    }

    let key = |name: &Option<String>| name.as_deref().map(name_key);

    let charge_name = key(&charge.vendor_name);

    charge_name.is_some() && charge_name == key(&receipt.vendor_name)
}

/// 0 for the same amount in the same currency; the relative difference
/// for a converted one within [`CONVERTED_MARGIN`]; `None` otherwise.
fn amount_closeness(charge: &Charge, receipt: &MatchReceipt, rates: &impl Rates) -> Option<f64> {
    let cent = Decimal::new(1, 2);
    let total = receipt.total()?;

    let exact_pairs = [
        (
            charge.original_amount.as_deref(),
            charge.original_currency.as_deref(),
        ),
        (Some(charge.amount.as_str()), charge.currency.as_deref()),
    ];

    for (value, currency) in exact_pairs {
        let (Some(value), Some(currency)) = (value, currency) else {
            continue;
        };

        if !currency.eq_ignore_ascii_case(&receipt.currency) {
            continue;
        }

        let value = amount(value)?;
        let close = (value.abs() - total.abs()).abs() <= cent;

        return close.then_some(0.0);
    }

    let currency = charge.currency.as_deref()?;
    let charged = amount(&charge.amount)?;

    let converted = convert(rates, total, &receipt.currency, currency, &charge.date)?;

    let charged = charged.abs().to_f64().unwrap_or(0.0);
    let expected = converted.abs().to_f64().unwrap_or(0.0);

    if expected == 0.0 {
        return None;
    }

    let difference = (charged - expected).abs() / expected;

    (difference <= CONVERTED_MARGIN).then_some(difference)
}

/// Receipts that are the same payment as another: the invoice and the
/// receipt a vendor sends for it (Stripe sends both), or the same receipt
/// read twice. Same vendor, amount and currency, and the same invoice or
/// receipt number; or a paid invoice and a receipt a few days apart.
fn same_payment(receipts: &[MatchReceipt]) -> HashMap<i64, Vec<i64>> {
    let mut groups: HashMap<(String, String, String), Vec<i64>> = HashMap::new();

    for receipt in receipts {
        let vendor = receipt
            .vendor_id
            .clone()
            .or_else(|| receipt.vendor_name.as_deref().map(name_key))
            .unwrap_or_default();

        let Some(total) = receipt.total() else {
            continue;
        };

        let money = format!("{} {}", total.abs().normalize(), receipt.currency);

        for number in [&receipt.invoice_number, &receipt.receipt_number]
            .into_iter()
            .flatten()
        {
            // Letters and digits only: `B085EFDD-0038` on the email is
            // `B085EFDD 0038` on its PDF.
            let plain: String = number
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_uppercase();

            if !plain.is_empty() {
                groups
                    .entry((vendor.clone(), money.clone(), plain))
                    .or_default()
                    .push(receipt.document_id);
            }
        }
    }

    let mut partners: HashMap<i64, Vec<i64>> = HashMap::new();

    for ids in groups.values().filter(|ids| ids.len() > 1) {
        for id in ids {
            partners
                .entry(*id)
                .or_default()
                .extend(ids.iter().filter(|other| *other != id));
        }
    }

    // The catalog's vendor when both have one; else the same name, for
    // vendors the catalog doesn't know.
    let same_vendor = |a: &MatchReceipt, b: &MatchReceipt| match (&a.vendor_id, &b.vendor_id) {
        (Some(a), Some(b)) => catalog().same_or_related(a, b),
        _ => {
            let key = |receipt: &MatchReceipt| receipt.vendor_name.as_deref().map(name_key);

            key(a).is_some_and(|name| !name.is_empty() && Some(name) == key(b))
        }
    };

    let papers: Vec<Paper> = receipts
        .iter()
        .map(|receipt| Paper {
            invoice: receipt.kind == "invoice",
            date: &receipt.date,
        })
        .collect();

    let same = |a: usize, b: usize| {
        let (a, b) = (&receipts[a], &receipts[b]);

        a.total().map(|t| t.abs()) == b.total().map(|t| t.abs())
            && a.currency == b.currency
            && same_vendor(a, b)
    };

    for (invoice, receipt) in pair_invoices(&papers, same) {
        let (invoice, receipt) = (receipts[invoice].document_id, receipts[receipt].document_id);

        partners.entry(invoice).or_default().push(receipt);
        partners.entry(receipt).or_default().push(invoice);
    }

    partners
}

/// The days each statement covers, per card: from its first charge to its
/// last, as day numbers.
fn coverage(charges: &[Charge]) -> Vec<(String, i64, i64)> {
    let mut ranges: BTreeMap<(i64, String), (i64, i64)> = BTreeMap::new();

    for charge in charges {
        let (Some(card), Some(day)) = (&charge.card_last4, day_number(&charge.date)) else {
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
/// and so do the days after it in which its charge could come.
fn is_covered(ranges: &[(String, i64, i64)], card: &str, date: &str) -> bool {
    let Some(day) = day_number(date) else {
        return false;
    };

    let within = |day: i64| {
        ranges
            .iter()
            .any(|(range_card, first, last)| range_card == card && *first <= day && day <= *last)
    };

    within(day) && within(day + DAYS_AFTER)
}

/// What matching has paired so far: payments, receipts, and which with which.
#[derive(Default)]
struct Taken {
    charges: HashSet<String>,
    receipts: HashSet<i64>,
    /// By payment: its receipt, and whether by hand.
    pairs: HashMap<String, (i64, bool)>,
}

impl Taken {
    /// Pairs a payment with a receipt, and the receipt's others of the same payment with it.
    fn take(
        &mut self,
        charge: String,
        receipt: i64,
        by_hand: bool,
        partners: &HashMap<i64, Vec<i64>>,
    ) {
        self.receipts.insert(receipt);
        self.receipts
            .extend(partners.get(&receipt).into_iter().flatten());
        self.charges.insert(charge.clone());
        self.pairs.insert(charge, (receipt, by_hand));
    }
}

/// Every payment matched to its receipt: links by hand first, then the
/// best fits, each receipt (with the others of the same payment) once.
/// Charges come newest first, as [`CHARGES_SQL`] gives them.
pub fn match_transactions(
    charges: Vec<Charge>,
    receipts: Vec<MatchReceipt>,
    links: &[ChargeLink],
    rates: &impl Rates,
) -> Matching {
    let charges: Vec<Charge> = charges.into_iter().map(Charge::with_vendor).collect();
    let receipts: Vec<MatchReceipt> = receipts
        .into_iter()
        .map(MatchReceipt::with_vendor)
        .collect();

    let links: BTreeMap<&str, &ChargeLink> =
        links.iter().map(|link| (link.id.as_str(), link)).collect();
    let partners = same_payment(&receipts);
    let known: BTreeSet<i64> = receipts.iter().map(|r| r.document_id).collect();

    let mut taken = Taken::default();

    // By hand first: a receipt that's still there.
    for charge in &charges {
        if let Some(receipt) = links
            .get(charge.key().as_str())
            .and_then(|link| link.receipt_id)
            && known.contains(&receipt)
        {
            taken.take(charge.key(), receipt, true, &partners);
        }
    }

    let mut candidates = Vec::new();

    for charge in &charges {
        let no_receipt = links
            .get(charge.key().as_str())
            .is_some_and(|link| link.no_receipt);

        if !charge.needs_receipt() || no_receipt || taken.charges.contains(&charge.key()) {
            continue;
        }

        for receipt in receipts
            .iter()
            .filter(|receipt| !taken.receipts.contains(&receipt.document_id))
        {
            if let Some(score) = score(charge, receipt, rates, DAYS_BEFORE, DAYS_AFTER, true) {
                candidates.push((score, charge.key(), receipt.document_id));
            }
        }
    }

    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));

    for (_, charge, receipt) in candidates {
        if taken.charges.contains(&charge) || taken.receipts.contains(&receipt) {
            continue;
        }

        taken.take(charge, receipt, false, &partners);
    }

    let pairs = taken.pairs;
    let used_receipts = taken.receipts;

    let mut matching = Matching::default();
    let mut missing_by_currency: BTreeMap<String, Decimal> = BTreeMap::new();

    for charge in &charges {
        let key = charge.key();
        let paired = pairs.get(&key).copied();
        let no_receipt = links.get(key.as_str()).is_some_and(|link| link.no_receipt);

        let state = match paired {
            Some(_) => {
                matching.matched += 1;

                "matched"
            }
            None if !charge.needs_receipt() || no_receipt => {
                matching.no_receipt_needed += 1;

                "no receipt needed"
            }
            None => {
                matching.missing += 1;

                let currency = charge.currency.clone().unwrap_or_default();

                *missing_by_currency.entry(currency).or_default() +=
                    amount(&charge.amount).unwrap_or_default();

                "missing"
            }
        };

        matching.transactions.push(Transaction {
            id: key,
            charge: charge.clone(),
            state: state.to_string(),
            receipt_id: paired.map(|(receipt, _)| receipt),
            by_hand: paired.is_some_and(|(_, by_hand)| by_hand),
        });
    }

    matching.missing_amount = missing_by_currency
        .into_iter()
        .map(|(currency, value)| (currency, value.to_string()))
        .collect();

    let covered = coverage(&charges);

    matching.receipts_without_charge = receipts
        .iter()
        .filter(|receipt| !used_receipts.contains(&receipt.document_id))
        .filter(|receipt| {
            receipt
                .card_last4
                .as_deref()
                .is_some_and(|card| is_covered(&covered, card, &receipt.date))
        })
        .map(|receipt| receipt.document_id)
        .collect();

    matching
}

/// Receipts that could be a payment's, the likeliest first, at most
/// `limit`: the same vendor within a few weeks, or any vendor's with the
/// exact amount.
pub fn suggestions(
    charge: &Charge,
    receipts: &[MatchReceipt],
    rates: &impl Rates,
    limit: usize,
) -> Vec<i64> {
    let charge = charge.clone().with_vendor();

    let mut scored: Vec<(f64, i64)> = receipts
        .iter()
        .cloned()
        .map(MatchReceipt::with_vendor)
        .filter_map(|receipt| {
            score(&charge, &receipt, rates, SUGGEST_DAYS, SUGGEST_DAYS, false)
                .map(|score| (score, receipt.document_id))
        })
        .collect();

    scored.sort_by(|a, b| a.0.total_cmp(&b.0));

    scored.into_iter().take(limit).map(|(_, id)| id).collect()
}

/// The payments with no receipt, as rows standing in for their receipts:
/// what subscriptions are worked out from beside the receipts. Purchases
/// only; a payment with its receipt is counted through the receipt.
pub fn payment_rows(matching: &Matching) -> Vec<Row> {
    matching
        .transactions
        .iter()
        .filter(|found| found.state == "missing" && found.charge.kind == "purchase")
        .filter_map(payment_row)
        .collect()
}

/// One payment as a row: paid, on its day, its vendor as the catalog knows
/// the merchant, else the merchant's own words.
fn payment_row(found: &Transaction) -> Option<Row> {
    let charge = &found.charge;
    let total = amount(&charge.amount)?.abs();

    let stored = StoredRow {
        id: charge.document_id,
        date: charge.date.clone(),
        kind: "receipt".into(),
        status: "paid".into(),
        is_receipt: 1,
        vendor_id: charge.vendor_id.clone(),
        vendor_name: charge
            .vendor_name
            .clone()
            .unwrap_or_else(|| charge.merchant.clone()),
        total: Some(total.to_string()),
        currency: charge.currency.clone(),
        card: charge
            .card_last4
            .as_ref()
            .map(|last4| format!("card {last4}")),
        source: charge.source.clone(),
        ..StoredRow::default()
    };

    let mut row = Row::from_stored(stored, catalog());

    row.payment = Some(found.id.clone());

    Some(row)
}

/// What matching needs to convert: the currencies of the charges and
/// receipts, and rates from which day on. `None` when one currency is all.
pub fn rates_needed(
    charges: &[Charge],
    receipts: &[MatchReceipt],
) -> Option<(BTreeSet<String>, String)> {
    let mut currencies: BTreeSet<String> = charges
        .iter()
        .filter_map(|charge| charge.currency.clone())
        .collect();

    currencies.extend(receipts.iter().map(|receipt| receipt.currency.clone()));

    if currencies.len() <= 1 {
        return None;
    }

    let earliest = charges.iter().map(|charge| charge.date.as_str()).min()?;
    let since = date_of(day_number(earliest)? - 7 - DAYS_AFTER);

    Some((currencies, since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rates::{Rate, RateTable};

    fn charge(position: i64, date: &str, merchant: &str, total: &str) -> Charge {
        Charge {
            id: position,
            document_id: 100,
            position,
            date: date.into(),
            merchant: merchant.into(),
            card_last4: Some("3776".into()),
            amount: total.into(),
            currency: Some("USD".into()),
            kind: "purchase".into(),
            ..Charge::default()
        }
    }

    fn receipt(document_id: i64, date: &str, vendor: &str, total: &str) -> MatchReceipt {
        MatchReceipt {
            document_id,
            vendor_name: Some(vendor.into()),
            date: date.into(),
            total: total.into(),
            currency: "USD".into(),
            kind: "receipt".into(),
            ..MatchReceipt::default()
        }
    }

    fn by_id(matching: &Matching) -> BTreeMap<&str, &Transaction> {
        matching
            .transactions
            .iter()
            .map(|t| (t.id.as_str(), t))
            .collect()
    }

    #[test]
    fn matches_each_payment_to_its_receipt_and_says_whats_missing() {
        let mut fee = charge(3, "2026-11-20", "ANNUAL CARD FEE", "25.00");

        fee.kind = "fee".into();

        let charges = vec![
            fee,
            charge(2, "2026-11-19", "VERCEL INC.", "20.00"),
            charge(1, "2026-11-05", "FIGMA MONTHLY", "45.00"),
        ];

        let receipts = vec![
            receipt(7, "2026-11-18", "Vercel", "20.00"),
            receipt(8, "2026-08-01", "Notion", "10.00"),
        ];

        let matched = match_transactions(charges, receipts, &[], &RateTable::default());
        let ids = by_id(&matched);

        assert_eq!(ids["100:2"].state, "matched");
        assert_eq!(ids["100:2"].receipt_id, Some(7));
        assert!(!ids["100:2"].by_hand);
        assert_eq!(ids["100:1"].state, "missing");
        assert_eq!(ids["100:3"].state, "no receipt needed");

        assert_eq!(
            (matched.matched, matched.missing, matched.no_receipt_needed),
            (1, 1, 1)
        );
        assert_eq!(
            matched.missing_amount,
            [("USD".to_string(), "45.00".to_string())]
        );
    }

    #[test]
    fn takes_links_by_hand_first() {
        let charges = vec![
            charge(2, "2026-11-19", "VERCEL INC.", "20.00"),
            charge(1, "2026-11-05", "SOME SHOP", "45.00"),
        ];

        let receipts = vec![
            receipt(7, "2026-11-18", "Vercel", "20.00"),
            receipt(9, "2026-11-01", "A shop", "45.00"),
        ];

        let links = vec![
            // Not the one matching would pick: someone knows better.
            ChargeLink {
                id: "100:1".into(),
                receipt_id: Some(9),
                ..ChargeLink::default()
            },
            ChargeLink {
                id: "100:2".into(),
                no_receipt: true,
                ..ChargeLink::default()
            },
        ];

        let matched = match_transactions(charges, receipts, &links, &RateTable::default());
        let ids = by_id(&matched);

        assert_eq!(ids["100:1"].receipt_id, Some(9));
        assert!(ids["100:1"].by_hand);
        assert_eq!(ids["100:2"].state, "no receipt needed");
    }

    #[test]
    fn matches_a_payment_in_another_currency_within_the_cards_fee() {
        let mut nok = charge(1, "2026-11-19", "VERCEL INC.", "224.50");

        nok.currency = Some("NOK".into());

        let rates = RateTable::new([
            Rate {
                date: "2026-11-18".into(),
                currency: "USD".into(),
                per_eur: 1.0,
            },
            Rate {
                date: "2026-11-18".into(),
                currency: "NOK".into(),
                per_eur: 11.0,
            },
        ]);

        let matched = match_transactions(
            vec![nok],
            vec![receipt(7, "2026-11-18", "Vercel", "20.00")],
            &[],
            &rates,
        );

        // 20 USD is 220 NOK: 224.50 is 2% off, within the card's fee.
        assert_eq!(matched.transactions[0].receipt_id, Some(7));
    }

    #[test]
    fn counts_an_invoice_and_its_receipt_as_one_payment() {
        let mut invoice = receipt(7, "2026-11-18", "Vercel", "20.00");

        invoice.kind = "invoice".into();
        invoice.vendor_id = Some("vercel.com".into());

        let mut paid = receipt(8, "2026-11-19", "Vercel", "20.00");

        paid.vendor_id = Some("vercel.com".into());

        let charges = vec![
            charge(2, "2026-11-20", "VERCEL INC.", "20.00"),
            charge(1, "2026-10-20", "VERCEL INC.", "20.00"),
        ];

        let matched = match_transactions(charges, vec![invoice, paid], &[], &RateTable::default());

        // One of them matched; the other is the same payment, not a second.
        assert_eq!(matched.matched, 1);
        assert_eq!(matched.missing, 1);
    }

    #[test]
    fn takes_an_unknown_vendors_receipt_with_its_invoice() {
        // Not in the catalog: the same name makes them the same vendor.
        let mut invoice = receipt(7, "2026-11-18", "Acme Widgets", "20.00");

        invoice.kind = "invoice".into();
        invoice.card_last4 = Some("3776".into());

        let mut paid = receipt(8, "2026-11-19", "Acme Widgets", "20.00");

        paid.card_last4 = Some("3776".into());

        // The statement runs from November 10 to December 5.
        let charges = vec![
            charge(3, "2026-12-05", "FIGMA MONTHLY", "45.00"),
            charge(2, "2026-11-20", "ACME WIDGETS", "20.00"),
            charge(1, "2026-11-10", "FIGMA MONTHLY", "45.00"),
        ];

        // Its charge linked to the invoice by hand.
        let links = [ChargeLink {
            id: key_of(100, 2),
            receipt_id: Some(7),
            no_receipt: false,
        }];

        let matched =
            match_transactions(charges, vec![invoice, paid], &links, &RateTable::default());

        // The receipt is the same payment, not one without a charge.
        assert_eq!(matched.matched, 1);
        assert!(matched.receipts_without_charge.is_empty());
    }

    #[test]
    fn takes_a_paid_invoice_with_its_later_receipt() {
        let mut invoice = receipt(7, "2026-11-01", "Vercel", "20.00");

        invoice.kind = "invoice".into();
        invoice.card_last4 = Some("3776".into());

        let mut paid = receipt(8, "2026-11-10", "Vercel", "20.00");

        paid.card_last4 = Some("3776".into());

        // The statement runs from October 25 to December 5.
        let charges = vec![
            charge(3, "2026-12-05", "FIGMA MONTHLY", "45.00"),
            charge(2, "2026-11-10", "VERCEL INC.", "20.00"),
            charge(1, "2026-10-25", "FIGMA MONTHLY", "45.00"),
        ];

        let matched = match_transactions(charges, vec![invoice, paid], &[], &RateTable::default());

        // The receipt took the charge; the invoice is the same payment, not one without a charge.
        assert_eq!(matched.matched, 1);
        assert!(matched.receipts_without_charge.is_empty());
    }

    #[test]
    fn suggests_receipts_for_a_payment_without_one() {
        let lone = charge(1, "2026-11-19", "SQ *COFFEE PLACE", "12.40");

        let receipts = vec![
            receipt(7, "2026-11-18", "Coffee Place", "12.40"),
            receipt(8, "2026-11-01", "Vercel", "20.00"),
        ];

        assert_eq!(suggestions(&lone, &receipts, &RateTable::default(), 5), [7]);
    }

    #[test]
    fn makes_rows_of_payments_without_a_receipt() {
        let charges = vec![
            charge(2, "2026-11-19", "VERCEL INC.", "20.00"),
            charge(1, "2026-11-05", "FIGMA MONTHLY", "45.00"),
        ];

        let matched = match_transactions(
            charges,
            vec![receipt(7, "2026-11-18", "Vercel", "20.00")],
            &[],
            &RateTable::default(),
        );
        let rows = payment_rows(&matched);

        // Vercel's has its receipt: only Figma's stands in for one.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].vendor_key, "figma.com");
        assert_eq!(rows[0].payment.as_deref(), Some("100:1"));
        assert_eq!(rows[0].card.as_deref(), Some("card 3776"));
    }

    #[test]
    fn says_which_rates_matching_needs() {
        let mut nok = charge(1, "2026-11-19", "VERCEL INC.", "224.50");

        nok.currency = Some("NOK".into());

        let (currencies, since) =
            rates_needed(&[nok], &[receipt(7, "2026-11-18", "Vercel", "20.00")]).unwrap();

        assert_eq!(currencies.into_iter().collect::<Vec<_>>(), ["NOK", "USD"]);
        assert_eq!(since, "2026-11-02");
    }
}
