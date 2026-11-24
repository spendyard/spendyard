# Spendyard design

Status: draft, 2026-11-19.

Spendyard has two parts:

1. **`spendyard`, an open-source binary** (Apache 2.0). It reads a
   document — an email, PDF, image or text — and works out whether it is a
   receipt or invoice, who the vendor is, how it was paid, what it cost and
   how it is billed. It scans Gmail and Outlook, keeps what it finds in a
   local store, and reports one person's spending. It runs as a CLI and as
   MCP tools for agents, using the user's own model API keys.
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
- A built-in way to compare models on labelled data (`spendyard eval run`).
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
  run the exact same code that people run locally. So the repository is a
  workspace: `spendyard-core` (no file, network, clock or OS; builds for
  `wasm32`), `spendyard-models` (the providers over HTTP), `spendyard-wasm`
  (the core for JavaScript, the npm package `@spendyard/core`) and the CLI.
  Models are called from the host: the CLI through `spendyard-models`, the
  cloud through Workers AI and AI Gateway, both with the core's prompts and
  its parsing of the answers.
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
| `document_type` | choice | receipt, invoice, renewal_notice, payment_failed, quote, trial_ending, card_statement (§8), other |
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
`spendyard eval decide` runs the first model alone. The state sent is the
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
spendyard eval decide message.eml        # first model only
spendyard parts message.eml              # list what's in the document, no model calls
spendyard eval run ./testdata            # §10
spendyard vendors lookup stripe.com      # vendor catalog (§9)
spendyard transactions list --missing    # card payments with no receipt (§8)
```

Exit codes: 0 receipt or invoice found, 1 none found, 2 error.

### No HTTP API in the binary (decided in M5)

The HTTP API is Spendyard Cloud's (§11). The binary doesn't serve one:
programs call the CLI, which prints JSON (`spendyard read --jsonl`), agents
use MCP, and other models plug in with `exec`. A local server would mostly
be a way to put model keys on a network by mistake. A `serve` command was
built and taken out again; it is easy to bring back if self-hosters ask.

### MCP: `spendyard mcp`

MCP over stdio with three tools: `read_document`, `decide_document` and
`list_parts`. Each takes `path`, or `base64` with an optional `name`, and
answers with the same JSON the CLI prints. Errors (an unreadable file, no
first model configured) come back as tool errors an agent can read, not
protocol errors. An agent with a mailbox can then sort and read receipts
itself. To add it to an MCP client:

```json
{ "mcpServers": { "spendyard": { "command": "spendyard", "args": ["mcp"] } } }
```

### `exec`: any program as a model

`provider = "exec"` with `command = ["program", "arg"]` under `[extractor]`
or `[decider]` makes any program a model. Spendyard writes the request as
JSON to its standard input and reads JSON from its standard output:

- extractor: `{ "kind": "extract", "system", "schema", "blocks": [text,
  pdf or image blocks, base64] }` in; the schema's JSON out, or `{ "output",
  "usage": { "input", "output" }, "model" }`.
- first model: `{ "kind": "decide", "state", "questions", "images" }` in, in
  the JEV format; `{ "answers", "usage", "model" }` out.

A non-zero exit is an error, with the end of standard error in the
message. It lives in the CLI, not the library, because it starts
processes.

### JSON, not protobuf

Every interface is JSON: MCP is JSON-RPC by definition, models answer in
JSON Schema, `exec` should be easy from any language, and the cloud runs in
Workers, where JSON is native. The result format is versioned
(`schema_version`) and has a generated JSON Schema. If the cloud API later
wants typed clients and binary payloads, Connect-style RPC can serve the
same messages as both JSON and protobuf from one definition; that's a
cloud decision, not the binary's.

### Mailboxes: Gmail and Outlook

The CLI can read receipts straight from a mailbox, so nobody has to export
emails first.

```bash
spendyard mailbox login gmail                    # opens the browser; token goes to the OS keychain
spendyard mailbox login outlook                  # device code: open a URL, type the code
spendyard mailbox list                           # signed-in mailboxes

