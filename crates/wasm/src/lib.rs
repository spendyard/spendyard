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
//! - [`transactions`], [`transaction_suggestions`], [`transaction_sql`]
//!   and [`transaction_rates_needed`]: payments from card statements, each
//!   matched to its receipt, as `spendyard transactions match` does.
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

/// One part of a document as it came in, by its index (as `read` and
/// evidence number them): an email's attached PDF, say. Empty when there's no
/// such part or the document can't be read.
#[wasm_bindgen]
pub fn part_bytes(bytes: &[u8], index: usize) -> Vec<u8> {
    spendyard_core::read(bytes, None)
        .ok()
        .and_then(|document| document.parts.into_iter().find(|part| part.index == index))
        .map(|part| part.bytes)
        .unwrap_or_default()
}

/// A stored result as the contract's `Reading`, in protobuf's JSON form
/// (`spendyard_core::reading`): what `GetDocument` gives. `result` is the
/// result's JSON as `results.json` keeps it; `read_at` when it was read,
/// RFC 3339, or empty. `{"error": "…"}` when `result` isn't JSON.
#[wasm_bindgen]
pub fn reading_of(result: &str, read_at: &str) -> String {
    let answer = match serde_json::from_str::<serde_json::Value>(result) {
        Ok(stored) => {
            let read_at = (!read_at.is_empty()).then_some(read_at);

            spendyard_core::reading::reading_of(&stored, read_at)
        }

        Err(error) => json!({ "error": error.to_string() }),
    };

    answer.to_string()
}

/// A `Reading` pushed by the CLI, in protobuf's JSON form, as a result and
/// the rows made from it, as [`understand`] gives them for a document read
/// here: `{"result": …, "rows": …}`. `received` is when the document
/// arrived, RFC 3339, for a reading that doesn't say when it was issued;
/// empty when it isn't known. `{"error": "…"}` when `reading` isn't JSON.
#[wasm_bindgen]
pub fn result_of_reading(reading: &str, received: &str) -> String {
    let answer = match serde_json::from_str::<serde_json::Value>(reading) {
        Ok(pushed) => {
            let result = spendyard_core::reading::result_of(&pushed);
            let received = (!received.is_empty()).then_some(received);

            let rows = spendyard_core::rows::rows(&result, received);

            json!({ "result": result, "rows": rows })
        }

        Err(error) => json!({ "error": error.to_string() }),
    };

    answer.to_string()
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

    /// The statements' payments and the receipts they're matched to (as
    /// [`transactions`] takes them), and links by hand: payments with no
    /// receipt stand in for theirs.
    #[serde(default)]
    charges: Vec<spendyard_core::transactions::Charge>,
    #[serde(default)]
    match_receipts: Vec<spendyard_core::transactions::MatchReceipt>,
    #[serde(default)]
    links: Vec<spendyard_core::transactions::ChargeLink>,
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

    /// The statements' payments, the receipts they're matched to, and links
    /// by hand: payments with no receipt count as spend too.
    #[serde(default)]
    charges: Vec<spendyard_core::transactions::Charge>,
    #[serde(default)]
    match_receipts: Vec<spendyard_core::transactions::MatchReceipt>,
    #[serde(default)]
    links: Vec<spendyard_core::transactions::ChargeLink>,
}

/// What [`payments`] works with: the rates matching converts with, what
/// people decided about vendors, the statements' payments with the
/// receipts they're matched to and links by hand, and the payments the
/// store keeps: their IDs, and those retired (moved into another, or gone),
/// whose numbers aren't given again.
#[derive(Debug, Default, Deserialize)]
struct PaymentsOptions {
    #[serde(default)]
    rates: Vec<spendyard_core::rates::Rate>,

    #[serde(default)]
    vendor_edits: Vec<spendyard_core::vendors::VendorEdit>,

    #[serde(default)]
    charges: Vec<spendyard_core::transactions::Charge>,
    #[serde(default)]
    match_receipts: Vec<spendyard_core::transactions::MatchReceipt>,
    #[serde(default)]
    links: Vec<spendyard_core::transactions::ChargeLink>,

