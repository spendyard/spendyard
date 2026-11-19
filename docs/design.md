# Spendyard design

Status: draft, 2026-11-19.

Spendyard has two parts:

1. **`spendyard`, an open-source binary** (Apache 2.0). It reads a
   document — an email, PDF, image or text — and works out whether it is a
   receipt or invoice, who the vendor is, how it was paid, what it cost and
   how it is billed. It scans Gmail and Outlook, keeps what it finds in a
   local store, and reports one person's spending. It runs as a CLI, as an
   HTTP API and as MCP tools for agents, using the user's own model API keys.
2. **Spendyard Cloud, a SaaS spend dashboard** for startups. The same store,
   shared by a company: everyone's receipts, over time and across people,
   with its subscriptions, renewals, seats, AI tool spend and who pays for
   what.

The principle behind both: **deterministic where possible, a model where
necessary, and the cheapest model that can answer.**

## 1. Who it is for

**First customer: startups of 10–50 people** with no IT department, no
single sign-on and no finance integrations. Their SaaS spend is spread
across founders' cards, personal cards that get expensed, and a growing pile
of AI tools billed partly by seat and partly by usage.

Existing SaaS management tools (Zylo, Productiv, Torii, Cledara, Spendflo)
find subscriptions mainly through single sign-on and finance-system
integrations. That suits larger companies. Spendyard starts from
**receipts**: point it at a shared inbox or forward receipts to it, and it
works on day one with nothing else connected.

The angle we lead with is **AI tool spend** (OpenAI, Anthropic, Cursor,
Copilot, Midjourney, …): prices mix seats and usage and change from month
to month, and only the receipt says what was actually used.

The open-source binary is how people find us. A developer runs
`spendyard read` on their own receipts, sees that it works, and brings it to
their company.

## 2. Goals and non-goals

### Goals

- Read every common input: `.eml`, `.msg`, PDF (with or without a text
  layer), JPEG / PNG / WebP / HEIC, HTML and plain text.
- Answer "is this a receipt or invoice?" cheaply enough to run on every
  message in an inbox.
- Extract the vendor, payment methods, amounts, billing model and line
  items (seats, usage, add-ons), with evidence for each value.
- Pluggable models; the user brings their own keys. Nothing phones home.
- One self-contained binary per platform.
- A built-in way to compare models on labelled data (`spendyard eval`).
- A local store that is simply there: results are kept in `~/.spendyard`
  without any setup, and `spendyard report` turns them into spending per
  vendor and month and a list of subscriptions (§8).

### Non-goals (binary v1)

- Customer-defined extraction schemas. Spendyard is about spend, not
  generic document extraction.
- Sharing, teams and alerts: a company-wide store, edits by several
  people, cards matched to people, renewal alerts. That is the cloud's job
  (§11).
- Its own OCR engine. Text layers are read directly; images and scanned
  pages go to models that can see.
- Receipts other than software and services (groceries, restaurants,
  travel). They may work, but we don't tune or evaluate for them.

## 3. Language: Rust

The binary is written in Rust.

- **One static binary** per platform (musl on Linux), with no runtime to
  install.
- **Better document libraries.** `mail-parser` (from Stalwart) handles
  real-world MIME well, and there are pure-Rust crates for PDFs (§3.1).
- **WebAssembly.** The core (reading, validation, model calls over HTTP)
  can compile to `wasm32` and run in a Cloudflare Worker. The cloud can then
  run the exact same code that people run locally. Until the cloud work
  starts, Spendyard is one crate (a library plus the CLI). The core moves
  into its own WebAssembly-ready crate then; meanwhile filesystem, keychain
  and terminal code stays out of the library.
- **Crates for the rest:** `clap` (CLI), `axum` (HTTP), `rmcp` (MCP),
  `reqwest`, `serde`, `schemars` (JSON Schema for structured output).

The cost is slower development than Go, which Stagd uses. We accept that for
the single binary and for running the same core in the cloud.

### 3.1 Reading decisions (M1)

| Input | Crate | Notes |
|---|---|---|
| Email | `mail-parser` | Bodies, attachments and forwarded emails, 41 character sets. |
| Outlook `.msg` | `cfb`, plus our own property reading (~300 lines) | The `.msg` crates are thin and unmaintained. We read only what receipts need. RTF-only bodies are not read yet. |
| HTML | `html2text` | Wide lines, no table borders, so a table row stays on one line. |
| PDF text | `lopdf` (no default features) | See below. |

