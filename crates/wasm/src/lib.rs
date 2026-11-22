//! The core for a JavaScript host (Spendyard Cloud's Worker), through
//! `wasm-bindgen`. Answers are JSON text, as the CLI prints them, so the host
//! reads exactly what `spendyard read` would.
//!
//! - [`read`]: a document's format, parts and text (`spendyard parts`).
//! - [`understand`]: the whole pipeline (`spendyard read`), with the CLI's
//!   providers and prompts, posting through a function the host gives: the
//!   Worker's `fetch` to AI Gateway and Workers AI. With the result come the
//!   rows a store keeps from it.
//! - [`subscriptions`]: the recurring vendors in a store's rows
//!   (`spendyard subscriptions`), from the rows of the core's `ROWS_SQL`,
//!   with the rates the host keeps.
//! - [`vendors`]: who the organisation pays, from the same rows: what each
//!   cost, month by month, and its subscriptions, cards and accounts.
//! - [`catalog_domain`]: whether the vendor catalog knows a domain.
//! - [`rows_sql`]: the query for those rows, which the host's store runs.
//! - [`rates_needed`], [`rates_url`] and [`parse_rates`]: which exchange
//!   rates converting needs, where to ask the ECB for them, and its answer
//!   read.
//!
//! [`read_json`] builds natively too, so the same answer can be checked both
//! ways (`examples/read.rs`, `tests/compare.mjs`).

mod transport;

use serde::Deserialize;
use serde_json::{Value, json};
use spendyard_core::{Pipeline, decide, extract};
use wasm_bindgen::prelude::wasm_bindgen;

/// Reads a document: `{"document": …}`, or `{"error": "…"}` when it can't be
/// read. The document is what `spendyard parts` shows: its format, parts and
/// text.
pub fn read_json(bytes: &[u8]) -> String {
    let answer = match spendyard_core::read(bytes, None) {
        Ok(document) => json!({ "document": document }),
        Err(error) => json!({ "error": error.to_string() }),
    };

    answer.to_string()
}

/// [`read_json`], for JavaScript.
#[wasm_bindgen]
pub fn read(bytes: &[u8]) -> String {
    read_json(bytes)
}

/// The core's version: `meta.spendyard_version` in its results.
#[wasm_bindgen]
pub fn version() -> String {
    spendyard_core::VERSION.to_string()
}

/// What [`subscriptions`] works with: how (the currency, today, whether
/// lapsed ones too) and the rates it converts with.
#[derive(Debug, Default, Deserialize)]
struct SpendOptions {
    #[serde(flatten)]
    options: spendyard_core::subscriptions::SubscriptionOptions,

    #[serde(default)]
    rates: Vec<spendyard_core::rates::Rate>,

    /// What people decided about vendors: applied to the rows first.
    #[serde(default)]
    vendor_edits: Vec<spendyard_core::vendors::VendorEdit>,
}

/// What [`vendors`] works with: how, the rates it converts with, and what
/// people decided about vendors.
#[derive(Debug, Default, Deserialize)]
struct VendorsOptions {
    #[serde(flatten)]
    options: spendyard_core::vendors::VendorOptions,

    #[serde(default)]
    rates: Vec<spendyard_core::rates::Rate>,

    #[serde(default)]
    vendor_edits: Vec<spendyard_core::vendors::VendorEdit>,
}

/// The rows of `ROWS_SQL` (JSON, its columns as named), grouped by vendor.
fn report_rows(rows: &str) -> Result<Vec<spendyard_core::spend::Row>, String> {
    let stored: Vec<spendyard_core::spend::StoredRow> =
        serde_json::from_str(rows).map_err(|error| error.to_string())?;

    let catalog = spendyard_core::catalog::catalog();

    Ok(stored
        .into_iter()
        .map(|row| spendyard_core::spend::Row::from_stored(row, catalog))
        .collect())
}