    #[serde(default)]
    stored: Vec<spendyard_core::payments::Payment>,
    #[serde(default)]
    retired: Vec<i64>,
}

/// Every payment in a store's rows and statements, newest first, as
/// `spendyard_core::payments::records` has them: `{"records", "payments",
/// "moved", "gone"}`, `payments` being what the store keeps from now on;
/// or `{"error": "…"}`. `options` is `{"rates"?, "vendor_edits"?,
/// "charges"?, "match_receipts"?, "links"?, "stored"?, "retired"?}`.
fn payments_json_here(rows: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let mut rows = report_rows(rows)?;
        let asked: PaymentsOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rates = spendyard_core::rates::RateTable::new(asked.rates);

        let matched = spendyard_core::transactions::match_transactions(
            asked.charges,
            asked.match_receipts,
            &asked.links,
            &rates,
        );

        rows.extend(spendyard_core::transactions::payment_rows(&matched));
        rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));

        let rows = spendyard_core::vendors::apply_vendor_edits(rows, &asked.vendor_edits);

        let found =
            spendyard_core::payments::records(&rows, &matched, &asked.stored, &asked.retired);

        serde_json::to_value(found).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`payments_json`], for JavaScript.
#[wasm_bindgen]
pub fn payments(rows: &str, options: &str) -> String {
    payments_json(rows, options)
}

/// The rows with the statements' payments that have no receipt standing in
/// for theirs (matched as [`transactions`] matches them, links by hand
/// first), newest first.
fn with_payments(
    mut rows: Vec<spendyard_core::spend::Row>,
    charges: Vec<spendyard_core::transactions::Charge>,
    match_receipts: Vec<spendyard_core::transactions::MatchReceipt>,
    links: &[spendyard_core::transactions::ChargeLink],
    rates: &spendyard_core::rates::RateTable,
) -> Vec<spendyard_core::spend::Row> {
    if charges.is_empty() {
        return rows;
    }

    let matched =
        spendyard_core::transactions::match_transactions(charges, match_receipts, links, rates);

    rows.extend(spendyard_core::transactions::payment_rows(&matched));
    rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));

    rows
}

/// The rows of `ROWS_SQL` (JSON, its columns as named), grouped by vendor.
fn report_rows(rows: &str) -> Result<Vec<spendyard_core::spend::Row>, String> {
    let stored: Vec<spendyard_core::spend::StoredRow> =
        serde_json::from_str(rows).map_err(|error| error.to_string())?;

    let catalog = spendyard_core::catalog::catalog();

    Ok(stored
        .into_iter()
        .map(|row| spendyard_core::spend::Row::from_stored(row, &catalog))
        .collect())
}