**PDF text.** Compared on a Chrome-printed invoice (embedded subset fonts,
like most SaaS invoices):

- `pdf-extract` split words at kerning: "$1 12.40", "V isa". It changes
  amounts, so it is out.
- `pdf_oxide` gave the nicest text (table rows on one line) but pulls in
  169 crates, including `libc` and a logger, and is at 0.3.
- `lopdf` got every value right, puts each table cell on its own line, has
  a small dependency tree and is mature. **Chosen.** Cells on separate lines
  are fine for models and for the quote check (§4.5).

Revisit with real receipts in the test set. The choice sits behind
`read::pdf`, so swapping is local.

**No PDF rendering yet.** A PDF without a text layer is returned without
text and with a warning. Models that read PDFs (Anthropic, OpenAI, Gemini)
take it as it is (§4.4). Rendering pages to images, with `hayro` (pure Rust),
is only needed for extractors that take images but not PDFs.

**Decided in M2: no rendering.** Anthropic, OpenAI and Gemini all read PDFs.
`openai-compatible` gets text and images only, and a scanned PDF fails
there with a message naming the providers that can read it. Local models
that read scanned PDFs well are rare anyway. Revisit if that changes.

**HEIC: detected, not decoded.** Decoding needs libheif (C), which breaks
the plain static build. HEIC goes as it is to models that accept it
(Gemini); for the others the error says to convert to JPEG. Revisit if
phone photos turn out to be common.

## 4. Pipeline

```
input ──▶ 1. read ──▶ 2. locate ──▶ 3. decide ──▶ 4. extract ──▶ 5. validate ──▶ result
          (fixed)     (fixed)       (cheap model)  (capable model)  (fixed)
```

### 4.1 Read (deterministic)

Every input becomes one `Document`:

```rust
struct Document {
    headers: Option<EmailHeaders>, // from, to, subject, date, message_id
    parts: Vec<Part>,              // body first, then attachments, forwarded emails flattened
    warnings: Vec<String>,
}

struct Part {
    index: usize,
    label: String,                 // "body", "attachment:Invoice-2291.pdf"
    kind: PartKind,                // Text | Html | Pdf | Image | Email | Other
    mime: String,
    bytes: Vec<u8>,
    text: Option<String>,          // HTML as text, or the PDF text layer
    pages: Option<u32>,
}
```

- The format is detected from the file's first bytes, never its extension
  or `Content-Type`.
- Forwarded emails (`message/rfc822`) are followed up to 3 levels deep.
- An email that can't be parsed falls back to being read as text, with a
  warning. It is not an error.
- Encrypted or corrupt files are errors that name the problem.

### 4.2 Locate (deterministic first)

Decide which part holds the receipt.

- The body has amounts and a vendor → the body.
- The body has no amount and there is one PDF or image attachment
  ("your receipt is attached") → that attachment.
- Otherwise the first model chooses (§4.3), and if it is unsure, the
  extractor gets every candidate part, labelled, and says which one it used.

The vendor hint comes from the From domain and the vendor catalog (§9), not
from a model.

### 4.3 Decide (cheap first model)

One call to a JEV-style decision model answers every multiple-choice and
yes/no question at once, each with a calibrated probability:

| Question | Type | Options |
|---|---|---|
| `document_type` | choice | receipt, invoice, renewal_notice, payment_failed, quote, trial_ending, other |
| `status` | choice | paid, due, failed, upcoming, none |
| `part` | choice | built from the document's parts |
| `billing_model` | choice | subscription, usage, mixed, one_time, unknown |
| `interval` | choice | month, year, other, none |
| `paid_by_card` | yes/no | |
| `has_seats` | yes/no | |
| `has_usage` | yes/no | |
| `about_money` | yes/no | Added in M3; decides routing. The definitions come from receipt-forwarder. |

Routing (as built in M3):

- `about_money` (yes/no: is this a financial document?) is no with at
  least the threshold's probability (default 0.85) → stop. No extraction.
  This is most of an inbox, at roughly $0.0001 per message. A yes/no gives
  a much sharper probability than `other` among seven document types: on
  Clef-flash, `other` scored 0.72–0.88 on obvious non-receipts, while
  `about_money` scored 0.02–0.09 on them and 0.94–0.97 on 29 real receipts.
- Otherwise → extract, with the answers as hints in a `<first-model>` block
  ("has seats and usage: expect both kinds of line"). An unsure first model
  needs no separate LLM fallback: the extractor answers the same questions
  in its own output anyway.
