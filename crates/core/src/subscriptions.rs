//! Subscriptions: what an organisation pays again and again, worked out from
//! its rows ([`crate::spend`]) for `spendyard subscriptions` and the cloud
//! alike.
//!
//! One subscription is one vendor: its ID is the vendor's key (`slack.com`),
//! stable while its documents are. Not told apart by account, which
//! documents name in different ways (an email on one, a workspace on the
//! next), nor for now by product; the account is what they say it's billed
//! to, the product the one they name most.
//!
//! What people decide about one (not a subscription, cancelled) comes in
//! with the options, by that ID: the store keeps it, the core applies it.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::name_key;
use crate::dates::{day_number, shift_date};
use crate::rates::Rates;
use crate::spend::{Row, converted, report_currency, spend};

/// A card a subscription was paid with, and when last.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardSeen {
    /// "visa 3776".
    pub card: String,
    pub last_used: String,
}

/// One subscription: what it costs, how often, on which card, and whether
/// the last payment went through.
#[derive(Debug, Clone, Serialize)]
pub struct Subscription {
    /// Its ID: the vendor's key, as an ID's part (`slack.com`).
    pub id: String,
    pub vendor: String,
    /// What the vendor's rows group by: its catalog ID (its domain), else
    /// its name's key.
    pub vendor_key: String,
    /// The vendor's domain, for its icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Who it's billed to, as its documents say: workspaces and emails,
    /// each once whatever its case, the most said first. Only what they say:
    /// one account can be named several ways.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub billed_to: Vec<String>,
    /// What it's for: the product its documents name most.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
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
    /// The card of the last payment: what's charged next.
    pub card: Option<String>,
    /// Every card it was paid with, the most recently used first.
    pub cards: Vec<CardSeen>,
    pub next_renewal: Option<String>,
    /// `active`, `payment failed`, `due`, `lapsed?`, `unpaid`, `cancelled`,
    /// or `not a subscription`.
    pub status: String,
    /// How many receipts and notices it's worked out from.
    pub documents: usize,
    /// How many payments from statements it's worked out from, with no
    /// receipt.
    pub payments: usize,
    /// Where it's known from: `receipts`, `payments` (only: its receipts are
    /// missing), or `both`.
    pub source: String,
    /// Its receipts and notices, newest first.
    pub document_ids: Vec<i64>,
    /// Its payments without a receipt, newest first: their transactions' IDs.
    pub payment_ids: Vec<String>,
}

/// The subscriptions, and what the active ones cost a month.
#[derive(Debug, Clone, Serialize)]
pub struct Subscriptions {
    pub currency: String,
    pub subscriptions: Vec<Subscription>,
    pub monthly: Decimal,
}

/// What someone decided about one subscription, by its ID.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionEdit {
    pub id: String,
    /// It isn't a subscription at all: left out.
    #[serde(default)]
    pub ignored: bool,
    /// The day it was cancelled, `YYYY-MM-DD`: cancelled, unless paid after.
    #[serde(default)]
    pub cancelled_on: Option<String>,
}

/// How to work out the subscriptions.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionOptions {
    /// The currency to report in; the most common one paid in when unset.
    #[serde(default)]
    pub currency: Option<String>,
    /// Today, `YYYY-MM-DD`: the host's clock, not the core's.
    pub today: String,
    /// Also those lapsed or cancelled more than a year ago, and those that
    /// aren't subscriptions.
    #[serde(default)]
    pub all: bool,
    /// What people decided about them.
    #[serde(default)]
    pub edits: Vec<SubscriptionEdit>,
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

    // A card statement's own reading isn't one vendor's: its payments are, as payments.
    let statement = row.kind == "card_statement";

    !row.personal && !row.ignored && !statement && (billed || repeats)
}

// --- payments without receipts -------------------------------------------------

/// How often payments days apart recur: about a week, a month, a quarter, a
/// year; `None` for anything else.
fn cadence(apart: i64) -> Option<&'static str> {
    match apart {
        6..=8 => Some("week"),
        25..=35 => Some("month"),
        85..=95 => Some("quarter"),
        350..=380 => Some("year"),
        _ => None,
    }
}