/// The recurring vendors in a store's rows, as `spendyard subscriptions
/// --json` has them: `{"currency", "subscriptions", "monthly"}`, or
/// `{"error": "…"}`. `options` is `{"currency"?, "today", "all"?, "rates"?,
/// "vendor_edits"?, "charges"?, "match_receipts"?, "links"?}`: with the
/// statements' payments, those with no receipt count too.
fn subscriptions_json_here(rows: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let rows = report_rows(rows)?;
        let asked: SpendOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rates = spendyard_core::rates::RateTable::new(asked.rates);

        let rows = with_payments(
            rows,
            asked.charges,
            asked.match_receipts,
            &asked.links,
            &rates,
        );

        let rows = spendyard_core::vendors::apply_vendor_edits(rows, &asked.vendor_edits);
        let found = spendyard_core::subscriptions::subscriptions(&rows, &asked.options, &rates);

        serde_json::to_value(found).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// The vendors in a store's rows: `{"currency", "vendors"}`, the most spent
/// on first, or `{"error": "…"}`. `options` is `{"currency"?, "today",
/// "all"?, "rates"?, "vendor_edits"?, "charges"?, "match_receipts"?,
/// "links"?}`: with the statements' payments, those with no receipt count
/// as spend too.
fn vendors_json_here(rows: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let rows = report_rows(rows)?;
        let asked: VendorsOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rates = spendyard_core::rates::RateTable::new(asked.rates);

        let rows = with_payments(
            rows,
            asked.charges,
            asked.match_receipts,
            &asked.links,
            &rates,
        );

        let rows = spendyard_core::vendors::apply_vendor_edits(rows, &asked.vendor_edits);
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

/// The queries for the payments matched (the core's `CHARGES_SQL`) and the
/// receipts they're matched to (`MATCH_RECEIPTS_SQL`), which the host's
/// store runs: `{"charges", "receipts"}`.
#[wasm_bindgen]
pub fn transaction_sql() -> String {
    json!({
        "charges": spendyard_core::transactions::CHARGES_SQL,
        "receipts": spendyard_core::transactions::MATCH_RECEIPTS_SQL,
    })
    .to_string()
}

/// What [`transactions`] and [`transaction_suggestions`] work with.
#[derive(Debug, Default, Deserialize)]
struct TransactionOptions {
    #[serde(default)]
    links: Vec<spendyard_core::transactions::ChargeLink>,

    #[serde(default)]
    rates: Vec<spendyard_core::rates::Rate>,

    /// For suggestions: the payment, by its ID, and how many at most.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// The rows of the two queries, as the core takes them.
fn transaction_rows(
    charges: &str,
    receipts: &str,
) -> Result<
    (
        Vec<spendyard_core::transactions::Charge>,
        Vec<spendyard_core::transactions::MatchReceipt>,
    ),
    String,
> {
    let charges = serde_json::from_str(charges).map_err(|error| format!("charges: {error}"))?;
    let receipts = serde_json::from_str(receipts).map_err(|error| format!("receipts: {error}"))?;

    Ok((charges, receipts))
}

/// Every payment matched to its receipt, as the CLI's `spendyard transactions
/// match` does: `{"transactions", "matched", "missing", …}`, or
/// `{"error": "…"}`. `options` is `{"links"?, "rates"?}`.
fn transactions_json_here(charges: &str, receipts: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let (charges, receipts) = transaction_rows(charges, receipts)?;
        let asked: TransactionOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let rates = spendyard_core::rates::RateTable::new(asked.rates);
        let matched = spendyard_core::transactions::match_transactions(
            charges,
            receipts,
            &asked.links,
            &rates,
        );

        serde_json::to_value(matched).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`transactions_json`], for JavaScript.
#[wasm_bindgen]
pub fn transactions(charges: &str, receipts: &str, options: &str) -> String {
    transactions_json(charges, receipts, options)
}

/// Receipts that could be one payment's, the likeliest first: their
/// document IDs, or `{"error": "…"}`. `options` is `{"id", "limit"?,
/// "rates"?}`.
fn transaction_suggestions_json_here(charges: &str, receipts: &str, options: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let (charges, receipts) = transaction_rows(charges, receipts)?;
        let asked: TransactionOptions =
            serde_json::from_str(options).map_err(|error| error.to_string())?;

        let id = asked.id.ok_or("which payment: options.id")?;
        let charge = charges
            .iter()
            .find(|charge| charge.key() == id)
            .ok_or("no such payment")?;

        let rates = spendyard_core::rates::RateTable::new(asked.rates);
        let found = spendyard_core::transactions::suggestions(
            charge,
            &receipts,
            &rates,
            asked.limit.unwrap_or(5),
        );

        Ok(json!(found))
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`transaction_suggestions_json`], for JavaScript.
#[wasm_bindgen]
pub fn transaction_suggestions(charges: &str, receipts: &str, options: &str) -> String {
    transaction_suggestions_json(charges, receipts, options)
}

/// What matching needs to convert: `{"currencies", "since"}`, none when
/// one currency is all; or `{"error": "…"}`.
#[wasm_bindgen]
pub fn transaction_rates_needed(charges: &str, receipts: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let (charges, receipts) = transaction_rows(charges, receipts)?;

        let (currencies, since) =
            spendyard_core::transactions::rates_needed(&charges, &receipts).unwrap_or_default();

        Ok(json!({ "currencies": currencies, "since": since }))
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// One thing that names a vendor: its ID as read, and its name.
#[derive(Debug, Default, Deserialize)]
struct Naming {
    #[serde(default)]
    vendor_id: Option<String>,
    #[serde(default)]
    vendor_name: Option<String>,
    /// The name it was corrected to, on that one thing.
    #[serde(default)]
    edited_vendor_name: Option<String>,
}

/// The vendors things name (`[{"vendor_id"?, "vendor_name"?,
/// "edited_vendor_name"?}]`), as the
/// organisation's vendors have them, with what people decided (`edits`,
/// as [`vendors`] takes them): `[{"id", "name", "domain"?}]`, in the same
/// order; or `{"error": "…"}`.
pub fn vendor_refs_json(items: &str, edits: &str) -> String {
    // The edits alone, or `{"vendor_edits", "vendors"}` with the organisation's own vendors.
    let (edits, own) = match serde_json::from_str::<RefsOptions>(edits) {
        Ok(options) => (
            serde_json::to_string(&options.vendor_edits).unwrap_or_default(),
            options.vendors,
        ),
        Err(_) => (edits.to_string(), Vec::new()),
    };

    spendyard_core::catalog::with_vendors(own_vendors(own), || vendor_refs_here(items, &edits))
}

/// What [`vendor_refs_json`] can take besides the things: what people
/// decided about vendors, and the organisation's own vendors.
#[derive(Debug, Default, Deserialize)]
struct RefsOptions {
    #[serde(default)]
    vendor_edits: Vec<serde_json::Value>,
    #[serde(default)]
    vendors: Vec<OwnVendor>,
}

fn vendor_refs_here(items: &str, edits: &str) -> String {
    let answer = (|| -> Result<Value, String> {
        let items: Vec<Naming> = serde_json::from_str(items).map_err(|error| error.to_string())?;
        let edits: Vec<spendyard_core::vendors::VendorEdit> =
            serde_json::from_str(edits).map_err(|error| error.to_string())?;

        let refs: Vec<_> = items
            .iter()
            .map(|item| {
                let name = item.vendor_name.as_deref().unwrap_or("");

                spendyard_core::vendors::vendor_ref(
                    item.vendor_id.as_deref(),
                    name,
                    item.edited_vendor_name.as_deref(),
                    &edits,
                )
            })
            .collect();

        serde_json::to_value(refs).map_err(|error| error.to_string())
    })();

    answer
        .unwrap_or_else(|error| json!({ "error": error }))
        .to_string()
}

/// [`vendor_refs_json`], for JavaScript.
#[wasm_bindgen]
pub fn vendor_refs(items: &str, edits: &str) -> String {
    vendor_refs_json(items, edits)
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
    /// An AI Gateway by name, for the first model on Workers AI.
    gateway: Option<String>,
    /// An authenticated AI Gateway's token, for the extractor at its URL.
    gateway_token: Option<String>,
}

/// Reads a document with the whole pipeline, as `spendyard read` does, posting
/// to the models through `post`. Its answer is JSON: `{"document": …,
/// "result": …, "rows": …, "cost": …}` (`cost` in dollars, null when a
/// model's price isn't known), or `{"error": "…", "stage": …}` when it couldn't
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

    // What reading it cost, in dollars, when its models' prices are known.
    let cost = spendyard_core::eval::cost(&result);

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
        "cost": cost,
    })
}

fn extractor_settings(options: ModelOptions) -> extract::Settings {
    extract::Settings {
        provider: options.provider,
        model: options.model,
        api_key: options.api_key,
        base_url: options.base_url,
        gateway_token: options.gateway_token,
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
        gateway: options.gateway,
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
    fn works_out_payments_keeping_the_stored_ids() {
        let rows = json!([
            {
                "id": 2, "date": "2026-11-03", "kind": "receipt", "status": "paid", "is_receipt": 1,
                "vendor_id": "vercel.com", "vendor_name": "Vercel", "total": "20.00", "currency": "USD",
                "flags": 0, "personal": 0, "ignored": 0
            },
            {
                "id": 1, "date": "2026-11-01", "kind": "invoice", "status": "paid", "is_receipt": 1,
                "vendor_id": "vercel.com", "vendor_name": "Vercel", "total": "20.00", "currency": "USD",
                "interval": "month", "flags": 0, "personal": 0, "ignored": 0
            }
        ]);

        let options = json!({
            "charges": [{
                "id": 1, "document_id": 90, "position": 4, "date": "2026-11-03",
                "merchant": "VERCEL INC.", "amount": "20.00", "currency": "USD", "kind": "purchase"
            }],
            "match_receipts": [{
                "document_id": 2, "vendor_name": "Vercel", "date": "2026-11-03",
                "total": "20.00", "currency": "USD", "kind": "receipt"
            }],
            "stored": [{ "id": 4, "evidence": { "documents": [], "charges": ["90:4"] } }],
            "retired": [7]
        });

        let answer: Value =
            serde_json::from_str(&payments_json(&rows.to_string(), &options.to_string())).unwrap();

        let record = &answer["records"][0];

        assert_eq!(answer["records"].as_array().unwrap().len(), 1);
        assert_eq!(record["id"], 4);
        assert_eq!(record["kind"], "payment");
        assert_eq!(record["row"]["id"], 2);
        assert_eq!(record["row"]["interval"], "month");
        assert_eq!(
            record["evidence"],
            json!({ "documents": [1, 2], "charges": ["90:4"] })
        );
        assert_eq!(answer["payments"][0]["id"], 4);
    }

    #[test]
    fn takes_the_organisations_own_vendors() {
        // The same firm, one receipt by its domain and one by its legal name.
        let rows = json!([
            {
                "id": 2, "date": "2026-11-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
                "vendor_id": "fjordbyra.no", "vendor_name": "Fjordbyrå", "total": "300.00", "currency": "NOK",
                "flags": 0, "personal": 0, "ignored": 0
            },
            {
                "id": 1, "date": "2026-10-19", "kind": "receipt", "status": "paid", "is_receipt": 1,
                "vendor_name": "Fjordbyrå Advokatfirma AS", "total": "300.00", "currency": "NOK",
                "flags": 0, "personal": 0, "ignored": 0
            }
        ]);

        let vendors = |options: Value| -> Value {
            serde_json::from_str(&vendors_json(&rows.to_string(), &options.to_string())).unwrap()
        };

        let alone = vendors(json!({ "today": "2026-11-21" }));

        let own = vendors(json!({
            "today": "2026-11-21",
            "vendors": [{
                "id": "fjordbyra.no",
                "name": "Fjordbyrå",
                "aliases": ["Fjordbyrå Advokatfirma AS"],
                "category": "legal"
            }]
        }));

        assert_eq!(alone["vendors"].as_array().unwrap().len(), 2);

        assert_eq!(own["vendors"].as_array().unwrap().len(), 1);
        assert_eq!(own["vendors"][0]["id"], "fjordbyra.no");
        assert_eq!(own["vendors"][0]["category"], "legal");
        assert_eq!(own["vendors"][0]["known"], true);
        assert_eq!(own["vendors"][0]["receipts"], 2);
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
    fn matches_payments_from_rows_as_json() {
        let charges = json!([{
            "id": 1, "document_id": 100, "position": 1, "date": "2026-11-19", "merchant": "VERCEL INC.",
            "card_last4": "3776", "amount": "20.00", "currency": "USD", "kind": "purchase"
        }]);

        let receipts = json!([{
            "document_id": 7, "vendor_name": "Vercel", "date": "2026-11-18", "total": "20.00", "currency": "USD", "kind": "receipt"
        }]);

        let matched: Value = serde_json::from_str(&transactions_json(
            &charges.to_string(),
            &receipts.to_string(),
            "{}",
        ))
        .unwrap();

        assert_eq!(matched["transactions"][0]["id"], "100:1");
        assert_eq!(matched["transactions"][0]["receipt_id"], 7);
        assert_eq!(matched["matched"], 1);

        let unlinked = json!({ "links": [{ "id": "100:1", "no_receipt": true }] });
        let unlinked: Value = serde_json::from_str(&transactions_json(
            &charges.to_string(),
            &receipts.to_string(),
            &unlinked.to_string(),
        ))
        .unwrap();

        assert_eq!(unlinked["transactions"][0]["state"], "no receipt needed");

        let suggested: Value = serde_json::from_str(&transaction_suggestions_json(
            &charges.to_string(),
            &receipts.to_string(),
            "{\"id\": \"100:1\"}",
        ))
        .unwrap();

        assert_eq!(suggested, json!([7]));
    }

    #[test]
    fn says_why_when_the_rows_arent_rows() {
        let answer: Value =
            serde_json::from_str(&subscriptions_json("[{}]", "{\"today\": \"2026-11-21\"}"))
                .unwrap();

        assert!(answer["error"].is_string());
    }
}

/// [`subscriptions_json_here`] with the organisation's own vendors (`options.vendors`) on top of the catalog.
pub fn subscriptions_json(rows: &str, options: &str) -> String {
    with_own_vendors(options, || subscriptions_json_here(rows, options))
}

/// [`payments_json_here`] with the organisation's own vendors (`options.vendors`) on top of the catalog.
pub fn payments_json(rows: &str, options: &str) -> String {
    with_own_vendors(options, || payments_json_here(rows, options))
}

/// [`vendors_json_here`] with the organisation's own vendors (`options.vendors`) on top of the catalog.
pub fn vendors_json(rows: &str, options: &str) -> String {
    with_own_vendors(options, || vendors_json_here(rows, options))
}

/// [`transactions_json_here`] with the organisation's own vendors (`options.vendors`) on top of the catalog.
pub fn transactions_json(charges: &str, receipts: &str, options: &str) -> String {
    with_own_vendors(options, || {
        transactions_json_here(charges, receipts, options)
    })
}

/// [`transaction_suggestions_json_here`] with the organisation's own vendors (`options.vendors`) on top of the catalog.
pub fn transaction_suggestions_json(charges: &str, receipts: &str, options: &str) -> String {
    with_own_vendors(options, || {
        transaction_suggestions_json_here(charges, receipts, options)
    })
}

/// One of the organisation's own vendors, as a host gives it: the catalog's
/// fields, by `id` (its main domain).
#[derive(Debug, Clone, Deserialize)]
struct OwnVendor {
    id: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    descriptors: Vec<String>,
    #[serde(default = "other")]
    category: String,
    #[serde(default)]
    pricing: Vec<String>,
}

fn other() -> String {
    "other".to_string()
}

/// `options.vendors`, the organisation's own vendors.
#[derive(Debug, Default, Deserialize)]
struct OwnVendors {
    #[serde(default)]
    vendors: Vec<OwnVendor>,
}

/// The organisation's own vendors as the catalog has them; any that
/// [`spendyard_core::catalog::vendor_problem`] finds wrong are left out.
fn own_vendors(own: Vec<OwnVendor>) -> Vec<spendyard_core::catalog::Vendor> {
    own.into_iter()
        .map(|vendor| spendyard_core::catalog::Vendor {
            id: vendor.id.trim().to_lowercase(),
            name: vendor.name.trim().to_string(),
            aliases: vendor.aliases,
            domains: vendor.domains,
            descriptors: vendor.descriptors,
            related: Vec::new(),
            category: vendor.category,
            pricing: vendor.pricing,
            local: true,
        })
        .filter(|vendor| spendyard_core::catalog::vendor_problem(vendor).is_none())
        .collect()
}

/// Runs `work` with the organisation's own vendors in `options` on top of the catalog.
fn with_own_vendors(options: &str, work: impl FnOnce() -> String) -> String {
    let own = serde_json::from_str::<OwnVendors>(options)
        .map(|options| options.vendors)
        .unwrap_or_default();

    spendyard_core::catalog::with_vendors(own_vendors(own), work)
}