/// The recurring vendors in a store's rows, as `spendyard subscriptions
/// --json` has them: `{"currency", "subscriptions", "monthly"}`, or
/// `{"error": "…"}`. `options` is `{"currency"?, "today", "all"?, "rates"?}`.
pub fn subscriptions_json(rows: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let rows = report_rows(rows)?;
        let asked: SpendOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rows = spendyard_core::vendors::apply_vendor_edits(rows, &asked.vendor_edits);
        let rates = spendyard_core::rates::RateTable::new(asked.rates);
        let found = spendyard_core::subscriptions::subscriptions(&rows, &asked.options, &rates);

        serde_json::to_value(found).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// The vendors in a store's rows: `{"currency", "vendors"}`, the most spent
/// on first, or `{"error": "…"}`. `options` is `{"currency"?, "today",
/// "all"?, "rates"?, "vendor_edits"?}`.
pub fn vendors_json(rows: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let rows = report_rows(rows)?;
        let asked: VendorsOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rows = spendyard_core::vendors::apply_vendor_edits(rows, &asked.vendor_edits);
        let rates = spendyard_core::rates::RateTable::new(asked.rates);
        let found = spendyard_core::vendors::vendors(&rows, &asked.options, &rates);

        serde_json::to_value(found).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`vendors_json`], for JavaScript.
#[wasm_bindgen]
pub fn vendors(rows: &str, options: &str) -> String {
    vendors_json(rows, options)
}

/// The catalog's vendor for a domain (or a subdomain of one): its ID, or
/// empty when the catalog doesn't know it.
#[wasm_bindgen]
pub fn catalog_domain(domain: &str) -> String {
    spendyard_core::catalog::catalog()
        .by_domain(domain)
        .map(|vendor| vendor.id.clone())
        .unwrap_or_default()
}

/// [`subscriptions_json`], for JavaScript.
#[wasm_bindgen]
pub fn subscriptions(rows: &str, options: &str) -> String {
    subscriptions_json(rows, options)
}

/// The query for a store's rows (the core's `ROWS_SQL`): what
/// [`subscriptions`] takes, its columns as named.
#[wasm_bindgen]
pub fn rows_sql() -> String {
    spendyard_core::spend::ROWS_SQL.to_string()
}

/// What converting a store's rows into `currency` (empty: the most common
/// one paid in) needs: `{"currency", "currencies", "since"}`, the rates of
/// which currencies from which day on (none when nothing needs
/// converting); or `{"error": "…"}`.
pub fn rates_needed_json(rows: &str, currency: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let rows = report_rows(rows)?;

        let paid = spendyard_core::spend::spend(&rows);
        let currency = spendyard_core::spend::report_currency(Some(currency.to_string()), &paid);

        let (currencies, since) =
            spendyard_core::spend::rates_needed(&paid, &currency).unwrap_or_default();

        Ok(json!({ "currency": currency, "currencies": currencies, "since": since }))
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`rates_needed_json`], for JavaScript.
#[wasm_bindgen]
pub fn rates_needed(rows: &str, currency: &str) -> String {
    rates_needed_json(rows, currency)
}

/// Where to ask the ECB for the daily rates of `currencies` (JSON, a list)
/// from `start` on; empty when only the euro is asked for.
#[wasm_bindgen]
pub fn rates_url(currencies: &str, start: &str) -> String {
    let currencies: std::collections::BTreeSet<String> =
        serde_json::from_str(currencies).unwrap_or_default();

    spendyard_core::rates::ecb_url(&currencies, start).unwrap_or_default()
}

/// The rates in the ECB's CSV answer: `[{"date", "currency", "per_eur"}]`,
/// or `{"error": "…"}` when it isn't the CSV expected.
#[wasm_bindgen]
pub fn parse_rates(csv: &str) -> String {
    match spendyard_core::rates::parse_ecb_csv(csv) {
        Some(rates) => json!(rates).to_string(),
        None => json!({ "error": "the ECB's answer wasn't the CSV expected" }).to_string(),
    }
}

/// What [`understand`] reads with, as JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    /// The capable model: `{"provider": "anthropic", "model": …, "apiKey": …, "baseUrl": …}`.
    extractor: ModelOptions,

    /// The cheap first model, if any: `{"provider": "clef-flash", "accountId": …, "apiKey": …}`.
    decider: Option<ModelOptions>,

    /// How sure the first model must be that a document isn't about money
    /// before the extractor is skipped; the CLI's default otherwise.
    threshold: Option<f64>,

    /// A file name, which helps tell a format apart.
    name: Option<String>,

    /// When it arrived, RFC 3339: a receipt that doesn't say when it was
    /// issued is dated by it.
    received: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOptions {
    provider: String,
    model: Option<String>,
    api_key: Option<String>,
    account_id: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
}

/// Reads a document with the whole pipeline, as `spendyard read` does, posting
/// to the models through `post`. Its answer is JSON: `{"document": …,
/// "result": …, "rows": …}`, or `{"error": "…", "stage": …}` when it couldn't
/// read it (`stage` is `options`, `read` or `understand`).
///
/// `post(url, headers, body)` takes the URL, the headers as JSON (`[[name,
/// value], …]`) and the body as JSON text, and returns a promise of
/// `{"status": 200, "body": "…"}`: the response's status and text. It
/// rejects only when no response came.
#[wasm_bindgen]
pub async fn understand(bytes: Vec<u8>, options: String, post: js_sys::Function) -> String {
    understand_with(bytes, &options, transport::host(post))
        .await
        .to_string()
}

async fn understand_with(
    bytes: Vec<u8>,
    options: &str,
    transport: std::sync::Arc<dyn spendyard_models::extract::Transport>,
) -> Value {
    let options: Options = match serde_json::from_str(options) {
        Ok(options) => options,
        Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
    };

    let document = match spendyard_core::read(&bytes, options.name.as_deref()) {
        Ok(document) => document,
        Err(error) => return json!({ "error": error.to_string(), "stage": "read" }),
    };

    let extractor = match spendyard_models::extract::build_with(
        extractor_settings(options.extractor),
        transport.clone(),
    ) {
        Ok(extractor) => extractor,
        Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
    };

    let decider = match options.decider.map(decider_settings) {
        Some(settings) => match spendyard_models::decide::build_with(settings, transport) {
            Ok(decider) => Some(decider),
            Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
        },
        None => None,
    };

    let pipeline = Pipeline {
        decider: decider.as_deref(),
        extractor: extractor.as_ref(),
        threshold: options.threshold.unwrap_or(decide::DEFAULT_THRESHOLD),
    };

    let result = match spendyard_core::understand(&document, &pipeline).await {
        Ok(result) => result,
        Err(error) => return json!({ "error": error.to_string(), "stage": "understand" }),
    };

    let headers = document.headers.as_ref();

    let received = options
        .received
        .clone()
        .or_else(|| headers.and_then(|headers| headers.date.clone()));

    let rows = spendyard_core::rows::rows(&result, received.as_deref());

    json!({
        "document": {
            "format": document.format,
            "from": headers.and_then(|headers| headers.from.clone()),
            "subject": headers.and_then(|headers| headers.subject.clone()),
            "date": headers.and_then(|headers| headers.date.clone()),
            "messageId": headers.and_then(|headers| headers.message_id.clone()),
        },
        "result": result,
        "rows": rows,
    })
}

fn extractor_settings(options: ModelOptions) -> extract::Settings {
    extract::Settings {
        provider: options.provider,
        model: options.model,
        api_key: options.api_key,
        base_url: options.base_url,
        max_tokens: options.max_tokens,
        ..extract::Settings::default()
    }
}

fn decider_settings(options: ModelOptions) -> decide::Settings {
    decide::Settings {
        provider: options.provider,
        model: options.model,
        api_key: options.api_key,
        account_id: options.account_id,
        base_url: options.base_url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works_out_subscriptions_from_rows_as_json() {
        let rows = json!([
            {
                "id": 2, "date": "2026-11-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
                "vendor_id": "vercel.com", "vendor_name": "Vercel", "total": "20.00", "currency": "USD",
                "billing_model": "subscription", "interval": "month", "flags": 0, "personal": 0, "ignored": 0
            },
            {
                "id": 1, "date": "2026-10-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
                "vendor_id": "vercel.com", "vendor_name": "Vercel", "total": "20.00", "currency": "USD",
                "billing_model": "subscription", "interval": "month", "flags": 0, "personal": 0, "ignored": 0
            }
        ]);

        let options = json!({
            "currency": "EUR",
            "today": "2026-11-21",
            "rates": [{ "date": "2026-11-19", "currency": "USD", "per_eur": 1.25 }]
        });

        let answer: Value =
            serde_json::from_str(&subscriptions_json(&rows.to_string(), &options.to_string()))
                .unwrap();

        assert_eq!(answer["currency"], "EUR");
        assert_eq!(answer["monthly"], "16.00");
        assert_eq!(answer["subscriptions"][0]["vendor"], "Vercel");
        assert_eq!(answer["subscriptions"][0]["status"], "active");
        assert_eq!(answer["subscriptions"][0]["documents"], 2);
    }

    #[test]
    fn says_which_rates_converting_needs() {
        let rows = json!([{
            "id": 1, "date": "2026-11-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
            "vendor_name": "Vercel", "total": "20.00", "currency": "USD", "flags": 0, "personal": 0, "ignored": 0
        }]);

        let needed: Value =
            serde_json::from_str(&rates_needed_json(&rows.to_string(), "NOK")).unwrap();

        assert_eq!(
            needed,
            json!({ "currency": "NOK", "currencies": ["NOK", "USD"], "since": "2026-11-12" })
        );

        let nothing: Value =
            serde_json::from_str(&rates_needed_json(&rows.to_string(), "")).unwrap();

        assert_eq!(
            nothing,
            json!({ "currency": "USD", "currencies": [], "since": "" })
        );
    }

    #[test]
    fn works_out_vendors_with_their_edits() {
        let rows = json!([{
            "id": 1, "date": "2026-11-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
            "vendor_id": "vercel.com", "vendor_name": "Vercel", "total": "20.00", "currency": "USD",
            "billing_model": "subscription", "interval": "month", "flags": 0, "personal": 0, "ignored": 0
        }]);

        let options = json!({
            "today": "2026-11-21",
            "vendor_edits": [{ "vendor": "vercel.com", "name": "Vercel (hosting)" }]
        });

        let answer: Value =
            serde_json::from_str(&vendors_json(&rows.to_string(), &options.to_string())).unwrap();

        assert_eq!(answer["vendors"][0]["id"], "vercel.com");
        assert_eq!(answer["vendors"][0]["name"], "Vercel (hosting)");
        assert_eq!(answer["vendors"][0]["spent_year"], "20.00");
        assert_eq!(catalog_domain("billing.vercel.com"), "vercel.com");
        assert_eq!(catalog_domain("nowhere.example"), "");
    }

    #[test]
    fn says_why_when_the_rows_arent_rows() {
        let answer: Value =
            serde_json::from_str(&subscriptions_json("[{}]", "{\"today\": \"2026-11-21\"}"))
                .unwrap();

        assert!(answer["error"].is_string());
    }
}
