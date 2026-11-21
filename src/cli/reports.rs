//! `spendyard receipts`, `report`, `subscriptions` and `edit`: what the
//! store says about spending.
//!
//! Spend counts money paid: receipts and paid invoices, not failed or due
//! ones, not those marked personal or ignored, and each receipt once even
//! when it was forwarded or saved twice.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use rusqlite::params;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;

use super::store::rates::amount;
use super::store::{SAME_PAYMENT_DAYS, Store, day_number, now};

/// One stored document's report row.
#[derive(Debug, Clone, Serialize)]
struct Row {
    id: i64,
    date: String,
    kind: String,
    status: String,
    is_receipt: bool,
    vendor_key: String,
    vendor_name: String,
    total: Option<String>,
    currency: Option<String>,
    billing_model: Option<String>,
    interval: Option<String>,
    next_renewal: Option<String>,
    number: Option<String>,
    card: Option<String>,
    seats: Option<f64>,
    flags: i64,
    personal: bool,
    ignored: bool,
    subject: Option<String>,
}

fn rows(store: &Store) -> Result<Vec<Row>> {
    let mut statement = store.connection.prepare(
        "SELECT d.id, coalesce(r.date, substr(d.received, 1, 10), ''), coalesce(r.type, ''), coalesce(r.status, ''),
                coalesce(r.is_receipt, 0),
                r.vendor_id,
                coalesce(r.vendor_name, ''),
                r.total, r.currency, r.billing_model, r.interval, r.next_renewal,
                coalesce(r.invoice_number, r.receipt_number),
                CASE WHEN r.card_last4 IS NOT NULL THEN coalesce(r.card_brand, 'card') || ' ' || r.card_last4 END,
                r.seats, coalesce(r.flags, 0),
                coalesce(e.personal, 0), coalesce(e.ignored, 0), d.name, e.vendor_name
         FROM documents d
         JOIN receipts r ON r.document_id = d.id
         LEFT JOIN edits e ON e.document_id = d.id
         ORDER BY 2 DESC, d.id DESC",
    )?;

    let catalog = spendyard_core::catalog::catalog();

    let mapped = statement.query_map([], |row| {
        let vendor_id: Option<String> = row.get(5)?;
        let read_name: String = row.get(6)?;
        let edited_name: Option<String> = row.get(19)?;

        // An edit wins; then the catalog, by ID or name; then the name.
        let (vendor_key, vendor_name) = match edited_name {
            Some(edited) => (catalog_key(catalog, None, &edited), edited),

            None => match catalog.find(vendor_id.as_deref(), Some(&read_name)) {
                Some(vendor) => (vendor.id.clone(), vendor.name.clone()),
                None => (spendyard_core::catalog::name_key(&read_name), read_name),
            },
        };

        Ok(Row {
            id: row.get(0)?,
            date: row.get(1)?,
            kind: row.get(2)?,
            status: row.get(3)?,
            is_receipt: row.get::<_, i64>(4)? != 0,
            vendor_key,
            vendor_name,
            total: row.get(7)?,
            currency: row.get(8)?,
            billing_model: row.get(9)?,
            interval: row.get(10)?,
            next_renewal: row.get(11)?,
            number: row.get(12)?,
            card: row.get(13)?,
            seats: row.get(14)?,
            flags: row.get(15)?,
            personal: row.get::<_, i64>(16)? != 0,
            ignored: row.get::<_, i64>(17)? != 0,
            subject: row.get(18)?,
        })
    })?;

    Ok(mapped.collect::<Result<_, _>>()?)
}

/// The grouping key for a name: the catalog ID when the catalog knows the
/// name, else the name without legal suffixes.
fn catalog_key(catalog: &spendyard_core::catalog::Catalog, id: Option<&str>, name: &str) -> String {
    match catalog.find(id, Some(name)) {
        Some(vendor) => vendor.id.clone(),
        None => spendyard_core::catalog::name_key(name),
    }
}