- The first model fails → the extractor reads the document alone, with a
  warning.
- After extraction, a first model that was sure and disagrees with the
  extractor (on receipt-or-invoice versus anything else, or on the billing
  model) raises a `models_disagree` flag.

The answers, with their probabilities, are in the result as `decisions`;
`spendyard decide` runs the first model alone. The state sent is the
headers plus each part's text (8,000 characters per part, 30,000 in all;
Clef has a 24K-token context). Clef also gets up to four images when a
document has no text; JEV is text only.

**One client, four providers.** JEV (TypeSafe; Codiv's OpenJev with
`base_url = "https://api.codiv.ai"`), Clef and Clef-flash on Cloudflare
(REST, the same request format plus images) and `systemone` servers all
take `{ model, state, questions }` and return `{ model, answers, usage }`.

**What else they can answer.** Any multiple-choice, yes/no or 1–10 score
question, up to 64 per call, each with a probability, so new questions are
nearly free. Candidates, each a line in `decide/questions.rs`:

- the spending category (AI, dev tools, design, infrastructure, office …)
  for vendors the catalog doesn't know
- business or personal, for receipts on personal cards
- whether it announces a price increase, a plan change or a seat change
- whether a scanned mailbox message is worth downloading at all, from
  headers and a snippet (§7, Mailboxes)
- in the cloud: which team or cost center a receipt belongs to, and how
  urgent a renewal or failed payment is (a score)

Decision models choose from options; they don't produce text. So they never
supply names, amounts or dates.

### 4.4 Extract (capable model)

One structured-output call to the configured extractor, with the chosen part
(or labelled parts), the hints and the output JSON Schema. Text goes as text.
PDFs go natively to providers that accept them (Anthropic, OpenAI, Gemini);
otherwise pages are rendered to images. Images go to vision models.

A scanned PDF or a photo skips the decision model when that model can't see
it, and the extractor answers the §4.3 questions as well.

### 4.5 Validate (deterministic)

- The lines add up to the subtotal, and subtotal plus tax to the total
  (allowing for rounding and discounts).
- Currencies are ISO 4217 codes; dates parse and are plausible (the period
  end comes after the start, and a renewal comes after the invoice date).
- A card's last four digits are four digits.
- **Every value comes with a quote, and the quote must appear in the
  source text** (normalized for whitespace and number formatting). A value
  whose quote isn't found is flagged and its confidence lowered. This catches
  invented values without a second model call. Values read from images can't
  be checked this way and are marked `evidence_checked: false`.
- **Cross-check the two models.** If the billing model the lines add up to
  disagrees with the decision model's `billing_model`, flag it.

A failed check becomes a flag on the result, not an error.

## 5. Plug-in models

### 5.1 Two traits

```rust
#[async_trait]
trait Decider {
    async fn decide(&self, doc: &Document, questions: &[Question]) -> Result<Vec<Answer>>;
}

#[async_trait]
trait Extractor {
    async fn extract(&self, doc: &Document, req: &ExtractRequest) -> Result<Extraction>;
}
```

`Question` and `Answer` follow the JEV request format (`choice`, `score`,
yes/no), so every decision model uses one adapter shape. Any `Extractor` can
also act as a `Decider` by answering the questions in its structured output.
Its probabilities aren't calibrated, so it is the fallback, not the default.

### 5.2 Built-in providers

| Provider | Decider | Extractor | Notes |
|---|---|---|---|
| `jev` | ✓ | | TypeSafe. Text only, early access. |
| `clef`, `clef-flash` | ✓ | | Cloudflare Workers AI REST API with the user's token. Accepts small images. Open weights. |
| `systemone` | ✓ | | Self-hosted Clef, Nimble and similar through llama.cpp or SGLang. |
| `anthropic` | fallback | ✓ | Reads PDFs natively. |
| `openai` | fallback | ✓ | Reads PDFs natively. |
| `gemini` | fallback | ✓ | Reads PDFs natively. |
| `openai-compatible` | fallback | ✓ | Ollama, vLLM, LM Studio and others, for running locally. |
| `exec` | ✓ | ✓ | Runs a command: request as JSON on stdin, answer as JSON on stdout. |

Providers are compiled in and register themselves, so adding one is a pull
request. `openai-compatible` and `exec` cover everything else without a
fork.

