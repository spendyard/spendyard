//! Subscriptions: what an organisation pays again and again, worked out from
//! its rows ([`crate::spend`]) for `spendyard subscriptions` and the cloud
//! alike.
//!
//! One subscription is one vendor's account's product. A vendor's documents
//! are told apart by account (its workspace, else its email) when they show
//! more than one; then by product, but only for products paid side by side
//! (Jira beside Confluence), not one renamed over time (Pro, then Team).
//! Each has an ID made of those parts, stable while its documents are:
//! `slack.com`, `slack.com:fjordtech`, `atlassian.com::jira`.
//!
//! What people decide about one (not a subscription, cancelled) comes in
//! with the options, by that ID: the store keeps it, the core applies it.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::name_key;
use crate::dates::{days_apart, shift_date};
use crate::rates::Rates;
use crate::spend::{Row, converted, report_currency, spend};

/// How close payments of two products must be to count as paid side by
/// side: within a few weeks of each other.
const SIDE_BY_SIDE_DAYS: i64 = 20;

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
    /// Its ID: the vendor's key, then its account and product when they
    /// tell it apart from the vendor's others, joined by `:`.
    pub id: String,
    pub vendor: String,
    /// What the vendor's rows group by: its catalog ID (its domain), else
    /// its name's key.
    pub vendor_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The account it's for, when its documents say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
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
    /// How many of its documents there are.
    pub documents: usize,
    /// Its documents, newest first.
    pub document_ids: Vec<i64>,
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

    !row.personal && !row.ignored && (billed || repeats)
}

// --- telling subscriptions apart ------------------------------------------------

/// One subscription's rows, newest first, and what told it apart.
struct Group<'a> {
    vendor_key: &'a str,
    /// Its account, when the vendor's rows show more than one.
    account: Option<String>,
    /// Its product, when the vendor's account pays for several side by side.
    product: Option<String>,
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

/// A subscription's ID from what told it apart: `vendor`, `vendor:account`,
/// `vendor::product`, `vendor:account:product`.
fn id_of(vendor_key: &str, account: Option<&str>, product: Option<&str>) -> String {
    let vendor = slug(vendor_key);
    let account = account.map(slug).unwrap_or_default();

    match product {
        Some(product) => format!("{vendor}:{account}:{}", slug(&name_key(product))),
        None if account.is_empty() => vendor,
        None => format!("{vendor}:{account}"),
    }
}