/// Payments from statements say nothing of how often they recur: from their
/// vendor's receipts when those say, else from how far apart they are (two
/// or more, the gap between them most of them share). A vendor paid once,
/// or at no steady rhythm, isn't a subscription by its payments.
pub(crate) fn with_intervals(rows: &[Row]) -> Vec<Row> {
    let mut rows = rows.to_vec();

    let vendors: BTreeSet<String> = rows.iter().map(|row| row.vendor_key.clone()).collect();

    for vendor in vendors {
        // Payments with no receipt: the interval the vendor's receipts say, else their own rhythm.
        let from_receipts = rows
            .iter()
            .filter(|row| row.vendor_key == vendor && row.payment.is_none() && recurring(row))
            .find_map(|row| row.interval.clone());

        let payments = days_of(
            rows.iter()
                .filter(|row| row.vendor_key == vendor && row.payment.is_some()),
        );

        if let Some(interval) = from_receipts.or_else(|| rhythm_of(&payments)) {
            for row in rows
                .iter_mut()
                .filter(|row| row.vendor_key == vendor && row.payment.is_some())
            {
                row.interval = Some(interval.clone());
                row.billing_model = Some("subscription".into());
            }
        }

        // Receipts read as one-offs, paid again and again: a subscription no receipt says is one. The rhythm tells; the
        // amount may change (seats added, another plan). Only two could be a month apart by chance: those two must
        // also be about the same amount.
        let one_offs: Vec<&Row> = rows
            .iter()
            .filter(|row| row.vendor_key == vendor && one_off(row))
            .collect();

        if one_offs.len() < 2 || (one_offs.len() == 2 && !alike(&one_offs)) {
            continue;
        }

        let Some(interval) = rhythm_of(&days_of(one_offs.into_iter())) else {
            continue;
        };

        for row in rows
            .iter_mut()
            .filter(|row| row.vendor_key == vendor && one_off(row))
        {
            row.interval = Some(interval.clone());
            row.billing_model = Some("subscription".into());
        }
    }

    rows
}

/// A receipt read as a one-off: paid, with an amount, of the organisation's spend, and not already recurring.
fn one_off(row: &Row) -> bool {
    row.payment.is_none()
        && row.total.is_some()
        && !row.personal
        && !row.ignored
        && row.kind != "card_statement"
        && !recurring(row)
}

/// Rows' days, each once, in order.
fn days_of<'a>(rows: impl Iterator<Item = &'a Row>) -> Vec<i64> {
    let mut days: Vec<i64> = rows.filter_map(|row| day_number(&row.date)).collect();

    days.sort_unstable();
    days.dedup();

    days
}

/// The rhythm most gaps between days share, when they do: `month`, `year`, ….
fn rhythm_of(days: &[i64]) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();

    for gap in days
        .windows(2)
        .filter_map(|pair| cadence(pair[1] - pair[0]))
    {
        *counts.entry(gap).or_default() += 1;
    }

    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .filter(|(_, count)| *count * 2 >= days.len().saturating_sub(1).max(1))
        .map(|(interval, _)| interval.to_string())
}

/// How far two one-offs' amounts may be apart and still be the same thing paid again: a fifth of the middle one.
const ALIKE: f64 = 0.2;

/// Whether rows were paid in one currency, each about the same amount: a plan paid again, not two purchases that
/// happen to be a month apart.
fn alike(rows: &[&Row]) -> bool {
    let currencies: BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.currency.as_deref())
        .collect();

    if currencies.len() > 1 {
        return false;
    }

    let mut amounts: Vec<f64> = rows
        .iter()
        .filter_map(|row| row.total.as_deref()?.parse::<f64>().ok())
        .collect();

    if amounts.len() != rows.len() {
        return false;
    }

    amounts.sort_by(|a, b| a.total_cmp(b));

    let middle = amounts[amounts.len() / 2];

    middle > 0.0
        && amounts
            .iter()
            .all(|amount| ((amount - middle) / middle).abs() <= ALIKE)
}

// --- one subscription per vendor ------------------------------------------------