Clef-flash is the default decider (open weights, so it doesn't depend on one
vendor). JEV is a supported alternative that `eval` measures against it.

### 5.3 Configuration and keys

`~/.spendyard/config.toml` (§8), overridable per run with flags and
environment variables:

```toml
[decider]
provider  = "clef-flash"
threshold = 0.85

[extractor]
provider = "anthropic"      # anthropic | openai | gemini | openai-compatible
model    = "claude-haiku-5-5" # the default; claude-opus-5-5 for hard documents
effort   = "low"            # Anthropic only

[providers.openai-compatible]
base_url = "http://localhost:11434/v1"
```

Keys come only from the environment or the OS keychain
(`ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`,
`GEMINI_API_KEY` or `GOOGLE_API_KEY`, `OPENAI_COMPATIBLE_API_KEY`,
`TYPESAFE_API_KEY`, `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`, …), never
the config file. `api_key_env` under `[providers.<name>]` reads a key from
another variable.

**Defaults (M2).** Anthropic with `claude-haiku-5-5` and structured
outputs: cheap enough to run on every receipt. Opus and Sonnet models also
get server-side fallback on refusals. OpenAI, Gemini and `openai-compatible`
have no default model: models change too fast to pick one in code, so the
config or `--extractor provider/model` names it.

**How each provider is held to the schema:**

| Provider | API | Schema |
|---|---|---|
| `anthropic` | Messages | `output_config.format` (`json_schema`) |
| `openai` | Responses | `text.format` (`json_schema`, strict) |
| `gemini` | `generateContent` | `responseJsonSchema` |
| `openai-compatible` | Chat Completions | `response_format` (`json_schema`, strict); servers vary in how strictly they follow it, and validation catches what slips through |

The schema is generated from the Rust types in `extract/schema.rs`, whose
doc comments are the field descriptions the model reads. Every object is
closed and every field required (nullable when optional), as strict modes
need. Spendyard has no keys of
its own and sends nothing anywhere except to the providers configured.

`--decider none` skips the first model; `--extractor none` stops after
deciding.

## 6. Output

```json
{
  "document": {
    "type": "invoice",
    "is_receipt": true,
    "is_invoice": true,
    "status": "paid",
    "source": "attachment:Invoice-2291.pdf",
    "confidence": 0.97
  },
  "vendor": {
    "id": "cursor.com",
    "name": "Cursor",
    "processor": "stripe",
    "account": { "email": "billing@example.com", "workspace": "Example Inc", "customer_id": "cus_…" }
  },
  "payments": [
    { "method": "card", "brand": "visa", "last4": "4242", "amount": "112.40" }
  ],
  "amount": { "total": "112.40", "subtotal": "92.00", "tax": "20.40", "currency": "USD" },
  "billing": {
    "model": "mixed",
    "interval": "month",
    "period": { "start": "2026-10-19", "end": "2026-11-18" },
    "next_renewal": "2026-11-19"
  },
  "lines": [
    {
      "kind": "seat",
      "product": "Cursor",
      "plan": "Pro",
      "description": "Pro (per seat)",
      "quantity": 4,
      "unit": "seat",
      "unit_price": "20.00",
      "amount": "80.00",
      "interval": "month"
    },
    {
      "kind": "usage",
      "product": "Cursor",
      "description": "Premium requests over quota",
      "quantity": 300,
      "unit": "request",
      "unit_price": "0.04",
      "amount": "12.00"
    }
  ],
  "evidence": {
    "amount.total": { "quote": "Amount paid $112.40", "part": 1, "checked": true },
    "payments[0].last4": { "quote": "Visa •••• 4242", "part": 1, "checked": true }
  },
  "flags": [],
  "decisions": { "document_type": { "receipt": 0.03, "invoice": 0.95, "other": 0.01 } },
  "meta": {
    "spendyard_version": "0.1.0",
    "schema_version": "2026-11-19",
    "decider": "clef-flash",
    "extractor": "anthropic/claude-haiku-5-5",
    "tokens": { "decider": 2900, "extractor": 4100 },
    "cost_usd": "0.0061",
    "ms": 2140
  }
}
```

Notes:

- **Money is a decimal string**, never a float.
- **A receipt proves payment and an invoice asks for it.** Most SaaS
  vendors send a "paid invoice", so `type` plus `status` carries the meaning;
  `is_receipt` and `is_invoice` are shortcuts.
- **`lines[].kind`**: `seat`, `plan` (flat fee), `usage`, `addon`,
  `discount`, `credit`, `tax`, `other`.
