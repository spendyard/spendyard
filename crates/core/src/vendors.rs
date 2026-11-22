//! Vendors: who an organisation pays, worked out from its rows
//! ([`crate::spend`]): what each has cost, month by month, split into
//! subscriptions, usage and one-offs; its seats over time; its
//! subscriptions, cards and accounts.
//!
//! What people decide about a vendor (its name, merging it into another,
//! its category, that it's personal or not spend at all) is kept by the
//! vendor's key and applied to the rows first ([`apply_vendor_edits`]), so
//! receipts that come later follow it, and subscriptions and reports see
//! the vendor as it was decided.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::catalog;
use crate::dates::shift_date;
use crate::rates::Rates;
use crate::spend::{Row, StoredRow, converted, report_currency, spend};
use crate::subscriptions::{CardSeen, SubscriptionOptions, subscriptions};

/// What someone decided about a vendor, by its key.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VendorEdit {
    /// The vendor's key, as the rows group by it.
    pub vendor: String,
    /// Its name as it should be shown.
    #[serde(default)]
    pub name: Option<String>,
    /// Another vendor's key: this one is that one.
    #[serde(default)]
    pub merge_into: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// Every receipt of it is personal, not the organisation's spend.
    #[serde(default)]
    pub personal: bool,
    /// Every receipt of it is not spend at all.
    #[serde(default)]
    pub ignored: bool,
}

/// How many merges to follow: a vendor merged into one merged into another,
/// and no further, so a loop can't hang.
const MERGE_DEPTH: usize = 8;

/// Where a vendor's key ends up after its merges.
fn merged(key: &str, edits: &BTreeMap<&str, &VendorEdit>) -> String {
    let mut current = key.to_string();

    for _ in 0..MERGE_DEPTH {
        let next = edits
            .get(current.as_str())
            .and_then(|edit| edit.merge_into.clone())
            .filter(|into| *into != current);

        match next {
            Some(into) => current = into,
            None => break,
        }
    }

    current
}

/// The rows with what people decided about their vendors: merged into
/// another, renamed, a category, personal, not spend. Applying them again
/// changes nothing more.
pub fn apply_vendor_edits(rows: Vec<Row>, edits: &[VendorEdit]) -> Vec<Row> {
    if edits.is_empty() {
        return rows;
    }

    let by_vendor: BTreeMap<&str, &VendorEdit> = edits
        .iter()
        .map(|edit| (edit.vendor.as_str(), edit))
        .collect();

    // A merged vendor takes its target's name, domain and category, as the target's own rows give them.
    let mut targets: BTreeMap<String, (String, Option<String>, Option<String>)> = BTreeMap::new();

    for row in &rows {
        targets.entry(row.vendor_key.clone()).or_insert_with(|| {
            (
                row.vendor_name.clone(),
                row.vendor_domain.clone(),
                row.category.clone(),
            )
        });
    }

    rows.into_iter()
        .map(|mut row| {
            let key = merged(&row.vendor_key, &by_vendor);

            if key != row.vendor_key {
                // The target as its own rows give it; else as the catalog knows it.
                let known = || {
                    catalog().find(Some(&key), None).map(|vendor| {
                        (
                            vendor.name.clone(),
                            Some(vendor.id.clone()),
                            Some(vendor.category.clone()),
                        )
                    })
                };

                if let Some((name, domain, category)) = targets.get(&key).cloned().or_else(known) {
                    row.vendor_name = name;
                    row.vendor_domain = domain;
                    row.category = category;
                }

                row.vendor_key = key.clone();
            }

            if let Some(edit) = by_vendor.get(key.as_str()) {
                if let Some(name) = edit.name.as_ref().filter(|name| !name.trim().is_empty()) {
                    row.vendor_name = name.trim().to_string();
                }

                if let Some(category) = edit
                    .category
                    .as_ref()
                    .filter(|category| !category.trim().is_empty())
                {
                    row.category = Some(category.trim().to_string());
                }

                row.personal |= edit.personal;
                row.ignored |= edit.ignored;
            }

            row
        })
        .collect()
}