spendyard mailbox scan gmail --since 2026-01-01   # results go to the store
spendyard mailbox scan outlook --folder Receipts
spendyard mailbox scan gmail --new               # only messages since the last scan
spendyard mailbox scan imap://mail.example.com   # anything else, over IMAP
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

**As built for Outlook (M5b).** `spendyard mailbox login outlook` signs in with a
device code (`--client-id`, `--tenant`, or `[mailboxes.outlook]` in the
config); the address and app go in `~/.spendyard/accounts.json`, the
refresh token in the OS keychain (or `~/.spendyard/tokens.json`, mode 0600,
when there is no keychain), rotated on every use. `spendyard mailbox scan outlook`:

1. lists messages newest first since the last scan (default 30 days ago;
   `--since`, `--folder`, `--limit`) with subject, sender, preview and
   attachments only;
2. keeps the likely ones with a local pre-filter, no model: money words in
   eight languages, billing senders and processors, or an attachment plus
   an amount (`--all` skips it);
3. downloads only those as MIME and runs the normal pipeline, so the first
   model (if configured) skips most of what's left;
4. prints one JSON line per message with a `source` (account, Message-ID,
   date, sender, subject), or summaries with `--pretty`; `--save` keeps the
   `.eml` files and `--dry-run` only lists;
5. saves what it reads in the store (§8), skips mail the store already
   has by Message-ID (`--again` reads it anyway), and remembers the newest
   message seen in the store's `scans` table, unless `--since` was given or
   `--limit` cut the listing short. `--no-store` does none of this.

Run on the first real mailbox (2026-11-19): the pre-filter kept 91 of the
400 newest messages; of 14 kept among the newest 60, Clef-flash skipped 7
(newsletters, a security alert, a support thread, internal mail) and Haiku
read 7 failed payments and unpaid invoices. 19 seconds in all.

**IMAP (built after Outlook).** For Gmail without an OAuth app, and for
iCloud, Fastmail, Yahoo, Proton (through Proton Bridge) and self-hosted
mail: `spendyard mailbox login imap you@gmail.com` asks for the password without
echoing it (or reads it from standard input), checks it by signing in, and
keeps it in the keychain. The server is known for common providers and
`imap.<domain>:993` otherwise (`--server` overrides); Outlook and Microsoft
365 refuse passwords over IMAP and are pointed at `login outlook`. Gmail,
iCloud, Yahoo and Fastmail need an **app password** (Gmail: 2-Step
Verification on, then myaccount.google.com/apppasswords; some Workspace
admins turn app passwords off).

`spendyard mailbox scan imap` scans Gmail's "All Mail" (found by its special-use
flag, since the name is translated) or INBOX elsewhere (`--folder`
overrides). It searches `SINCE` the date, or after the last UID seen when
the folder's UIDVALIDITY hasn't changed; fetches only headers for the
pre-filter, without marking anything read; downloads the likely ones; and
reads them through the same steps as Outlook. Connections are TLS with the
system's certificate store; plain IMAP is allowed only to this machine on a
port other than 993 (Proton Bridge, test servers).

Tested against a local IMAP server (GreenMail) with receipts, a forwarded
receipt, a renewal notice, a failed payment and non-receipts: all read as
expected, and a second scan found nothing new.

`spendyard mailbox list` and `spendyard mailbox logout` manage sign-ins, and MCP has a
`scan_mailbox` tool for both. Until Spendyard registers its own Entra app, Outlook
needs an app ID of the user's own (delegated `Mail.Read`, public client
flows allowed).

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
  `spendyard mailbox login gmail --client-id …` uses it. A guide walks through the
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
| `spendyard mailbox scan …` | Always. That's the point of scanning. |
| `spendyard read file…` | Yes. `--no-store` for a one-off look. |
| `spendyard transactions` | Only the matches: `match` sets each charge's receipt. |
| `spendyard payments` | The payments' numbers and evidence, worked out again each time. |
| `spendyard parts`, `eval decide` | No. They only look. |
| `spendyard mcp` | No, unless the config says `store = true`. It answers agents, which shouldn't fill your store by surprise. |
| `spendyard eval run` | No. It uses a temporary store. |

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
| `transactions` | Card payments from statements (and, in the cloud, card platforms and banks), with the receipt each one matched. Below. |