- **Price per seat** is the `unit_price` of a `seat` line. It isn't a field
  of its own, so an invoice with seats on two products (Jira and Confluence)
  still works.
- **`billing.model`** comes from the lines: subscription lines only →
  `subscription`; usage only → `usage`; both → `mixed`.
- **`vendor.id`** is the canonical domain from the vendor catalog (§9), so the
  cloud can group by it. `processor` is set when a payment processor (Stripe,
  Paddle, Lemon Squeezy, …) sent the receipt for the real vendor.
- **`vendor.account`** holds what can tell two subscriptions with the same
  vendor apart.
- **Nothing is logged.** Neither the input nor the result is logged unless
  `--debug` is given. Results are kept only in the store (§8).

The JSON Schema for this output is generated from the Rust types and
published with every release. `schema_version` changes whenever the output
changes.

## 7. Interfaces

All three run the same pipeline.

### CLI

```bash
spendyard read invoice.pdf               # JSON result to stdout
spendyard read message.eml --pretty      # readable summary
cat receipt.txt | spendyard read -
spendyard read ./inbox/*.eml --jsonl     # one result per line, run in parallel
spendyard decide message.eml             # first model only
spendyard parts message.eml              # list what's in the document, no model calls
spendyard eval ./testdata                # §10
spendyard vendors lookup stripe.com      # vendor catalog
```

Exit codes: 0 receipt or invoice found, 1 none found, 2 error.

### HTTP API: `spendyard serve`

```
POST /v1/read       body: raw file, multipart or JSON { "base64": … }
POST /v1/decide
POST /v1/parts
GET  /v1/health
```

Binds to `127.0.0.1` by default. `--token` requires a bearer token. Keys are
the server's own, from its environment.

### MCP: `spendyard mcp`

MCP over stdio with three tools: `read_document`, `decide_document` and
`list_parts`. Each takes a file path or base64 content. An agent with a
mailbox can then sort and read receipts itself.

### Mailboxes: Gmail and Outlook

The CLI can read receipts straight from a mailbox, so nobody has to export
emails first.

```bash
spendyard login gmail                    # opens the browser; token goes to the OS keychain
spendyard login outlook                  # device code: open a URL, type the code
spendyard accounts                       # signed-in mailboxes

spendyard scan gmail --since 2026-01-01   # results go to the store
spendyard scan outlook --folder Receipts
spendyard scan gmail --new               # only messages since the last scan
spendyard scan imap://mail.example.com   # anything else, over IMAP
```

How a scan works:

1. **Filter on the server first.** Search for likely receipts so most mail
   is never downloaded: Gmail's `q` (`newer_than:1y (receipt OR invoice OR
   faktura OR kvittering OR has:attachment)`), Microsoft Graph's `$search`,
   and From addresses from the vendor catalog (§9).
2. **Decide on headers and a snippet** where that is enough, then download
   the full message (raw MIME) only for likely receipts.
3. **Run the normal pipeline**, one result per message, with `source`
   naming the mailbox and message ID.
4. **Keep the results and remember what was scanned.** Results, message
   IDs and the mailbox's change cursor (Gmail `historyId`, Graph delta
   link) go to the store (§8), so `--new` reads only new mail and a message
   is never read twice.

Providers:

| Mailbox | API | Sign-in | Permission |
|---|---|---|---|
| Gmail / Google Workspace | Gmail API | OAuth in the browser, with a local redirect (`http://127.0.0.1:<port>`). Google doesn't allow device codes for Gmail. | `gmail.readonly` |
| Outlook / Microsoft 365 | Microsoft Graph | Device code, or the browser with a local redirect | `Mail.Read`, `offline_access` |
| Anything else | IMAP | Password or app password, or XOAUTH2 | |

Tokens live only in the OS keychain (macOS Keychain, Windows Credential
Manager, Secret Service on Linux), never in a file.

**The catch: who owns the OAuth app.**

- **Gmail.** `gmail.readonly` is a *restricted* scope. An app that signs in
  more than 100 users with it must pass Google's verification and a
  yearly third-party security assessment (CASA). For an open-source CLI the
  way out is the same as for model keys: **users bring their own OAuth
  client.** They create one in their own Google Cloud project (an internal
  app for a Workspace domain needs no verification), and
  `spendyard login gmail --client-id …` uses it. A guide walks through the
  setup. Spendyard Cloud gets its own verified app later; that cost belongs
  to the cloud.