/// One subscription's rows, newest first: a vendor's.
struct Group<'a> {
    vendor_key: &'a str,
    rows: Vec<&'a Row>,
}

/// A part of an ID: lowercase, its letters, digits, dots, `@` and `-`.
fn slug(text: &str) -> String {
    let lowered = text.trim().to_lowercase();

    let kept: String = lowered
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '@' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();

    kept.split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// A subscription's ID: its vendor's key, as an ID's part.
fn id_of(vendor_key: &str) -> String {
    slug(vendor_key)
}

/// Who rows say they're billed to: each once whatever its case, in the
/// spelling said most, the most said first.
fn billed_to(rows: &[&Row]) -> Vec<String> {
    let mut said: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();

    for account in rows.iter().filter_map(|row| row.account.as_deref()) {
        *said
            .entry(account.to_lowercase())
            .or_default()
            .entry(account)
            .or_default() += 1;
    }

    let mut accounts: Vec<(usize, &str)> = said
        .values()
        .map(|spellings| {
            let total = spellings.values().sum();
            let (spelling, _) = spellings.iter().max_by_key(|(_, n)| **n).unwrap();

            (total, *spelling)
        })
        .collect();

    accounts.sort_by_key(|account| std::cmp::Reverse(account.0));

    accounts
        .into_iter()
        .map(|(_, account)| account.to_string())
        .collect()
}

/// The recurring rows, as subscriptions: one per vendor. Rows stay newest
/// first.
fn groups(rows: &[Row]) -> Vec<Group<'_>> {
    let mut by_vendor: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();

    for row in rows.iter().filter(|row| recurring(row)) {
        by_vendor
            .entry(row.vendor_key.as_str())
            .or_default()
            .push(row);
    }

    let mut groups = Vec::new();

    for (vendor_key, rows) in by_vendor {
        groups.push(Group { vendor_key, rows });
    }

    groups
}

// --- one subscription -------------------------------------------------------------

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

/// Where its payments stand, from its newest document and its last payment.
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

/// A card's last four digits, however it's written: `visa 3776`, `card 3776`.
fn last4(card: &str) -> &str {
    card.rsplit(' ').next().unwrap_or(card)
}

/// Every card its rows were paid with, the most recently used first: one
/// card once, by its last four digits, named by its brand where a row says.
pub(crate) fn cards_of(rows: &[&Row]) -> Vec<CardSeen> {
    let mut seen: Vec<CardSeen> = Vec::new();

    // Rows come newest first: a card's first row is its last use.
    for row in rows {
        let Some(card) = &row.card else {
            continue;
        };

        match seen
            .iter_mut()
            .find(|known| last4(&known.card) == last4(card))
        {
            // A statement says `card 3776`; a receipt, `visa 3776`.
            Some(known) if known.card.starts_with("card ") && !card.starts_with("card ") => {
                known.card = card.clone()
            }
            Some(_) => {}
            None => seen.push(CardSeen {
                card: card.clone(),
                last_used: row.date.clone(),
            }),
        }
    }

    seen
}

/// The product its rows name most, as written.
fn product_of(rows: &[&Row]) -> Option<String> {
    let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();

    for product in rows.iter().filter_map(|row| row.product.as_ref()) {
        let entry = counts
            .entry(name_key(product))
            .or_insert_with(|| (product.clone(), 0));

        entry.1 += 1;
    }

    counts
        .into_values()
        .max_by_key(|(_, count)| *count)
        .map(|(product, _)| product)
}