Re-extracting never needs the mailbox again when originals are kept
(`keep_files`). Without them, `spendyard rescan` fetches the messages again
by ID.

### Reports

```bash
spendyard report                          # the last 12 months, per vendor
spendyard report --by month --currency NOK
spendyard subscriptions                   # vendor, interval, cost per month, seats, card, next renewal, status
spendyard documents list --vendor cursor  # the receipts behind a number, with their IDs
spendyard documents get 412               # one whole: what was read, corrections, its payments
spendyard documents edit 412 --personal   # leave it out of the company's spend
spendyard documents edit 412 --vendor "Atlassian"  # group it under another name
```

Each command prints a table, or JSON with `--json`, and
`sqlite3 ~/.spendyard/spendyard.db` works for anything they don't cover.

**As built (M5c).** The tables are those above, in one SQLite file,
versioned with `PRAGMA user_version`. A store of another version is
refused, not upgraded: move it away and read again (it's a cache of what
was read, and the originals are where they came from). A document is found again by its
SHA-256 or its Message-ID; every result is kept, and the newest fills
`receipts`, `lines` and `payments`. Spend is what was paid: receipts and
paid invoices, positive totals, not marked personal or ignored, and each
counted once (same vendor, invoice or receipt number and total). Amounts
are added as exact decimals, converted through the euro with the ECB's
reference rate on or before each receipt's date; rates are fetched from
the ECB's data API when a report needs them and kept in `rates`. The
report currency defaults to the one paid in most often.

`subscriptions` groups recurring documents by vendor (the edit's name,
else the vendor's domain, else its name). For each: the interval seen,
the last paid amount as a monthly cost, seats and card from the newest
document that has them, the next renewal (stated, or estimated from the
last payment and the interval), and a status: `payment failed` or `due`
when the newest document says so, `lapsed?` when the last payment is older
than the interval allows, otherwise `active`. Grouping by vendor and
account, and price changes, come later.

### Charges: what the card paid (M6b)

Receipts say what a vendor billed; a card statement says what the card
actually paid. Matching the two finds receipts that are missing, receipts
paid with a card nobody connected, and the exact amount in the company's
currency, card fees included.

`read` takes a statement like any other document: you don't need to know
what a file is before handing it over, and `scan` handles the monthly
statement in a mailbox without a special case. Charges have their own
group for what works on the stored ones:

```bash
spendyard read dnb-2026-05.pdf --pretty   # a statement: every line becomes a charge, then matching runs
spendyard transactions                    # match again and summarize, after new receipts came in
spendyard transactions list --missing     # purchases with no receipt
spendyard transactions list --card 3776 --month 2026-05 --json
```

```text
../charges.pdf → card_statement, none
  Statement  DNB Corporate Mastercard · 814124803 · issued 2026-05-15 · due 2026-06-04
  Charges    91 · total 117711.57 NOK
  Cards      •••• 3776 INNKJØPSKONTO LISENSER (91)
  Check      ✓ rows: rows 1 to 91, none missing
  Check      ✓ count: 91 lines read, the statement says 91
  Check      ✓ total: the charges add up to the total, 117711.57
  Check      ✓ conversion: original amount × rate gives the amount for all 88 foreign charges
  Check      ✓ cards: every purchase is on a card: •••• 3776
  Read by    claude-haiku-5-5 · 3 calls, 0 reread · 35252 in, 16321 out · 21.8 s
  …
91 transactions
    49 matched to a receipt
    42 missing a receipt: 78362.46 NOK
```

The group is `transactions`, not `statements`: what is stored, listed and
matched is a payment on a card or an account, and the statement is only
one source of them. The cloud fills the same table from card platforms
(Pleo, Ramp, Brex) and banks through open banking, so the matching is
written once. It's the dashboard's word for them too, and the table's:
`transactions`.

1. **Recognise.** `document_type` has a `card_statement` option, also in
   the extractor's schema: a card issuer's list of many purchases, even
   when it's titled invoice (DNB's is). When the first model says so, the
   receipt extractor never runs. Without a first model, a PDF longer than
   four pages is asked about on its first page only, so a statement isn't
   read in full as one invoice first. A receipt's result then gets a
   `statement` section with the summary, cards, charges and checks.