- **Outlook.** We register one multi-tenant Microsoft Entra app with
  `Mail.Read` (delegated) and complete publisher verification. Users in
  most Microsoft 365 tenants can consent themselves. Where an admin has
  blocked user consent, the admin approves the app once, or the company
  uses its own app registration (`--client-id`), as with Gmail.

The MCP server gets a `scan_mailbox` tool on top of the same code, so an
agent can ask for "receipts from the last month" without handling MIME
itself.

## 8. The store

Everything Spendyard finds is kept in one place, with no setup:

```
~/.spendyard/
  config.toml        # models, thresholds, mailboxes (§5.3)
  spendyard.db       # SQLite: documents, results, receipts, scan cursors
  files/ab/abcd….pdf # originals, only with keep_files = true, named by SHA-256
```

- **It's just there.** The first command that needs it creates it. Nobody
  has to pass `--store`.
- **Somewhere else:** `SPENDYARD_HOME=/path`, or `--home /path` for one
  run. A project folder or a shared drive works the same way.
- **Tokens are not in it.** Mailbox and cloud tokens stay in the OS keychain
  (§7), so the folder can be copied or backed up without leaking access.

### What writes to it

| Command | Stores? |
|---|---|
| `spendyard scan …` | Always. That's the point of scanning. |
| `spendyard read file…` | Yes. `--no-store` for a one-off look. |
| `spendyard parts`, `decide` | No. They only look. |
| `spendyard serve`, `mcp` | No, unless the config says `store = true`. They answer other programs and agents, which shouldn't fill your store by surprise. |
| `spendyard eval` | No. It uses a temporary store. |

### What's in it

SQLite, because the cloud runs on Cloudflare D1, which is SQLite too. The
local store and the cloud share one schema and the same queries, and moving
data between them is moving rows.

| Table | Holds |
|---|---|
| `documents` | One row per input: SHA-256 of the bytes, source (file path, or mailbox and message ID), format, when it was seen. The SHA-256 and the message ID keep a receipt from being counted twice, however often it is scanned or forwarded. |
| `results` | The full JSON from §6 for a document, with the Spendyard version, schema version and models used. Re-running with a better model adds a new result; the newest one counts. |
| `receipts` | The fields reports need, as columns: vendor ID, document type, status, date, total, tax, currency, billing model, interval, period, next renewal, card brand and last four digits. |
| `lines`, `payments` | Line items (kind, product, quantity, unit price, amount) and payment methods. |
| `rates` | Exchange rates per day, from the European Central Bank's free daily feed, so spending can be added up in one currency. |
| `scans` | Mailbox cursors and which messages were already read. |
| `edits` | Corrections by hand: a fixed vendor, a receipt marked personal or ignored. They win over the model's result and survive re-runs. |

Re-extracting never needs the mailbox again when originals are kept
(`keep_files`). Without them, `spendyard rescan` fetches the messages again
by ID.

### Reports

```bash
spendyard report                          # this month and last, per vendor
spendyard report --by vendor --months 12
spendyard report --by month --currency NOK
spendyard subscriptions                   # vendor, plan, interval, seats, last amount, next renewal, card
spendyard receipts --vendor cursor.com    # the receipts behind a number
spendyard edit <receipt> --personal       # leave it out of the company's spend
```

Each command prints a table, or JSON with `--json`. Reports are plain SQL
over the store, and the SQL is documented, so `sqlite3 ~/.spendyard/spendyard.db`
works for anything the commands don't cover.

Subscriptions are worked out from receipts: the same vendor and account
(`vendor.account`, §6) billed at a regular interval is one subscription. A
price per seat that changes between two receipts is a price change.

### To the cloud and back

```bash
spendyard login cloud
spendyard push                # new and changed rows since the last push
spendyard push --with-files   # originals too
spendyard pull                # edits made in the cloud, such as fixed vendors
```

Rows are matched by document SHA-256 and message ID, so pushing twice, or
two people pushing the same forwarded receipt, adds it once. Details in
§11.

## 9. Vendor catalog

A data file in the repo (`vendors/*.toml`), compiled into the binary:

```toml
[cursor]
id      = "cursor.com"
name    = "Cursor"
domains = ["cursor.com", "cursor.sh", "anysphere.inc"]
category = "ai-dev-tools"
pricing = ["seat", "usage"]
```

It covers:

- vendors: canonical ID, name, domains, aliases and category
- payment processors (Stripe, Paddle, Lemon Squeezy, FastSpring, Chargebee,
  Apple, Google Play) and how to find the real vendor on their receipts
- the From addresses each vendor sends receipts from

The catalog sets the vendor hint before any model call and makes `vendor.id`
consistent. Contributions improve results for everyone.

## 10. Evaluation

```bash
spendyard eval ./testdata \
  --decider clef-flash,jev \
  --extractor anthropic/claude-haiku-5-5,openai-compatible/qwen3
```

- `testdata/` holds inputs with expected results in `*.expected.json`,
  personal details removed. It ships in the repo as a public set of SaaS and
  AI-tool receipts, which also shows that Spendyard works.
- For each combination: accuracy per field, how often the decider routed
  correctly, flags raised, cost and time per document.
- The first questions to answer: "renewal notice vs receipt" (decision
  models are weaker at reasoning), Norwegian, Swedish and German receipts,
  and processor vs vendor.
- Any change to prompts, the schema or defaults comes with an `eval` run.

**As built (M4).** `spendyard label <dir>` drafts `<file>.expected.json`
for every document without one, with `claude-opus-5-5` by default, marked
`"reviewed": false` until a person has checked it. Expected answers are
flat and easy to edit:

```json
{ "reviewed": true, "about_money": true, "type": "receipt", "status": "paid",
  "vendor_id": "slack.com", "vendor_name": "Slack", "total": "173.25",
  "currency": "EUR", "last4": ["8239"], "billing_model": "subscription",
  "interval": "month", "invoice_number": "SBIE-3281007", "seats": 21 }
```