/// One subscription from its group, in `currency`.
fn subscription_of(group: &Group, currency: &str, today: &str, rates: &impl Rates) -> Subscription {
    let rows = &group.rows;

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
        .and_then(|row| converted(rates, row, currency))
        .map(|value| monthly_of(value, &interval));

    let next_renewal = rows
        .iter()
        .find_map(|row| row.next_renewal.clone())
        .filter(|date| date.as_str() >= today)
        .or_else(|| last_paid.and_then(|row| estimated_renewal(&row.date, &interval)));

    let last_amount = last_paid.and_then(|row| {
        let total = row.total.as_deref()?;
        let currency = row.currency.as_deref().unwrap_or("");

        Some(format!("{total} {currency}").trim_end().to_string())
    });

    let cards = cards_of(rows);

    let payments = rows.iter().filter(|row| row.payment.is_some()).count();
    let receipts = rows.len() - payments;

    let source = match (receipts, payments) {
        (0, _) => "payments",
        (_, 0) => "receipts",
        _ => "both",
    };
    // The card of the last payment, named as the cards seen name it (branded where a receipt says).
    let card = last_paid
        .and_then(|row| row.card.as_deref())
        .and_then(|paid| cards.iter().find(|seen| last4(&seen.card) == last4(paid)))
        .or_else(|| cards.first())
        .map(|seen| seen.card.clone());

    Subscription {
        id: id_of(group.vendor_key),
        vendor: latest.vendor_name.clone(),
        vendor_key: latest.vendor_key.clone(),
        vendor_domain: rows.iter().find_map(|row| row.vendor_domain.clone()),
        category: rows.iter().find_map(|row| row.category.clone()),
        billed_to: billed_to(rows),
        product: product_of(rows),
        interval: interval.clone(),
        last_paid: last_paid.map(|row| row.date.clone()),
        last_amount,
        last_total: last_paid.and_then(|row| row.total.clone()),
        last_currency: last_paid.and_then(|row| row.currency.clone()),
        last_document: last_paid.unwrap_or(latest).id,
        newest_document: latest.id,
        per_month,
        seats: rows.iter().find_map(|row| row.seats),
        card,
        cards,
        next_renewal,
        status: status_of(latest, last_paid, &interval, today).to_string(),
        documents: receipts,
        payments,
        source: source.to_string(),
        document_ids: rows
            .iter()
            .filter(|row| row.payment.is_none())
            .map(|row| row.id)
            .collect(),
        payment_ids: rows.iter().filter_map(|row| row.payment.clone()).collect(),
    }
}

/// What someone decided about it, applied: not a subscription, or cancelled
/// (unless paid after).
fn apply(subscription: &mut Subscription, edit: &SubscriptionEdit) {
    if edit.ignored {
        subscription.status = "not a subscription".to_string();
        subscription.next_renewal = None;

        return;
    }

    let Some(cancelled_on) = &edit.cancelled_on else {
        return;
    };

    let paid_after = subscription
        .last_paid
        .as_deref()
        .is_some_and(|paid| paid > cancelled_on.as_str());

    if !paid_after {
        subscription.status = "cancelled".to_string();
        subscription.next_renewal = None;
    }
}

/// Whether it's left out unless all are asked for: not a subscription, or
/// lapsed or cancelled with its last payment over a year ago.
fn long_gone(subscription: &Subscription, year_ago: &str) -> bool {
    match subscription.status.as_str() {
        "not a subscription" => true,
        "lapsed?" | "cancelled" => subscription
            .last_paid
            .as_deref()
            .is_none_or(|date| date < year_ago),
        _ => false,
    }
}