/// A vendor's rows by account, when they show more than one. Rows that
/// don't say join the only account when there's one; with several, they're
/// a subscription of their own.
fn by_account<'a>(rows: Vec<&'a Row>) -> Vec<(Option<String>, Vec<&'a Row>)> {
    let mut accounts: BTreeMap<String, (String, Vec<&'a Row>)> = BTreeMap::new();
    let mut unknown = Vec::new();

    for row in &rows {
        match &row.account {
            Some(account) => {
                let entry = accounts
                    .entry(account.to_lowercase())
                    .or_insert_with(|| (account.clone(), Vec::new()));

                entry.1.push(row);
            }
            None => unknown.push(*row),
        }
    }

    if accounts.len() <= 1 {
        return vec![(None, rows)];
    }

    let mut groups: Vec<(Option<String>, Vec<&'a Row>)> = accounts
        .into_values()
        .map(|(account, rows)| (Some(account), rows))
        .collect();

    if !unknown.is_empty() {
        groups.push((None, unknown));
    }

    groups
}

/// Whether two products' rows were paid side by side: some of each within
/// a few weeks of each other.
fn side_by_side(a: &[&Row], b: &[&Row]) -> bool {
    a.iter().any(|x| {
        b.iter()
            .any(|y| days_apart(&x.date, &y.date).is_some_and(|apart| apart <= SIDE_BY_SIDE_DAYS))
    })
}

/// An account's rows by product, when it pays for several side by side,
/// each at least twice. The rest join the product with the same total as
/// theirs, else the one with the most rows. Otherwise they stay together.
fn by_product<'a>(rows: Vec<&'a Row>) -> Vec<(Option<String>, Vec<&'a Row>)> {
    let mut products: BTreeMap<String, (String, Vec<&'a Row>)> = BTreeMap::new();

    for row in &rows {
        if let Some(product) = &row.product {
            let entry = products
                .entry(name_key(product))
                .or_insert_with(|| (product.clone(), Vec::new()));

            entry.1.push(row);
        }
    }

    // Products paid more than once: one-offs are no subscription of their own.
    let recurring: Vec<(&String, &(String, Vec<&Row>))> = products
        .iter()
        .filter(|(_, (_, rows))| rows.len() >= 2)
        .collect();

    let parallel = recurring.iter().enumerate().any(|(i, (_, (_, a)))| {
        recurring[i + 1..]
            .iter()
            .any(|(_, (_, b))| side_by_side(a, b))
    });

    if !parallel {
        return vec![(None, rows)];
    }

    let mut groups: Vec<(Option<String>, Vec<&'a Row>)> = recurring
        .iter()
        .map(|(_, (product, _))| (Some(product.clone()), Vec::new()))
        .collect();

    let keys: Vec<&String> = recurring.iter().map(|(key, _)| *key).collect();

    for row in rows {
        let own = row
            .product
            .as_deref()
            .map(name_key)
            .and_then(|key| keys.iter().position(|k| **k == key));

        let same_total = || {
            groups
                .iter()
                .position(|(_, rows)| rows.first().is_some_and(|first| first.total == row.total))
        };

        let biggest = || {
            (0..groups.len())
                .max_by_key(|&i| recurring[i].1.1.len())
                .unwrap_or(0)
        };

        let at = own.or_else(same_total).unwrap_or_else(biggest);

        groups[at].1.push(row);
    }

    groups
}

/// The recurring rows, as subscriptions: by vendor, then account, then
/// product. Rows stay newest first.
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
        for (account, rows) in by_account(rows) {
            for (product, rows) in by_product(rows) {
                groups.push(Group {
                    vendor_key,
                    account: account.clone(),
                    product,
                    rows,
                });
            }
        }
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

/// Every card its rows were paid with, the most recently used first.
fn cards_of(rows: &[&Row]) -> Vec<CardSeen> {
    let mut seen: Vec<CardSeen> = Vec::new();

    // Rows come newest first: a card's first row is its last use.
    for row in rows {
        let Some(card) = &row.card else {
            continue;
        };

        if !seen.iter().any(|known| &known.card == card) {
            seen.push(CardSeen {
                card: card.clone(),
                last_used: row.date.clone(),
            });
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
    let card = last_paid
        .and_then(|row| row.card.clone())
        .or_else(|| cards.first().map(|seen| seen.card.clone()));

    Subscription {
        id: id_of(
            group.vendor_key,
            group.account.as_deref(),
            group.product.as_deref(),
        ),
        vendor: latest.vendor_name.clone(),
        vendor_key: latest.vendor_key.clone(),
        category: rows.iter().find_map(|row| row.category.clone()),
        account: group
            .account
            .clone()
            .or_else(|| rows.iter().find_map(|row| row.account.clone())),
        product: group.product.clone().or_else(|| product_of(rows)),
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
        documents: rows.len(),
        document_ids: rows.iter().map(|row| row.id).collect(),
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

    let mut list: Vec<Subscription> = groups(rows)
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
    fn tells_a_vendors_accounts_apart() {
        let slack = |id: i64, date: &str, workspace: Option<&str>| {
            let mut row = row(id, date, "Slack", "87.75", "USD");

            row.account_workspace = workspace.map(String::from);

            row
        };

        let found = found(
            vec![
                slack(4, "2026-11-10", Some("Fjordtech")),
                slack(3, "2026-11-09", Some("fjordtech-labs")),
                slack(2, "2026-10-10", Some("fjordtech")),
                slack(1, "2026-10-09", None),
            ],
            &options("2026-11-21"),
        );

        let ids = by_id(&found);

        assert_eq!(
            ids.keys().copied().collect::<Vec<_>>(),
            [
                "slack.com",
                "slack.com:fjordtech",
                "slack.com:fjordtech-labs"
            ]
        );
        assert_eq!(ids["slack.com:fjordtech"].document_ids, [4, 2]);
        assert_eq!(
            ids["slack.com:fjordtech"].account.as_deref(),
            Some("Fjordtech")
        );

        // One account only: the rows that don't say join it, and the ID is the vendor's.
        let one = found_one_account();

        assert_eq!(one.subscriptions.len(), 1);
        assert_eq!(one.subscriptions[0].id, "slack.com");
        assert_eq!(one.subscriptions[0].account.as_deref(), Some("fjordtech"));
        assert_eq!(one.subscriptions[0].documents, 2);
    }

    fn found_one_account() -> Subscriptions {
        let mut said = row(2, "2026-11-10", "Slack", "87.75", "USD");

        said.account_workspace = Some("fjordtech".into());

        found(
            vec![said, row(1, "2026-10-10", "Slack", "87.75", "USD")],
            &options("2026-11-21"),
        )
    }

    #[test]
    fn tells_products_paid_side_by_side_apart_but_not_one_renamed() {
        let atlassian = |id: i64, date: &str, product: &str, total: &str| {
            let mut row = row(id, date, "Atlassian", total, "USD");

            row.product = Some(product.into());

            row
        };

        let side_by_side = found(
            vec![
                atlassian(4, "2026-11-03", "Confluence", "55.00"),
                atlassian(3, "2026-11-01", "Jira", "80.00"),
                atlassian(2, "2026-10-03", "Confluence", "55.00"),
                atlassian(1, "2026-10-01", "Jira", "80.00"),
            ],
            &options("2026-11-21"),
        );

        let ids = by_id(&side_by_side);

        assert_eq!(
            ids.keys().copied().collect::<Vec<_>>(),
            ["atlassian.com::confluence", "atlassian.com::jira"]
        );
        assert_eq!(ids["atlassian.com::jira"].document_ids, [3, 1]);
        assert_eq!(ids["atlassian.com::jira"].product.as_deref(), Some("Jira"));

        // Pro, then Team: one subscription, renamed.
        let renamed = found(
            vec![
                atlassian(4, "2026-11-01", "Team", "80.00"),
                atlassian(3, "2026-10-01", "Team", "80.00"),
                atlassian(2, "2026-06-01", "Pro", "50.00"),
                atlassian(1, "2026-05-01", "Pro", "50.00"),
            ],
            &options("2026-11-21"),
        );

        assert_eq!(renamed.subscriptions.len(), 1);
        assert_eq!(renamed.subscriptions[0].id, "atlassian.com");
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

    #[test]
    fn makes_ids_of_the_parts() {
        assert_eq!(id_of("vercel.com", None, None), "vercel.com");
        assert_eq!(
            id_of("slack.com", Some("Fjordtech Labs"), None),
            "slack.com:fjordtech-labs"
        );
        assert_eq!(
            id_of(
                "google.com",
                Some("ada@fjordtech.no"),
                Some("Google Workspace")
            ),
            "google.com:ada@fjordtech.no:google-workspace"
        );
        assert_eq!(
            id_of("acme corp", None, Some("Pro Plan")),
            "acme-corp::pro-plan"
        );
    }
}