A field left out isn't scored; `null` or `[]` means the document has none.
Amounts compare as decimals, names and numbers by letters and digits,
vendor names by containment ("Slack" matches "Slack Technologies
Limited"), card digits as sets. `spendyard eval <dir> -d none,clef-flash
-e anthropic/claude-haiku-5-5,…` runs every combination and reports
accuracy per field, routing (skipped, and receipts missed), flags, and
cost and time per document; `--wrong` lists every miss, `--json` gives one
line per combination.

**First run (2026-11-19, 26 documents: 20 real receipts and 6 made-up
non-receipts, drafted by Opus, not yet reviewed).** Haiku alone and
Clef-flash → Haiku both got receipt-or-not, type, status, total, currency
and card digits right on every document; billing model and interval
86–91%, invoice number 82%. Clef-flash skipped the extractor on the four
made-up non-receipts and on no receipt. Most invoice-number misses are
Stripe receipts carrying both a receipt number and an invoice number,
where the models (Opus included) pick either: the schema needs a rule, or
a separate `receipt_number`.

The real eval set stays private (`private-receipts/eval/`, next to the
repo): it is the user's own mail. A public set in `testdata/` comes from
made-up receipts later.

## 11. Spendyard Cloud

The binary is complete for one person: it reads, scans, keeps and reports.
The cloud sells **sharing, teams and always-on scanning**, not the
arithmetic. A founder alone gets real value for free, which is how
Spendyard spreads; a company that wants everyone's receipts in one place
pays.

| Open binary | Cloud |
|---|---|
| Reading every format; decide and extract | A shared store for the company: everyone's receipts, duplicates across people removed |
| Validation and evidence; vendor catalog | Edits by several people, with history |
| Gmail, Outlook and IMAP scans from your machine | Scanning around the clock on the server; a forwarding address; shared inboxes |
| One person's store (§8) | Cards' last four digits matched to people; spend per person and team |
| Spending per vendor and month; subscriptions; price changes | Renewal calendar and alerts ("Linear renews yearly in 14 days"); the weekly email |
| `push` and `pull` | AI tool spend for the whole company: seats vs usage per tool and month |
| CLI, `serve`, `mcp`, `eval` | Expense exports for accounting (Fiken, Tripletex, Xero, QuickBooks) |
| | Later: seats paid for vs used, by connecting Google Workspace, Microsoft 365 or GitHub |

### Cloud v1 for startups

1. Sign up, get a forwarding address, forward receipts (or connect one
   Gmail or Microsoft 365 mailbox).
2. **Subscriptions**: every vendor, what it costs per month, interval, seats,
   card and owner.
3. **Renewals**: what renews in the next 30 days.
4. **AI spend**: per tool and month, seats vs usage.
5. A weekly email: new subscriptions, price changes, upcoming renewals.

### How the cloud uses the binary

The core compiles to WebAssembly and runs in a Cloudflare Worker, so the
cloud reads documents with exactly the code that is open. The cloud uses its
own model keys and bills for usage.

### Sending from the CLI to the cloud (later)

A company can also run `spendyard` itself, on a laptop or a server of its
own, and send results to the cloud:

```bash
spendyard login cloud                    # device code: open a URL, type the code
spendyard scan gmail --new --push        # scan into the local store, then push
spendyard push                           # new and changed rows from the store (§8)
spendyard push --with-files              # originals too
spendyard pull                           # edits made in the cloud
```

- By default only **rows** are sent: results, receipts, lines and edits.
  Documents stay where they are, which suits companies that won't hand
  their mailbox to a SaaS.
- `--with-files` also uploads the originals, so the cloud can show them.
- The cloud's database uses the store's schema (both are SQLite), so a push
  is rows going from one to the other, matched by document SHA-256 and
  message ID.
- The cloud might also run some of the work for the CLI: its own model keys
  (no need to bring your own), a vendor catalog newer than the binary's, or
  receipts stored for search. These are services the CLI calls when you are
  signed in to the cloud. The binary must keep working fully without them.

Not part of v1; the API for it is designed with Cloud C1.

Cloud data handling (retention, deletion, subprocessors) gets its own
document before launch.

## 12. Milestones

| Milestone | Scope |
|---|---|
| **M0: Repo** | One Cargo package (library plus CLI), CI, release builds for macOS and Linux (arm64 and x86_64), Apache 2.0 licence, README. |
| **M1: Read** | `Document` from eml, msg, PDF (text layer), images, HTML and text; `spendyard parts`. Pick the PDF crates and the HEIC approach. |
| **M2: Extract** | `Extractor` trait; `anthropic`, `openai`, `gemini` and `openai-compatible`; output schema; validation and evidence checks; `spendyard read`. Done: run end to end with a local model; the hosted providers still need a first real run. |
| **M3: Decide** | `Decider` trait; `clef-flash`, `clef`, `jev`, `systemone`; routing and thresholds; LLM fallback. Built, with the extractor as the fallback; `spendyard decide`, `spendyard config`. Not yet run against a real decision model (no key). |
| **M4: Eval** | `spendyard eval`, first 200 labelled documents, defaults chosen from the results. Built: `spendyard label` and `spendyard eval`; a first private set of 26. Still to do: grow it to 200 with real non-receipts from an inbox, review the drafts, then set the threshold and default models from it. |
| **M5: Serve and MCP** | `spendyard serve` and `spendyard mcp`; `exec` provider. |
| **M5b: Mailboxes** | `login` and `scan` for Outlook (Graph, our Entra app) and Gmail (own OAuth client), IMAP; keychain; `scan_mailbox` MCP tool; Gmail setup guide. |
| **M5c: Store and reports** | `~/.spendyard` with SQLite (`rusqlite`, SQLite compiled in); `read` and `scan` store results; `report`, `subscriptions`, `receipts`, `edit`; exchange rates. Could come before M5b, fed by `read` on files. |
| **M6: Vendor catalog** | First 200 vendors, AI tools first; processor rules. |
| **M7: Public release** | Docs, Homebrew, crates.io, announcement. |
| **Cloud C1** | Forwarding address, storage, subscriptions, renewals, AI spend, weekly email. |
| **Cloud C2** | `spendyard login cloud`, `push` and `pull` against the shared store; cloud services the CLI can call (models, catalog, storage); a verified Google app for the cloud. |

## 13. Open questions

- Name and home on GitHub (organization, crate name).
- Does `lopdf` hold up on real receipts (§3.1)?
- Do Gemini's and OpenAI's strict schema modes accept the generated schema
  as is? Only Anthropic's request shape and a local model have been run;
  the others are tested against recorded request and response shapes.
- Is JEV still in early access, and on what retention terms?
- Does Cloudflare's REST API wrap Clef's answers in `{ "result": … }`? The
  client accepts both; the first real call settles it.
- Gmail for the CLI: is "bring your own OAuth client" acceptable to
  startups, or is it worth paying for Google's verification early?
- Default thresholds per question, set from the `eval` results.
- Where the line goes between free reports and the cloud: is "price
  changes" free, and is the weekly email a cloud-only feature?
- Cloud stack: Cloudflare Workers with the WebAssembly core is the
  assumption. Confirm when Cloud C1 starts.