/// The subscriptions in `rows` (newest first, as the store's query gives
/// them): live ones first, most expensive first; long-gone ones only with
/// `all`.
pub fn subscriptions(
    rows: &[Row],
    options: &SubscriptionOptions,
    rates: &impl Rates,
) -> Subscriptions {
    let paid = spend(rows);
    let currency = report_currency(options.currency.clone(), &paid);
    let today = options.today.as_str();

    let edits: BTreeMap<&str, &SubscriptionEdit> = options
        .edits
        .iter()
        .map(|edit| (edit.id.as_str(), edit))
        .collect();

    // Payments without receipts recur as their vendor's receipts do, or as their own rhythm says.
    let rows = with_intervals(rows);

    let mut list: Vec<Subscription> = groups(&rows)
        .iter()
        .map(|group| subscription_of(group, &currency, today, rates))
        .collect();

    for subscription in &mut list {
        if let Some(edit) = edits.get(subscription.id.as_str()) {
            apply(subscription, edit);
        }
    }

    if !options.all {
        let year_ago = shift_date(today, -12, 0);

        list.retain(|subscription| !long_gone(subscription, &year_ago));
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

    Subscriptions {
        currency,
        subscriptions: list,
        monthly: cents(monthly),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;
    use crate::rates::{Rate, RateTable};
    use crate::spend::StoredRow;

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
            today: today.into(),
            ..SubscriptionOptions::default()
        }
    }

    fn found(stored: Vec<StoredRow>, options: &SubscriptionOptions) -> Subscriptions {
        subscriptions(&rows(stored), options, &RateTable::default())
    }

    fn by_id(found: &Subscriptions) -> BTreeMap<&str, &Subscription> {
        found
            .subscriptions
            .iter()
            .map(|s| (s.id.as_str(), s))
            .collect()
    }

    #[test]
    fn works_out_what_each_costs_a_month_and_where_it_stands() {
        let mut yearly = row(3, "2026-02-01", "Figma", "1440.00", "USD");

        yearly.interval = Some("year".into());

        let mut failed = row(4, "2026-11-20", "Notion", "96.00", "USD");

        failed.kind = "payment_failed".into();
        failed.is_receipt = 0;
        failed.status = "failed".into();

        let found = found(
            vec![
                failed,
                row(2, "2026-11-19", "Vercel", "20.00", "USD"),
                row(5, "2026-11-02", "Notion", "96.00", "USD"),
                row(1, "2026-10-19", "Vercel", "20.00", "USD"),
                yearly,
                row(6, "2025-06-01", "Heroku", "25.00", "USD"),
            ],
            &options("2026-11-21"),
        );

        let by_vendor: BTreeMap<&str, &Subscription> = found
            .subscriptions
            .iter()
            .map(|s| (s.vendor.as_str(), s))
            .collect();

        assert_eq!(found.currency, "USD");

        assert_eq!(by_vendor["Vercel"].id, "vercel.com");
        assert_eq!(by_vendor["Vercel"].status, "active");
        assert_eq!(by_vendor["Vercel"].per_month, Some(Decimal::new(2000, 2)));
        assert_eq!(
            by_vendor["Vercel"].next_renewal.as_deref(),
            Some("2026-12-19")
        );
        assert_eq!(by_vendor["Vercel"].document_ids, [2, 1]);

        assert_eq!(by_vendor["Figma"].per_month, Some(Decimal::new(12000, 2)));
        assert_eq!(by_vendor["Notion"].status, "payment failed");

        // Lapsed over a year ago: left out.
        assert!(!by_vendor.contains_key("Heroku"));

        // The active ones a month: Figma's 120 and Vercel's 20.
        assert_eq!(found.monthly, Decimal::new(14000, 2));
    }

    #[test]
    fn a_vendors_accounts_however_named_are_one_subscription() {
        let slack = |id: i64, date: &str, workspace: Option<&str>| {
            let mut row = row(id, date, "Slack", "87.75", "USD");

            row.account_workspace = workspace.map(String::from);

            row
        };

        let found = found(
            vec![
                slack(4, "2026-11-10", Some("Fjordtech")),
                slack(3, "2026-10-10", Some("fjordtech")),
                slack(2, "2026-09-10", Some("fjordtech-labs")),
                slack(1, "2026-08-10", None),
            ],
            &options("2026-11-21"),
        );

        assert_eq!(found.subscriptions.len(), 1);

        let slack = &found.subscriptions[0];

        assert_eq!(slack.id, "slack.com");
        assert_eq!(slack.documents, 4);

        // Each once whatever its case, the most said first.
        assert_eq!(slack.billed_to.len(), 2);
        assert!(slack.billed_to[0].eq_ignore_ascii_case("fjordtech"));
        assert_eq!(slack.billed_to[1], "fjordtech-labs");
    }

    #[test]
    fn one_offs_paid_again_and_again_are_a_subscription() {
        let plain = |id: i64, date: &str, total: &str| {
            let mut row = row(id, date, "Plain", total, "USD");

            row.billing_model = Some("one_time".into());
            row.interval = None;

            row
        };

        let ids = |stored: Vec<StoredRow>| {
            found(stored, &options("2026-11-21"))
                .subscriptions
                .iter()
                .map(|s| (s.id.clone(), s.interval.clone()))
                .collect::<Vec<_>>()
        };

        // Two a month apart, about the same: monthly.
        assert_eq!(
            ids(vec![
                plain(2, "2026-11-17", "803.00"),
                plain(1, "2026-10-17", "790.00")
            ]),
            [("plain.com".to_string(), "month".to_string())]
        );

        // Two a month apart for very different amounts: could be two purchases, so not one.
        assert!(
            ids(vec![
                plain(2, "2026-11-17", "803.00"),
                plain(1, "2026-10-17", "120.00")
            ])
            .is_empty()
        );

        // Three a month apart, the amount changing as seats are added: still monthly.
        assert_eq!(
            ids(vec![
                plain(3, "2026-11-17", "1203.00"),
                plain(2, "2026-10-17", "803.00"),
                plain(1, "2026-09-17", "403.00"),
            ]),
            [("plain.com".to_string(), "month".to_string())]
        );

        // One alone, or no rhythm: one-offs.
        assert!(ids(vec![plain(1, "2026-10-17", "803.00")]).is_empty());
        assert!(
            ids(vec![
                plain(3, "2026-11-17", "803.00"),
                plain(2, "2026-11-02", "803.00"),
                plain(1, "2026-06-17", "803.00"),
            ])
            .is_empty()
        );
    }

    #[test]
    fn a_vendors_products_are_one_subscription() {
        let atlassian = |id: i64, date: &str, product: &str, total: &str| {
            let mut row = row(id, date, "Atlassian", total, "USD");

            row.product = Some(product.into());

            row
        };

        // Jira and Confluence side by side: one Atlassian, its product the one named most.
        let found = found(
            vec![
                atlassian(5, "2026-11-03", "Confluence", "55.00"),
                atlassian(4, "2026-11-01", "Jira", "80.00"),
                atlassian(3, "2026-10-03", "Confluence", "55.00"),
                atlassian(2, "2026-10-01", "Jira", "80.00"),
                atlassian(1, "2026-09-01", "Jira", "80.00"),
            ],
            &options("2026-11-21"),
        );

        assert_eq!(found.subscriptions.len(), 1);
        assert_eq!(found.subscriptions[0].id, "atlassian.com");
        assert_eq!(found.subscriptions[0].documents, 5);
        assert_eq!(found.subscriptions[0].product.as_deref(), Some("Jira"));
    }

    #[test]
    fn charges_the_last_payments_card_and_knows_every_card() {
        let paid = |id: i64, date: &str, card: &str| {
            let mut row = row(id, date, "Vercel", "20.00", "USD");

            row.card = Some(card.into());

            row
        };

        let found = found(
            vec![
                paid(3, "2026-11-19", "visa 1111"),
                paid(2, "2026-10-19", "mastercard 2222"),
                paid(1, "2026-09-19", "visa 1111"),
            ],
            &options("2026-11-21"),
        );

        let vercel = &found.subscriptions[0];

        assert_eq!(vercel.card.as_deref(), Some("visa 1111"));
        assert_eq!(
            vercel.cards,
            [
                CardSeen {
                    card: "visa 1111".into(),
                    last_used: "2026-11-19".into()
                },
                CardSeen {
                    card: "mastercard 2222".into(),
                    last_used: "2026-10-19".into()
                },
            ]
        );
    }

    #[test]
    fn applies_what_people_decided() {
        let stored = || {
            vec![
                row(2, "2026-11-19", "Vercel", "20.00", "USD"),
                row(1, "2026-11-10", "Figma", "45.00", "USD"),
            ]
        };

        let decided = SubscriptionOptions {
            edits: vec![
                SubscriptionEdit {
                    id: "vercel.com".into(),
                    cancelled_on: Some("2026-11-20".into()),
                    ..Default::default()
                },
                SubscriptionEdit {
                    id: "figma.com".into(),
                    ignored: true,
                    ..Default::default()
                },
            ],
            ..options("2026-11-21")
        };

        let found_now = found(stored(), &decided);

        assert_eq!(found_now.subscriptions.len(), 1);
        assert_eq!(found_now.subscriptions[0].status, "cancelled");
        assert_eq!(found_now.subscriptions[0].next_renewal, None);
        assert_eq!(found_now.monthly, Decimal::new(0, 2));

        // Not a subscription: there when all are asked for.
        let all = found(
            stored(),
            &SubscriptionOptions {
                all: true,
                ..decided.clone()
            },
        );

        assert!(
            all.subscriptions
                .iter()
                .any(|s| s.status == "not a subscription")
        );

        // Paid after it was cancelled: it's back.
        let paid_after = SubscriptionOptions {
            edits: vec![SubscriptionEdit {
                id: "vercel.com".into(),
                cancelled_on: Some("2026-11-01".into()),
                ..Default::default()
            }],
            ..options("2026-11-21")
        };

        assert_eq!(
            by_id(&found(stored(), &paid_after))["vercel.com"].status,
            "active"
        );
    }

    #[test]
    fn converts_into_the_currency_asked_for() {
        let rates = RateTable::new([Rate {
            date: "2026-11-18".into(),
            currency: "USD".into(),
            per_eur: 1.25,
        }]);

        let asked = SubscriptionOptions {
            currency: Some("eur".into()),
            ..options("2026-11-21")
        };

        let found = subscriptions(
            &rows(vec![row(1, "2026-11-19", "Vercel", "20.00", "USD")]),
            &asked,
            &rates,
        );

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

    /// A payment from a statement with no receipt, as `payment_rows` makes it.
    fn paid(id: i64, date: &str, vendor: &str, total: &str) -> Row {
        let stored = StoredRow {
            id,
            date: date.into(),
            kind: "receipt".into(),
            status: "paid".into(),
            is_receipt: 1,
            vendor_name: vendor.into(),
            total: Some(total.into()),
            currency: Some("USD".into()),
            card: Some("card 3776".into()),
            ..StoredRow::default()
        };

        let mut row = Row::from_stored(stored, catalog());

        row.payment = Some(format!("100:{id}"));

        row
    }

    #[test]
    fn finds_a_subscription_in_payments_alone() {
        let rows = vec![
            paid(3, "2026-11-05", "Figma", "45.00"),
            paid(2, "2026-10-05", "Figma", "45.00"),
            paid(1, "2026-09-04", "Figma", "45.00"),
            // Once only, or at no steady rhythm: not a subscription.
            paid(4, "2026-11-01", "Some Shop", "12.00"),
        ];

        let found = subscriptions(&rows, &options("2026-11-21"), &RateTable::default());

        assert_eq!(found.subscriptions.len(), 1);

        let figma = &found.subscriptions[0];

        assert_eq!(
            (
                figma.id.as_str(),
                figma.interval.as_str(),
                figma.source.as_str()
            ),
            ("figma.com", "month", "payments")
        );
        assert_eq!((figma.documents, figma.payments), (0, 3));
        assert_eq!(figma.payment_ids, ["100:3", "100:2", "100:1"]);
        assert!(figma.document_ids.is_empty());
        assert_eq!(figma.per_month, Some(Decimal::new(4500, 2)));
        assert_eq!(figma.status, "active");
    }

    #[test]
    fn counts_receipts_and_payments_without_receipts_together() {
        let mut with_card = rows(vec![row(1, "2026-10-19", "Vercel", "20.00", "USD")]);

        with_card[0].card = Some("visa 3776".into());

        // November's payment came without its receipt: it takes October's interval.
        let mut both = with_card;

        both.insert(0, paid(2, "2026-11-19", "Vercel", "20.00"));

        let found = subscriptions(&both, &options("2026-11-21"), &RateTable::default());
        let vercel = &found.subscriptions[0];

        assert_eq!(
            (vercel.source.as_str(), vercel.documents, vercel.payments),
            ("both", 1, 1)
        );
        assert_eq!(vercel.last_paid.as_deref(), Some("2026-11-19"));

        // One card, as the receipt names it.
        assert_eq!(vercel.card.as_deref(), Some("visa 3776"));
        assert_eq!(vercel.cards.len(), 1);
    }

    #[test]
    fn makes_ids_of_vendors() {
        assert_eq!(id_of("vercel.com"), "vercel.com");
        assert_eq!(id_of("acme corp"), "acme-corp");
    }
}