/// Paid receipts that count as spend, each once.
fn spend(rows: &[Row]) -> Vec<Row> {
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

fn days_apart(a: &str, b: &str) -> Option<i64> {
    Some((day_number(a)? - day_number(b)?).abs())
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

/// The currency to report in: the flag, or the most common one paid in.
fn report_currency(flag: Option<String>, rows: &[Row]) -> String {
    if let Some(currency) = flag {
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

/// Makes sure the store has the rates to convert `rows` into `target`.
async fn ensure_rates(store: &Store, rows: &[Row], target: &str) -> Result<()> {
    let mut currencies: BTreeSet<String> =
        rows.iter().filter_map(|row| row.currency.clone()).collect();

    currencies.insert(target.to_string());

    if currencies.len() <= 1 {
        return Ok(());
    }

    let earliest = rows
        .iter()
        .map(|row| row.date.as_str())
        .filter(|date| date.len() == 10)
        .min();

    let Some(earliest) = earliest else {
        return Ok(());
    };

    // A few days earlier, so the first date has a rate on or before it.
    let since = shift_date(earliest, 0, -7);

    match store.update_rates(&currencies, &since).await {
        Ok(added) if added > 0 => {
            eprintln!("spendyard: fetched {added} exchange rates from the ECB")
        }
        Ok(_) => {}
        Err(error) => eprintln!(
            "spendyard: no new exchange rates ({error:#}); converting with what the store has"
        ),
    }

    Ok(())
}

/// A row's total in `target`, or `None` when it can't be converted.
fn converted(store: &Store, row: &Row, target: &str) -> Result<Option<Decimal>> {
    let (Some(total), Some(currency)) = (
        row.total.as_deref().and_then(amount),
        row.currency.as_deref(),
    ) else {
        return Ok(None);
    };

    store.convert(total, currency, target, &row.date)
}

fn today() -> String {
    now().chars().take(10).collect()
}

/// `YYYY-MM-DD` moved by whole months and days.
fn shift_date(date: &str, months: i32, days: i32) -> String {
    let parts: Vec<i32> = date
        .split('-')
        .filter_map(|part| part.parse().ok())
        .collect();

    let [year, month, day] = parts[..] else {
        return date.to_string();
    };

    let total_months = year * 12 + (month - 1) + months;

    let (year, month) = (total_months.div_euclid(12), total_months.rem_euclid(12) + 1);

    let shifted = format!("{year:04}-{month:02}-{:02}", day.min(28));

    if days == 0 {
        return shifted;
    }

    let epoch_days = days_from_civil(year, month as u32, day.min(28) as u32) + days as i64;

    let (year, month, day) = super::store::civil_from_days(epoch_days);

    format!("{year:04}-{month:02}-{day:02}")
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);

    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);

    let month_index = if month > 2 { month - 3 } else { month + 9 } as i64;

    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;

    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

fn money(value: Decimal) -> String {
    let rounded = value.round_dp(2);

    let text = format!("{rounded:.2}");

    let (whole, cents) = text.split_once('.').unwrap_or((&text, "00"));

    let negative = whole.starts_with('-');

    let digits: Vec<char> = whole.trim_start_matches('-').chars().collect();

    let mut grouped = String::new();

    for (index, digit) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(' ');
        }

        grouped.push(*digit);
    }

    format!("{}{grouped}.{cents}", if negative { "-" } else { "" })
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}

pub struct ReceiptsOptions {
    pub vendor: Option<String>,
    pub since: Option<String>,
    pub limit: usize,
    pub all: bool,
    pub json: bool,
}

/// Lists stored documents, newest first: receipts and invoices by
/// default, everything with `--all`.
pub fn receipts(options: ReceiptsOptions) -> Result<()> {
    let store = Store::open()?;

    let rows = rows(&store)?;

    let vendor = options.vendor.map(|vendor| vendor.to_lowercase());

    let shown: Vec<&Row> = rows
        .iter()
        .filter(|row| {
            options.all
                || row.is_receipt
                || matches!(
                    row.kind.as_str(),
                    "invoice" | "renewal_notice" | "payment_failed"
                )
        })
        .filter(|row| {
            vendor.as_ref().is_none_or(|vendor| {
                row.vendor_key.contains(vendor) || row.vendor_name.to_lowercase().contains(vendor)
            })
        })
        .filter(|row| {
            options
                .since
                .as_ref()
                .is_none_or(|since| row.date.as_str() >= since.as_str())
        })
        .take(options.limit)
        .collect();

    if options.json {
        for row in shown {
            println!("{}", serde_json::to_string(row)?);
        }

        return Ok(());
    }

    if shown.is_empty() {
        println!("nothing stored yet; `spendyard read` and `spendyard scan` fill the store");

        return Ok(());
    }

    for row in shown {
        let total = match (&row.total, &row.currency) {
            (Some(total), Some(currency)) => format!("{total} {currency}"),
            (Some(total), None) => total.clone(),
            _ => String::new(),
        };

        let mut marks = Vec::new();

        if row.personal {
            marks.push("personal");
        }

        if row.ignored {
            marks.push("ignored");
        }

        if row.flags > 0 {
            marks.push("flagged");
        }

        println!(
            "{:>5}  {:<10}  {:<14} {:<8} {:<26} {:>16}  {:<18} {}",
            row.id,
            row.date,
            clip(&row.kind, 14),
            clip(&row.status, 8),
            clip(&row.vendor_name, 26),
            total,
            clip(row.card.as_deref().unwrap_or(""), 18),
            marks.join(", ")
        );
    }

    Ok(())
}

pub struct ReportOptions {
    pub by: String,
    pub months: u32,
    pub currency: Option<String>,
    pub json: bool,
}

/// Spending per vendor or per month over the last `months` months.
pub async fn report(options: ReportOptions) -> Result<()> {
    let store = Store::open()?;

    let since = shift_date(&today(), -(options.months as i32), 0);

    let paid: Vec<Row> = spend(&rows(&store)?)
        .into_iter()
        .filter(|row| row.date >= since)
        .collect();

    if paid.is_empty() {
        println!("no paid receipts since {since}");

        return Ok(());
    }

    let currency = report_currency(options.currency, &paid);

    ensure_rates(&store, &paid, &currency).await?;

    let mut groups: BTreeMap<String, (String, Decimal, usize, String)> = BTreeMap::new();

    let mut unconverted = 0;
    let mut no_currency = 0;

    for row in &paid {
        if row.currency.is_none() {
            no_currency += 1;

            continue;
        }

        let Some(value) = converted(&store, row, &currency)? else {
            unconverted += 1;

            continue;
        };

        let key = match options.by.as_str() {
            "month" => row.date.chars().take(7).collect(),
            "vendor" => row.vendor_key.clone(),
            other => bail!("--by {other}: use vendor or month"),
        };

        let label = if options.by == "month" {
            key.clone()
        } else {
            row.vendor_name.clone()
        };

        let entry = groups
            .entry(key)
            .or_insert((label, Decimal::ZERO, 0, String::new()));

        entry.1 += value;
        entry.2 += 1;

        if row.date > entry.3 {
            entry.3 = row.date.clone();
        }
    }

    let mut ordered: Vec<(String, Decimal, usize, String)> = groups.into_values().collect();

    if options.by == "month" {
        ordered.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        ordered.sort_by_key(|group| std::cmp::Reverse(group.1));
    }

    let total: Decimal = ordered.iter().map(|group| group.1).sum();

    if options.json {
        let groups: Vec<_> = ordered
            .iter()
            .map(|(label, value, count, last)| json!({ "group": label, "total": value.round_dp(2), "receipts": count, "last": last }))
            .collect();

        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "currency": currency, "since": since, "by": options.by, "total": total.round_dp(2), "groups": groups })
            )?
        );

        return Ok(());
    }

    println!("Spend since {since}, in {currency}, by {}", options.by);
    println!();

    for (label, value, count, last) in &ordered {
        let share = if total > Decimal::ZERO {
            (*value / total * Decimal::from(100)).round_dp(0)
        } else {
            Decimal::ZERO
        };

        let detail = if options.by == "month" {
            String::new()
        } else {
            format!("last {last}")
        };

        println!(
            "  {:<32} {:>14} {:>4}%  {:>4} receipts  {detail}",
            clip(label, 32),
            money(*value),
            share,
            count
        );
    }

    println!();
    println!(
        "  {:<32} {:>14}         {:>4} receipts",
        "Total",
        money(total),
        paid.len() - unconverted - no_currency
    );

    if no_currency > 0 {
        println!();
        println!(
            "  {no_currency} receipts left out: no currency was read from them (`spendyard receipts --all` shows them)"
        );
    }

    if unconverted > 0 {
        println!();
        println!("  {unconverted} receipts left out: no exchange rate for their currency and date");
    }

    Ok(())
}