/// A vendor as other things point at it (a receipt's, a payment's): its
/// key, as [`vendors`] has it, its name and its domain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VendorRef {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// The vendor a reading or a payment names (its ID and name as read, and
/// the name it was corrected to, if it was), as [`vendors`] would have it:
/// through the catalog, then what people decided (merged, renamed).
pub fn vendor_ref(
    vendor_id: Option<&str>,
    vendor_name: &str,
    edited_name: Option<&str>,
    edits: &[VendorEdit],
) -> VendorRef {
    let stored = StoredRow {
        vendor_id: vendor_id.map(String::from),
        vendor_name: vendor_name.to_string(),
        edited_vendor_name: edited_name.map(String::from),
        ..StoredRow::default()
    };

    let row = Row::from_stored(stored, catalog());
    let row = apply_vendor_edits(vec![row], edits).remove(0);

    VendorRef {
        id: row.vendor_key,
        name: row.vendor_name,
        domain: row.vendor_domain,
    }
}

/// One month of a vendor's spend, in the report's currency: on
/// subscriptions, on usage, and one-offs; and the most seats any of its
/// receipts that month paid for.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VendorMonth {
    /// `YYYY-MM`.
    pub month: String,
    pub subscription: Decimal,
    pub usage: Decimal,
    pub other: Decimal,
    pub seats: Option<f64>,
}

/// One vendor, as the organisation knows it.
#[derive(Debug, Clone, Serialize)]
pub struct VendorSummary {
    /// Its key: the catalog's ID (its domain) when the catalog knows it,
    /// else its name's key.
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Its domain, for its icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The catalog knows it.
    pub known: bool,
    /// What it cost these last twelve months, in the report's currency.
    pub spent_year: Decimal,
    /// What its active subscriptions cost a month together.
    pub monthly: Decimal,
    /// Its paid receipts, each once.
    pub receipts: usize,
    pub first_paid: Option<String>,
    pub last_paid: Option<String>,
    /// How many subscriptions it has, live or not.
    pub subscriptions: usize,
    /// Its last twelve months, oldest first.
    pub months: Vec<VendorMonth>,
    /// Every card it was paid with, the most recently used first.
    pub cards: Vec<CardSeen>,
    /// The accounts its documents name.
    pub accounts: Vec<String>,
    /// Every one of its receipts is personal, or not spend.
    pub personal: bool,
    pub ignored: bool,
    /// Its documents, newest first.
    pub document_ids: Vec<i64>,
}

/// The organisation's vendors, the most spent on first, and in which currency.
#[derive(Debug, Clone, Serialize)]
pub struct Vendors {
    pub currency: String,
    pub vendors: Vec<VendorSummary>,
}

/// How to work out the vendors.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VendorOptions {
    /// The currency to report in; the most common one paid in when unset.
    #[serde(default)]
    pub currency: Option<String>,
    /// Today, `YYYY-MM-DD`: the host's clock, not the core's.
    pub today: String,
    /// Also vendors that are personal or not spend.
    #[serde(default)]
    pub all: bool,
}

/// Which kinds of document make a vendor one the organisation pays.
fn about_money(row: &Row) -> bool {
    row.is_receipt
        || matches!(
            row.kind.as_str(),
            "invoice" | "renewal_notice" | "payment_failed"
        )
}

/// Which part of a month's spend a receipt is: subscription, usage, other.
fn bucket(row: &Row) -> &'static str {
    match row.billing_model.as_deref() {
        Some("usage") => "usage",
        Some("subscription" | "mixed") => "subscription",
        _ if row.interval.is_some() => "subscription",
        _ => "other",
    }
}

