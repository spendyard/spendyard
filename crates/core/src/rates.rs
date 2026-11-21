//! Exchange rates: the European Central Bank's daily reference rates, how
//! to ask for them and read its answer, and converting through the euro
//! with the newest rate on or before a day. Where rates are kept is the
//! host's: the CLI's store, the cloud's database.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};

/// The ECB's data API for exchange rates.
pub const ECB: &str = "https://data-api.ecb.europa.eu/service/data/EXR";

/// One day's rate: how many of a currency one euro bought.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub date: String,
    pub currency: String,
    pub per_eur: f64,
}

/// Where rates come from: units of `currency` per euro on `date`, or the
/// newest before it.
pub trait Rates {
    fn per_eur(&self, currency: &str, date: &str) -> Option<f64>;
}

/// The URL for the daily rates of `currencies` from `start` on, as CSV.
/// `None` when there's nothing to ask for: the euro needs no rate.
pub fn ecb_url(currencies: &BTreeSet<String>, start: &str) -> Option<String> {
    let wanted: Vec<&str> = currencies
        .iter()
        .map(String::as_str)
        .filter(|currency| *currency != "EUR")
        .collect();

    if wanted.is_empty() {
        return None;
    }

    let keys = wanted.join("+");

    Some(format!(
        "{ECB}/D.{keys}.EUR.SP00.A?format=csvdata&startPeriod={start}"
    ))
}

/// The rates in the ECB's CSV answer. `None` when it isn't the CSV expected.
pub fn parse_ecb_csv(csv: &str) -> Option<Vec<Rate>> {
    let mut lines = csv.lines();

    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();

    let column = |name: &str| header.iter().position(|field| *field == name);

    let currency_at = column("CURRENCY")?;
    let date_at = column("TIME_PERIOD")?;
    let value_at = column("OBS_VALUE")?;

    let mut rates = Vec::new();

    // The fields needed come before any quoted ones, so a plain split is
    // enough for them.
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

        rates.push(Rate {
            date: date.to_string(),
            currency: currency.to_string(),
            per_eur,
        });
    }

    Some(rates)
}

/// `amount` in `from` converted to `to`, at the rates of `date`. `None`
/// when a rate is missing.
pub fn convert(
    rates: &impl Rates,
    amount: Decimal,
    from: &str,
    to: &str,
    date: &str,
) -> Option<Decimal> {
    if from == to {
        return Some(amount);
    }

    let from_rate = per_eur(rates, from, date)?;
    let to_rate = per_eur(rates, to, date)?;

    let euros = amount.to_f64().unwrap_or(0.0) / from_rate;

    Decimal::from_f64(euros * to_rate).map(|value| value.round_dp(2))
}

fn per_eur(rates: &impl Rates, currency: &str, date: &str) -> Option<f64> {
    if currency == "EUR" {
        return Some(1.0);
    }

    rates.per_eur(currency, date)
}

/// Rates in memory: what a host hands over, such as the cloud's.
#[derive(Debug, Default)]
pub struct RateTable {
    /// Each currency's rates by day.
    by_currency: BTreeMap<String, BTreeMap<String, f64>>,
}

impl RateTable {
    pub fn new(rates: impl IntoIterator<Item = Rate>) -> RateTable {
        let mut table = RateTable::default();

        for rate in rates {
            table
                .by_currency
                .entry(rate.currency)
                .or_default()
                .insert(rate.date, rate.per_eur);
        }

        table
    }
}

impl Rates for RateTable {
    fn per_eur(&self, currency: &str, date: &str) -> Option<f64> {
        let days = self.by_currency.get(currency)?;

        days.range(..=date.to_string())
            .next_back()
            .map(|(_, rate)| *rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> RateTable {
        RateTable::new([
            Rate {
                date: "2026-11-17".into(),
                currency: "USD".into(),
                per_eur: 1.10,
            },
            Rate {
                date: "2026-11-17".into(),
                currency: "NOK".into(),
                per_eur: 11.0,
            },
            Rate {
                date: "2026-11-19".into(),
                currency: "USD".into(),
                per_eur: 1.20,
            },
        ])
    }

    #[test]
    fn converts_through_the_euro_with_the_rate_on_or_before_the_day() {
        let twenty = Decimal::new(2000, 2);

        // The 18th has no rate: the 17th's.
        assert_eq!(
            convert(&table(), twenty, "USD", "NOK", "2026-11-18"),
            Some(Decimal::new(20000, 2))
        );
        assert_eq!(
            convert(&table(), Decimal::new(1200, 2), "USD", "EUR", "2026-11-19"),
            Some(Decimal::new(1000, 2))
        );
        assert_eq!(
            convert(&table(), twenty, "USD", "USD", "2020-01-01"),
            Some(twenty)
        );
        assert_eq!(convert(&table(), twenty, "USD", "EUR", "2026-11-01"), None);
        assert_eq!(convert(&table(), twenty, "GBP", "EUR", "2026-11-19"), None);
    }

    #[test]
    fn reads_the_ecb_csv() {
        let csv = "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE,OBS_STATUS\n\
                   EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2026-11-19,1.1612,A\n\
                   EXR.D.NOK.EUR.SP00.A,D,NOK,EUR,SP00,A,2026-11-19,11.735,A\n";

        let rates = parse_ecb_csv(csv).unwrap();

        assert_eq!(rates.len(), 2);
        assert_eq!(
            rates[0],
            Rate {
                date: "2026-11-19".into(),
                currency: "USD".into(),
                per_eur: 1.1612
            }
        );
        assert!(parse_ecb_csv("something else").is_none());
    }

    #[test]
    fn asks_for_what_isnt_the_euro() {
        let currencies: BTreeSet<String> = ["EUR", "NOK", "USD"].map(String::from).into();

        assert_eq!(
            ecb_url(&currencies, "2026-01-01").as_deref(),
            Some(
                "https://data-api.ecb.europa.eu/service/data/EXR/D.NOK+USD.EUR.SP00.A?format=csvdata&startPeriod=2026-01-01"
            )
        );
        assert_eq!(ecb_url(&["EUR".to_string()].into(), "2026-01-01"), None);
    }
}