pub struct SubscriptionsOptions {
    pub currency: Option<String>,
    pub json: bool,

    /// Also show subscriptions lapsed for more than a year.
    pub all: bool,
}

#[derive(Serialize)]
struct Subscription {
    vendor: String,
    interval: String,
    last_paid: Option<String>,
    last_amount: Option<String>,
    per_month: Option<Decimal>,
    seats: Option<f64>,
    card: Option<String>,
    next_renewal: Option<String>,
    status: String,
}

/// Recurring vendors: what each costs, how often, on which card, and
/// whether the last payment went through.
pub async fn subscriptions(options: SubscriptionsOptions) -> Result<()> {
    let store = Store::open()?;

    let all = rows(&store)?;

    let recurring: Vec<&Row> = all
        .iter()
        .filter(|row| !row.personal && !row.ignored)
        .filter(|row| {
            matches!(
                row.billing_model.as_deref(),
                Some("subscription" | "mixed" | "usage")
            ) || matches!(
                row.interval.as_deref(),
                Some("month" | "year" | "quarter" | "week")
            )
        })
        .collect();

    let paid = spend(&all);

    let currency = report_currency(options.currency, &paid);

    ensure_rates(&store, &paid, &currency).await?;

    let mut by_vendor: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();

    for row in &recurring {
        by_vendor
            .entry(row.vendor_key.as_str())
            .or_default()
            .push(row);
    }

    let today = today();

    let mut subscriptions = Vec::new();

    for rows in by_vendor.values() {
        // Rows come newest first.
        let latest = rows[0];

        let last_paid = rows
            .iter()
            .find(|row| row.is_receipt && row.total.is_some());

        let interval = rows
            .iter()
            .find_map(|row| row.interval.clone())
            .unwrap_or_else(|| "?".to_string());

        let per_month = match last_paid {
            Some(row) => converted(&store, row, &currency)?.map(|value| match interval.as_str() {
                "year" => (value / Decimal::from(12)).round_dp(2),
                "quarter" => (value / Decimal::from(3)).round_dp(2),
                "week" => (value * Decimal::new(433, 2)).round_dp(2),
                _ => value,
            }),
            None => None,
        };

        let estimated_renewal = last_paid.and_then(|row| match interval.as_str() {
            "month" => Some(shift_date(&row.date, 1, 0)),
            "year" => Some(shift_date(&row.date, 12, 0)),
            "quarter" => Some(shift_date(&row.date, 3, 0)),
            _ => None,
        });

        let next_renewal = rows
            .iter()
            .find_map(|row| row.next_renewal.clone())
            .filter(|date| *date >= today)
            .or(estimated_renewal);

        let status =
            if matches!(latest.kind.as_str(), "payment_failed") || latest.status == "failed" {
                "payment failed".to_string()
            } else if latest.status == "due" && !latest.is_receipt {
                "due".to_string()
            } else if let Some(row) = last_paid {
                let grace = match interval.as_str() {
                    "year" => 14,
                    "quarter" => 4,
                    _ => 2,
                };

                if shift_date(&row.date, grace, 0) < today {
                    "lapsed?".to_string()
                } else {
                    "active".to_string()
                }
            } else {
                "unpaid".to_string()
            };

        subscriptions.push(Subscription {
            vendor: latest.vendor_name.clone(),
            interval,
            last_paid: last_paid.map(|row| row.date.clone()),
            last_amount: last_paid.and_then(|row| {
                Some(format!(
                    "{} {}",
                    row.total.as_deref()?,
                    row.currency.as_deref().unwrap_or("")
                ))
            }),
            per_month,
            seats: rows.iter().find_map(|row| row.seats),
            card: rows.iter().find_map(|row| row.card.clone()),
            next_renewal,
            status,
        });
    }

    // Long-gone tools only with --all.
    if !options.all {
        let year_ago = shift_date(&today, -12, 0);

        subscriptions.retain(|subscription| {
            subscription.status != "lapsed?"
                || subscription
                    .last_paid
                    .as_deref()
                    .is_some_and(|date| date >= year_ago.as_str())
        });
    }

    // Live ones first, most expensive first.
    subscriptions.sort_by(|a, b| {
        let live = |subscription: &Subscription| {
            subscription.status == "active" || subscription.status == "payment failed"
        };

        live(b).cmp(&live(a)).then(
            b.per_month
                .unwrap_or_default()
                .cmp(&a.per_month.unwrap_or_default()),
        )
    });

    if options.json {
        for subscription in &subscriptions {
            println!("{}", serde_json::to_string(subscription)?);
        }

        return Ok(());
    }

    if subscriptions.is_empty() {
        println!("no recurring vendors in the store yet");

        return Ok(());
    }

    let monthly: Decimal = subscriptions
        .iter()
        .filter(|subscription| subscription.status == "active")
        .filter_map(|subscription| subscription.per_month)
        .sum();

    println!(
        "{:<28} {:<7} {:>12} {:>6} {:<11} {:<11} {:<17} Status",
        "Vendor",
        "Every",
        &format!("{currency}/month"),
        "Seats",
        "Last paid",
        "Next",
        "Card",
    );

    for subscription in &subscriptions {
        println!(
            "{:<28} {:<7} {:>12} {:>6} {:<11} {:<11} {:<17} {}",
            clip(&subscription.vendor, 28),
            subscription.interval,
            subscription.per_month.map(money).unwrap_or_default(),
            subscription
                .seats
                .map(|seats| format!("{seats}"))
                .unwrap_or_default(),
            subscription.last_paid.as_deref().unwrap_or(""),
            subscription.next_renewal.as_deref().unwrap_or(""),
            clip(subscription.card.as_deref().unwrap_or(""), 17),
            subscription.status
        );
    }

    println!();
    println!(
        "Active subscriptions: about {} {currency} a month",
        money(monthly)
    );

    Ok(())
}