/// The last twelve months, `YYYY-MM`, oldest first, up to today's.
fn last_twelve(today: &str) -> Vec<String> {
    (0..12)
        .rev()
        .map(|back| shift_date(today, -back, 0).chars().take(7).collect())
        .collect()
}

/// One vendor's months: its paid receipts' amounts by month and part, and its seats.
fn months_of(
    rows: &[&Row],
    paid: &[&Row],
    currency: &str,
    today: &str,
    rates: &impl Rates,
) -> Vec<VendorMonth> {
    let mut months: BTreeMap<String, VendorMonth> = last_twelve(today)
        .into_iter()
        .map(|month| {
            (
                month.clone(),
                VendorMonth {
                    month,
                    ..VendorMonth::default()
                },
            )
        })
        .collect();

    for row in paid {
        let month: String = row.date.chars().take(7).collect();

        let (Some(entry), Some(value)) = (months.get_mut(&month), converted(rates, row, currency))
        else {
            continue;
        };

        match bucket(row) {
            "usage" => entry.usage += value,
            "subscription" => entry.subscription += value,
            _ => entry.other += value,
        }
    }

    for row in rows {
        let month: String = row.date.chars().take(7).collect();

        if let (Some(entry), Some(seats)) = (months.get_mut(&month), row.seats) {
            entry.seats = Some(entry.seats.map_or(seats, |most: f64| most.max(seats)));
        }
    }

    months.into_values().collect()
}

/// Every card its rows were paid with, the most recently used first.
fn cards_of(rows: &[&Row]) -> Vec<CardSeen> {
    let mut seen: Vec<CardSeen> = Vec::new();

    for row in rows {
        if let Some(card) = &row.card
            && !seen.iter().any(|known| &known.card == card)
        {
            seen.push(CardSeen {
                card: card.clone(),
                last_used: row.date.clone(),
            });
        }
    }

    seen
}