2. **Extract.** `statement::read_statement` in the library, with its own
   output schema: the issuer, number, dates, currency, total and line
   count from the summary, and each charge with its row number, purchase
   and posting dates, the merchant as printed (`CURSOR USAGE MID APR, SAN
   FRANCISCO, US`), the card's last four digits and name, the amount in
   the statement's currency, the original amount, currency and rate for
   foreign charges, and its kind (purchase, refund, fee, interest,
   payment, other). A PDF is split into groups of four pages, each sent as
   its own small PDF, all at once; text (a CSV) in groups of 120 lines,
   each with the header line. Statements differ from bank to bank, so a
   model reads them rather than a parser per issuer; the checks below are
   what make that safe.
3. **Check.** The rows run from 1 with no gaps, their number is the line
   count the summary states, they add up to its total (with or without
   payments to the card account), original amount × rate gives the amount
   to the øre for every foreign charge, and every purchase has a card. A
   group with gaps in its own rows, a conversion that doesn't add up or no
   charges is read again once, and the better answer kept. The quote check
   receipts get isn't used: the arithmetic checks more, and needs no text
   layer.
4. **Store.** A `transactions` table (§8): the statement's document, position,
   issuer, row, dates, merchant as printed, card and name, amount and
   currency, original amount, currency and rate, kind, the vendor it was
   mapped to, and the receipt it matched. Reading the statement again
   replaces its charges. Matching runs after `read` saves one.
5. **Match.** Each purchase and refund is paired with a receipt or paid
   invoice (not ignored) by:
   - **vendor**: `Catalog::by_merchant` finds it from the merchant as
     printed: a vendor's `descriptors` first (`descriptors = ["NOT JUST
     TI"]`, matched as a prefix without case), then each part of the name
     split at `*` and `:`, with city and country dropped, as a domain
     (`PLAIN.COM`), then as a name with words dropped from the end
     (`CURSOR USAGE MID APR` → Cursor). On the DNB statement, 17 of 18
     distinct merchants were found;
   - **amount**: exactly, in the receipt's currency (the original amount)
     or the statement's; otherwise converted with the ECB rate of the
     charge's date, within 4% for the card's currency fee;
   - **date**: the charge from 3 days before the receipt's date to 10 days
     after (some vendors charge the card a week after the receipt);
   - **card**: the last four digits must agree when both have them.

   Exact amounts beat converted ones, then the closer date; each receipt
   is used once. What's left is the report: purchases missing a receipt,
   and receipts on the statements' cards, within their dates, with no
   charge (often someone's own card, or one that isn't connected). Fees,
   interest and payments to the card account need no receipt.

**One payment, several documents.** The same payment often comes as
more than one document: a receipt saved as an email and as its PDF, the
invoice and the receipt Stripe sends for it, the same statement sent
twice. Matching and spend reports treat these as one: receipts with the
same vendor, amount and invoice or receipt number (compared by letters and
digits, since `B085EFDD-0038` on an email is `B085EFDD 0038` on its PDF),
and a paid invoice with a receipt for the same vendor and amount within
three days when they share no number (Plain's PDFs carry only the invoice
number, its emails only the receipt number). Charges are kept once across
statements by card, date, amount and row (or merchant).

**As built.** A PDF text layer whose fonts don't decode (DNB's comes out
as `Na!$ on &ar#`) is now treated as no text, measured by symbols per
letter, so the model gets the pages. Extractors answer in any output
format (`Extractor::answer` with an `OutputFormat`); `extract` reads
receipts on top of it, and `exec` gets a `format` field. Still to do:
CSV column mappings kept per issuer so a CSV needs no model, a charge
marked personal, `receipts` showing the charge it matched, and the
opening-and-closing-balance check for statements that print balances.