pub struct EditOptions {
    pub id: i64,
    pub personal: Option<bool>,
    pub ignore: Option<bool>,
    pub vendor: Option<String>,
    pub note: Option<String>,
}

/// Marks a stored receipt by hand. Edits survive re-reading.
pub fn edit(options: EditOptions) -> Result<()> {
    let store = Store::open()?;

    let exists: i64 = store.connection.query_row(
        "SELECT count(*) FROM documents WHERE id = ?1",
        params![options.id],
        |row| row.get(0),
    )?;

    if exists == 0 {
        bail!(
            "no document {} in the store; `spendyard receipts` lists them",
            options.id
        );
    }

    store.connection.execute(
        "INSERT INTO edits (document_id, updated) VALUES (?1, ?2) ON CONFLICT (document_id) DO NOTHING",
        params![options.id, now()],
    )?;

    if let Some(personal) = options.personal {
        store.connection.execute(
            "UPDATE edits SET personal = ?2 WHERE document_id = ?1",
            params![options.id, personal],
        )?;
    }

    if let Some(ignore) = options.ignore {
        store.connection.execute(
            "UPDATE edits SET ignored = ?2 WHERE document_id = ?1",
            params![options.id, ignore],
        )?;
    }

    if let Some(vendor) = &options.vendor {
        store.connection.execute(
            "UPDATE edits SET vendor_name = ?2 WHERE document_id = ?1",
            params![options.id, vendor],
        )?;
    }

    if let Some(note) = &options.note {
        store.connection.execute(
            "UPDATE edits SET note = ?2 WHERE document_id = ?1",
            params![options.id, note],
        )?;
    }

    store.connection.execute(
        "UPDATE edits SET updated = ?2 WHERE document_id = ?1",
        params![options.id, now()],
    )?;

    println!("edited {}", options.id);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_shift() {
        assert_eq!(shift_date("2026-11-19", 1, 0), "2026-12-19");
        assert_eq!(shift_date("2026-12-19", 1, 0), "2027-01-19");
        assert_eq!(shift_date("2026-01-31", 1, 0), "2026-02-28");
        assert_eq!(shift_date("2026-11-19", -12, 0), "2025-11-19");
        assert_eq!(shift_date("2026-03-03", 0, -7), "2026-02-24");
    }

    #[test]
    fn money_groups_thousands() {
        assert_eq!(money(Decimal::new(123456789, 2)), "1 234 567.89");
        assert_eq!(money(Decimal::new(500, 2)), "5.00");
    }
}
