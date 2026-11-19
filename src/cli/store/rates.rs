//! Exchange rates from the European Central Bank's daily reference rates,
//! kept in the store. Amounts convert through the euro, using the newest
//! rate on or before the receipt's date.

use std::collections::BTreeSet;
use std::str::FromStr;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

use super::Store;

const ECB: &str = "https://data-api.ecb.europa.eu/service/data/EXR";

impl Store {
    /// Fetches rates for `currencies` from `since` onwards, where the store
    /// doesn't have them yet. Returns how many rates were added.
    pub async fn update_rates(&self, currencies: &BTreeSet<String>, since: &str) -> Result<usize> {
        let wanted: Vec<&String> = currencies
            .iter()
            .filter(|currency| currency.as_str() != "EUR")
            .collect();

        if wanted.is_empty() {
            return Ok(0);
        }

        let newest: Option<String> = self
            .connection
            .query_row("SELECT max(date) FROM rates", [], |row| row.get(0))
            .optional()?
            .flatten();

        let known: Vec<String> = {
            let mut statement = self
                .connection
                .prepare("SELECT DISTINCT currency FROM rates")?;

            statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?
        };

        let all_known = wanted.iter().all(|currency| known.contains(currency));

        let start = match (&newest, all_known) {
            (Some(newest), true) if newest.as_str() >= since => newest.clone(),
            _ => since.to_string(),
        };

        let keys = wanted
            .iter()
            .map(|currency| currency.as_str())
            .collect::<Vec<_>>()
            .join("+");

        let url = format!("{ECB}/D.{keys}.EUR.SP00.A?format=csvdata&startPeriod={start}");

        let csv = reqwest::get(&url)
            .await
            .context("fetching exchange rates from the ECB")?
            .error_for_status()?
            .text()
            .await?;

        let mut added = 0;

        let transaction = self.connection.unchecked_transaction()?;

        let mut lines = csv.lines();

        let header: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();

        let column = |name: &str| header.iter().position(|field| *field == name);

        let (Some(currency_at), Some(date_at), Some(value_at)) = (
            column("CURRENCY"),
            column("TIME_PERIOD"),
            column("OBS_VALUE"),
        ) else {
            anyhow::bail!("the ECB's answer wasn't the CSV expected");
        };

        // The fields needed come before any quoted ones, so a plain split
        // is enough for them.
        for line in lines {
            let fields: Vec<&str> = line.split(',').collect();

            let (Some(currency), Some(date), Some(value)) = (
                fields.get(currency_at),
                fields.get(date_at),
                fields.get(value_at),
            ) else {
                continue;
            };

            let Ok(per_eur) = value.parse::<f64>() else {
                continue;
            };

            added += transaction.execute(
                "INSERT OR IGNORE INTO rates (date, currency, per_eur) VALUES (?1, ?2, ?3)",
                params![date, currency, per_eur],
            )?;
        }

        transaction.commit()?;

        Ok(added)
    }

    /// Units of `currency` per euro on `date`, or the newest rate before it.
    fn per_eur(&self, currency: &str, date: &str) -> Result<Option<f64>> {
        if currency == "EUR" {
            return Ok(Some(1.0));
        }

        let rate = self
            .connection
            .query_row(
                "SELECT per_eur FROM rates WHERE currency = ?1 AND date <= ?2 ORDER BY date DESC LIMIT 1",
                params![currency, date],
                |row| row.get(0),
            )
            .optional()?;

        Ok(rate)
    }

    /// `amount` in `from` converted to `to`, at the rates of `date`. `None`
    /// when a rate is missing.
    pub fn convert(
        &self,
        amount: Decimal,
        from: &str,
        to: &str,
        date: &str,
    ) -> Result<Option<Decimal>> {
        if from == to {
            return Ok(Some(amount));
        }

        let (Some(from_rate), Some(to_rate)) = (self.per_eur(from, date)?, self.per_eur(to, date)?)
        else {
            return Ok(None);
        };

        let euros = amount.to_f64().unwrap_or(0.0) / from_rate;

        let converted = Decimal::from_f64(euros * to_rate).map(|value| value.round_dp(2));

        Ok(converted)
    }
}

/// Parses a stored amount.
pub fn amount(text: &str) -> Option<Decimal> {
    Decimal::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_through_the_euro_with_the_rate_on_or_before_the_date() {
        let dir = std::env::temp_dir().join(format!("spendyard-rates-{}", std::process::id()));

        let store = Store::open_at(&dir.join("test.db")).unwrap();

        for (date, currency, rate) in [
            ("2026-11-10", "USD", 1.10),
            ("2026-11-10", "NOK", 11.0),
            ("2026-11-12", "USD", 1.20),
        ] {
            store
                .connection
                .execute(
                    "INSERT INTO rates VALUES (?1, ?2, ?3)",
                    params![date, currency, rate],
                )
                .unwrap();
        }

        let converted = store
            .convert(Decimal::from(110), "USD", "NOK", "2026-11-11")
            .unwrap();

        assert_eq!(converted, Some(Decimal::from(1100)));

        let euros = store
            .convert(Decimal::from(120), "USD", "EUR", "2026-11-13")
            .unwrap();

        assert_eq!(euros, Some(Decimal::from(100)));

        assert_eq!(
            store
                .convert(Decimal::from(1), "USD", "JPY", "2026-11-13")
                .unwrap(),
            None
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