### To the cloud and back

```bash
spendyard cloud login
spendyard cloud push                # what's new or changed since the last push, originals included
spendyard cloud pull                # corrections made in the cloud
```

Documents are matched by SHA-256 and message ID, so pushing twice, or two
people pushing the same forwarded receipt, adds it once. Details in §11,
and in the platform's `docs/push-pull.md`.

## 9. Vendor catalog

One file per vendor in `vendors/`, named by the vendor's ID (its main
domain), compiled into the core by `crates/core/build.rs`, so the CLI and the cloud's
WebAssembly carry the same catalog:

```toml
# vendors/anthropic.com.toml
name = "Anthropic"
aliases = ["Anthropic, PBC"]
domains = ["claude.com", "claude.ai"]
category = "ai"
pricing = ["usage", "seat"]
```

One file per vendor keeps contributions small and conflict-free. Payment
processors and billing platforms (Stripe, Paddle, Lemon Squeezy, Orb,
Chargebee, Apple, …) are in `vendors/_processors.toml`: their domains never
identify a vendor. If the catalog grows its own crowd of contributors, it
can move to its own repo without changing the format.

**As built (M6).** 248 vendors (AI tools first, then every vendor on the
first real mailbox, then common SaaS) and 18 processors. It is used in
three places:

- **Results.** `vendor.id` becomes the catalog ID, found by the domain the
  model read, then the name (ignoring legal suffixes, and using the trading
  name after "dba" or "t/a"), then the sender's domain unless the sender is
  a processor. `vendor.name` becomes the catalog's name, the name as
  printed is kept as `vendor.legal_name`, and `vendor.category` and
  `vendor.known` are added.
- **The prompt.** When the email's sender is a known vendor, the extractor
  gets a `<known-sender>` note.
- **Reports.** Receipts group by catalog ID, already-stored ones included
  (the lookup happens when reporting, so nothing needs reading again).
  Names the catalog doesn't know group by their cleaned-up name.

`spendyard vendors` summarizes the catalog, `vendors lookup <name, domain
or address>` finds one, `vendors list --category ai` lists, and `vendors
unknown` lists vendors on stored receipts the catalog lacks: the to-do list
for contributions. On the first real store it found 5, none of them SaaS.

**Local vendors.** The built-in catalog only changes with a release, so
files in `~/.spendyard/vendors/*.toml`, in the same format, are read at
startup on top of it: one named like a built-in vendor replaces it, others
are added, and broken ones are skipped with a warning. The library takes
them as text from the CLI (it never reads files itself), so the cloud can
pass its own. Fetching a newer catalog (`spendyard vendors update` from a
release, or `pull` from the cloud) comes later.

**Logos** aren't stored. A vendor's ID is its domain, so the dashboard
fetches its icon from `https://www.google.com/s2/favicons?domain={id}&sz={size}`.
In the cloud that goes through our own cached proxy, so viewers' browsers
don't tell Google which vendors a company uses.

## 10. Evaluation

```bash
spendyard eval run ./testdata \
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

**As built (M4).** `spendyard eval label <dir>` drafts `<file>.expected.json`
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
Limited"), card digits as sets. `spendyard eval run <dir> -d none,clef-flash
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
where the models (Opus included) pick either. Fixed the same day with a
separate `receipt_number` and a rule in the prompt: invoice numbers went
from 82% to 95.5% and receipt numbers are 100% on the same set.

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
| CLI, `mcp`, `exec`, `eval` | Expense exports for accounting (Fiken, Tripletex, Xero, QuickBooks) |
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

### Bring your own keys? Not at first

