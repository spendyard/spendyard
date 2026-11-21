//! Exchange rates from the European Central Bank's daily reference rates,
//! kept in the store. Amounts convert through the euro, using the newest
//! rate on or before the receipt's date.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use rust_decimal::Decimal;

use super::Store;

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

        let Some(url) = spendyard_core::rates::ecb_url(currencies, &start) else {
            return Ok(0);
        };

        let csv = reqwest::get(&url)
            .await
            .context("fetching exchange rates from the ECB")?
            .error_for_status()?
            .text()
            .await?;

        let Some(rates) = spendyard_core::rates::parse_ecb_csv(&csv) else {
            anyhow::bail!("the ECB's answer wasn't the CSV expected");
        };

        let mut added = 0;

        let transaction = self.connection.unchecked_transaction()?;

        for rate in rates {
            added += transaction.execute(
                "INSERT OR IGNORE INTO rates (date, currency, per_eur) VALUES (?1, ?2, ?3)",
                params![rate.date, rate.currency, rate.per_eur],
            )?;
        }

        transaction.commit()?;

        Ok(added)
    }

    /// Units of `currency` per euro on `date`, or the newest rate before it.
    fn per_eur_stored(&self, currency: &str, date: &str) -> Result<Option<f64>> {
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
        Ok(spendyard_core::rates::convert(self, amount, from, to, date))
    }
}

/// The store's rates, for the core's conversions. A rate it can't read is missing.
impl spendyard_core::rates::Rates for Store {
    fn per_eur(&self, currency: &str, date: &str) -> Option<f64> {
        self.per_eur_stored(currency, date).ok().flatten()
    }
}

/// Parses a stored amount.
pub use spendyard_core::spend::amount;

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