/// The vendors in `rows` (newest first, with their vendors' edits applied):
/// the most spent on these last twelve months first. Personal and not-spend
/// vendors only with `all`.
pub fn vendors(rows: &[Row], options: &VendorOptions, rates: &impl Rates) -> Vendors {
    let paid_all = spend(rows);
    let currency = report_currency(options.currency.clone(), &paid_all);
    let today = options.today.as_str();
    let year_ago = shift_date(today, -12, 0);

    let found_subscriptions = subscriptions(
        rows,
        &SubscriptionOptions {
            currency: Some(currency.clone()),
            today: today.to_string(),
            all: true,
            edits: Vec::new(),
        },
        rates,
    );

    let mut by_vendor: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();

    for row in rows.iter().filter(|row| about_money(row)) {
        by_vendor
            .entry(row.vendor_key.as_str())
            .or_default()
            .push(row);
    }

    let mut list = Vec::new();

    for (key, rows) in by_vendor {
        let latest = rows[0];

        let paid: Vec<&Row> = paid_all
            .iter()
            .filter(|row| row.vendor_key == key)
            .collect();

        let spent_year: Decimal = paid
            .iter()
            .filter(|row| row.date >= year_ago)
            .filter_map(|row| converted(rates, row, &currency))
            .sum();

        let theirs: Vec<_> = found_subscriptions
            .subscriptions
            .iter()
            .filter(|subscription| subscription.vendor_key == key)
            .collect();

        let monthly: Decimal = theirs
            .iter()
            .filter(|subscription| subscription.status == "active")
            .filter_map(|subscription| subscription.per_month)
            .sum();

        let accounts: BTreeSet<String> =
            rows.iter().filter_map(|row| row.account.clone()).collect();

        let mut spent_year = spent_year.round_dp(2);
        let mut monthly = monthly.round_dp(2);

        spent_year.rescale(2);
        monthly.rescale(2);

        list.push(VendorSummary {
            id: key.to_string(),
            name: latest.vendor_name.clone(),
            category: rows.iter().find_map(|row| row.category.clone()),
            domain: rows.iter().find_map(|row| row.vendor_domain.clone()),
            known: rows.iter().any(|row| row.category.is_some()),
            spent_year,
            monthly,
            receipts: paid.len(),
            first_paid: paid.iter().map(|row| row.date.clone()).min(),
            last_paid: paid.iter().map(|row| row.date.clone()).max(),
            subscriptions: theirs.len(),
            months: months_of(&rows, &paid, &currency, today, rates),
            cards: cards_of(&rows),
            accounts: accounts.into_iter().collect(),
            personal: rows.iter().all(|row| row.personal),
            ignored: rows.iter().all(|row| row.ignored),
            document_ids: rows.iter().map(|row| row.id).collect(),
        });
    }

    if !options.all {
        list.retain(|vendor| !vendor.personal && !vendor.ignored);
    }

    // The most spent on first; then by name.
    list.sort_by(|a, b| {
        b.spent_year
            .cmp(&a.spent_year)
            .then_with(|| a.name.cmp(&b.name))
    });

    Vendors {
        currency,
        vendors: list,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;
    use crate::rates::RateTable;
    use crate::spend::StoredRow;

    fn row(id: i64, date: &str, vendor: &str, total: &str) -> StoredRow {
        StoredRow {
            id,
            date: date.into(),
            kind: "receipt".into(),
            status: "paid".into(),
            is_receipt: 1,
            vendor_name: vendor.into(),
            total: Some(total.into()),
            currency: Some("USD".into()),
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

    fn options() -> VendorOptions {
        VendorOptions {
            today: "2026-11-21".into(),
            ..VendorOptions::default()
        }
    }

    fn found(rows: &[Row]) -> Vendors {
        vendors(rows, &options(), &RateTable::default())
    }

    #[test]
    fn works_out_what_each_vendor_cost_month_by_month() {
        let mut usage = row(3, "2026-11-05", "Anthropic", "412.50");

        usage.billing_model = Some("usage".into());
        usage.interval = None;

        let mut seats = row(2, "2026-11-19", "Vercel", "40.00");

        seats.seats = Some(2.0);
        seats.card = Some("visa 3776".into());

        let rows = rows(vec![
            seats,
            usage,
            row(1, "2026-10-19", "Vercel", "20.00"),
            row(4, "2024-01-01", "Vercel", "20.00"),
        ]);

        let found = found(&rows);

        let names: Vec<&str> = found.vendors.iter().map(|v| v.name.as_str()).collect();

        assert_eq!(names, ["Anthropic", "Vercel"]);

        let vercel = &found.vendors[1];

        assert_eq!(vercel.id, "vercel.com");
        assert_eq!(vercel.domain.as_deref(), Some("vercel.com"));
        assert!(vercel.known);

        // The receipt from 2024 is outside the year, but it's paid.
        assert_eq!(vercel.spent_year, Decimal::new(6000, 2));
        assert_eq!(vercel.receipts, 3);
        assert_eq!(vercel.first_paid.as_deref(), Some("2024-01-01"));
        assert_eq!(vercel.subscriptions, 1);
        assert_eq!(vercel.monthly, Decimal::new(4000, 2));

        assert_eq!(vercel.months.len(), 12);
        assert_eq!(vercel.months[11].month, "2026-11");
        assert_eq!(vercel.months[11].subscription, Decimal::new(4000, 2));
        assert_eq!(vercel.months[11].seats, Some(2.0));
        assert_eq!(vercel.months[10].subscription, Decimal::new(2000, 2));

        assert_eq!(vercel.cards[0].card, "visa 3776");

        let anthropic = &found.vendors[0];

        assert_eq!(anthropic.months[11].usage, Decimal::new(41250, 2));

        // Billed by use, month after month: a subscription too, as the CLI has it.
        assert_eq!(anthropic.subscriptions, 1);
    }

    #[test]
    fn applies_what_people_decided_about_vendors() {
        let edits = vec![
            VendorEdit {
                vendor: crate::catalog::name_key("Whiteboard Studio Ltd"),
                merge_into: Some("miro.com".into()),
                ..VendorEdit::default()
            },
            VendorEdit {
                vendor: "miro.com".into(),
                name: Some("Miro (design)".into()),
                category: Some("design".into()),
                ..VendorEdit::default()
            },
            VendorEdit {
                vendor: "netflix".into(),
                personal: true,
                ..VendorEdit::default()
            },
        ];

        let stored = vec![
            row(3, "2026-11-10", "Netflix", "15.00"),
            row(2, "2026-11-05", "Miro", "16.00"),
            row(1, "2026-10-05", "Whiteboard Studio Ltd", "16.00"),
        ];

        let applied = apply_vendor_edits(rows(stored), &edits);

        // Applying them again changes nothing more.
        let again = apply_vendor_edits(applied.clone(), &edits);

        assert_eq!(
            applied.iter().map(|r| &r.vendor_key).collect::<Vec<_>>(),
            again.iter().map(|r| &r.vendor_key).collect::<Vec<_>>()
        );

        let found = found(&applied);

        // Netflix is personal: left out. Whiteboard Studio is Miro now.
        assert_eq!(found.vendors.len(), 1);
        assert_eq!(found.vendors[0].id, "miro.com");
        assert_eq!(found.vendors[0].name, "Miro (design)");
        assert_eq!(found.vendors[0].category.as_deref(), Some("design"));
        assert_eq!(found.vendors[0].receipts, 2);
        assert_eq!(found.vendors[0].document_ids, [2, 1]);

        let all = vendors(
            &applied,
            &VendorOptions {
                all: true,
                ..options()
            },
            &RateTable::default(),
        );

        assert!(all.vendors.iter().any(|v| v.id == "netflix" && v.personal));
    }

    #[test]
    fn points_at_a_vendor_as_vendors_have_it() {
        let edits = [VendorEdit {
            vendor: crate::catalog::name_key("Whiteboard Studio Ltd"),
            merge_into: Some("miro.com".into()),
            ..VendorEdit::default()
        }];

        assert_eq!(
            vendor_ref(Some("vercel.com"), "Vercel Inc.", None, &[]),
            VendorRef {
                id: "vercel.com".into(),
                name: "Vercel".into(),
                domain: Some("vercel.com".into())
            }
        );

        // Merged into a vendor the catalog knows: that one, as the catalog has it.
        assert_eq!(
            vendor_ref(None, "Whiteboard Studio Ltd", None, &edits),
            VendorRef {
                id: "miro.com".into(),
                name: "Miro".into(),
                domain: Some("miro.com".into())
            }
        );

        // A receipt's own correction stays its name, the vendor its catalog's.
        assert_eq!(
            vendor_ref(Some("vercel.com"), "Vercel", Some("Vercel (team)"), &[]).name,
            "Vercel (team)"
        );

        // Unknown, with a domain read: the name's key, and that domain.
        assert_eq!(
            vendor_ref(Some("tiny.example"), "Tiny Tool", None, &[])
                .domain
                .as_deref(),
            Some("tiny.example")
        );
    }

    #[test]
    fn follows_merges_but_not_round_in_a_loop() {
        let edits = [
            VendorEdit {
                vendor: "a".into(),
                merge_into: Some("b".into()),
                ..VendorEdit::default()
            },
            VendorEdit {
                vendor: "b".into(),
                merge_into: Some("a".into()),
                ..VendorEdit::default()
            },
        ];

        let by_vendor: BTreeMap<&str, &VendorEdit> = edits
            .iter()
            .map(|edit| (edit.vendor.as_str(), edit))
            .collect();

        // It stops; where doesn't matter, only that it does.
        assert!(["a", "b"].contains(&merged("a", &by_vendor).as_str()));
    }
}