Measured cost per document is about $0.0015 (Clef-flash first, Haiku
extracting). A startup's inbox of a few thousand messages a month, most of
them skipped by the first model, costs a few dollars of model use: too
little to be worth a customer managing keys, and easy to fold into the
subscription. Our own keys also keep the models, prompts and thresholds the
ones `eval` measured, one agreement with each provider (zero retention,
logging off), and one place to handle rate limits and outages.

Bring-your-own-keys is still worth having later, for a different reason:
compliance. A company that wants its documents processed under its own
agreement with Anthropic, OpenAI, Google or Azure (or on its own models)
could set keys per organization in the cloud, stored encrypted, used only
for that organization, and shown as theirs in the results. Until then, such
a company runs the open binary with its own keys and pushes only results
(below), which already gives it that.

### Sending from the CLI to the cloud (later)

A company can also run `spendyard` itself, on a laptop or a server of its
own, and send results to the cloud:

```bash
spendyard cloud login                      # device code: open a URL, check the code
spendyard mailbox scan gmail --new         # scan into the local store
spendyard cloud push                       # then send what's new or changed (§8)
spendyard cloud pull                       # corrections made in the cloud
```

Everything about the cloud itself is under `spendyard cloud` (`login`,
`logout`, `status`, `switch`, `push`, `pull`), so what
talks to Spendyard is easy to see: those, and two flags on commands that
already exist, `--cloud` and `-e cloud`. Reading and scanning only fill the
local store; pushing is a step of its own. The mailboxes the CLI reads are under
`spendyard mailbox` (`login`, `list`, `scan`, `logout`).

- What's sent is documents, their readings (results and the rows made from
  them) and edits, and always their originals: the CLI keeps each original
  it reads in `~/.spendyard/originals/`, so it has it to send. Never
  payments: the cloud works out its own. The mailbox itself stays the
  CLI's, which suits companies that won't hand theirs to a SaaS.
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
| **M3: Decide** | `Decider` trait; `clef-flash`, `clef`, `jev`, `systemone`; routing and thresholds; LLM fallback. Built, with the extractor as the fallback; `spendyard eval decide`, `spendyard config`. Not yet run against a real decision model (no key). |
| **M4: Eval** | `spendyard eval`, first 200 labelled documents, defaults chosen from the results. Built: `spendyard eval label` and `spendyard eval run`; a first private set of 26. Still to do: grow it to 200 with real non-receipts from an inbox, review the drafts, then set the threshold and default models from it. |
| **M5: MCP and exec** | `spendyard mcp`; `exec` provider. Done; `serve` was dropped (the HTTP API is the cloud's). |
| **M5b: Mailboxes** | `login` and `scan` for Outlook (Graph, our Entra app) and Gmail (own OAuth client), IMAP; keychain; `scan_mailbox` MCP tool; Gmail setup guide. Outlook done and run on a real mailbox; IMAP done (Gmail with an app password, iCloud, Fastmail, Yahoo, others), tested on a local server; still to do: Gmail through its API, registering Spendyard's own Entra app. |
| **M5c: Store and reports** | `~/.spendyard` with SQLite (`rusqlite`, SQLite compiled in); `read` and `mailbox scan` store results; `report`, `subscriptions`, `documents list`, `documents edit`; exchange rates. Done; still to do: grouping by account, price changes. |
| **M6: Vendor catalog** | First 200 vendors, AI tools first; processor rules. Done: 248 vendors, 18 processors, `spendyard vendors`. |
| **M6b: Charges** | `card_statement` in the decision; `read` reads PDF and CSV statements into the `charges` table, checked; `descriptors` and `by_merchant` in the catalog; matching against receipts; `charges list` and `charges match`. Done and run on a real 11-page DNB statement (91 charges, every check held). Still to do: CSV mappings per issuer, personal charges, balances. |
| **M7: Public release** | Docs, Homebrew, crates.io, announcement. |
| **Cloud C1** | Forwarding address, storage, subscriptions, renewals, AI spend, weekly email. |
| **Cloud C2** | `spendyard cloud login`, `cloud push` and `cloud pull` against the shared store; cloud services the CLI can call (models, catalog, storage); a verified Google app for the cloud. |

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
