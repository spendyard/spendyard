# Spendyard

Spendyard reads receipts and invoices — emails, PDFs, images, text, or
straight from Gmail and Outlook — and works out what you're paying for:
the vendor, the card, the price, the price per seat, usage charges, and
whether it's billed monthly, yearly, by usage or a mix.

It is one binary. It runs as a CLI, as an HTTP API and as MCP tools for
agents, with your own model API keys. Nothing is sent anywhere except to the
model providers you configure.

> **Status:** early development. `spendyard parts` and `spendyard read` work;
> the cheap first model, mailboxes and the store come next. The design is in
> [docs/design.md](docs/design.md).

## How it works

```
document ──▶ read ──▶ locate ──▶ decide ──▶ extract ──▶ validate ──▶ JSON
             (fixed)  (fixed)    (cheap      (capable    (fixed)
                                  model)      model)
```

1. **Read** emails, PDFs, images, HTML and text into one shape.
2. **Locate** the part that holds the receipt (the body, or the PDF it says
   is attached).
3. **Decide** with a cheap decision model, such as Clef or JEV: is it a
   receipt or an invoice, paid or due, billed by seat or by usage? Most
   email stops here.
4. **Extract** the details with a capable model (Anthropic, OpenAI, Gemini or
   a local model).
5. **Validate**: totals add up, and every value comes with a quote that must
   appear in the document.

## Usage

```bash
export ANTHROPIC_API_KEY=…
spendyard read invoice.pdf                 # JSON
spendyard read message.eml --pretty        # a readable summary
spendyard read *.eml                       # one JSON line per document
spendyard read receipt.png -e gemini/<model>
spendyard read receipt.eml -e openai-compatible/qwen3:4b   # Ollama, locally
spendyard parts message.eml --text         # what's inside, no model
```

Settings live in `~/.spendyard/config.toml`:

```toml
[extractor]
provider = "anthropic"
model    = "claude-haiku-5-5"

[providers.openai-compatible]
base_url = "http://localhost:11434/v1"
```

### Coming later

```bash
spendyard login gmail
spendyard scan gmail --since 2026-01-01   # kept in ~/.spendyard
spendyard report --by vendor --months 12
spendyard subscriptions
spendyard serve
spendyard mcp
```

## Building

```bash
cargo build --release
```

The binary is `target/release/spendyard`.

## Licence

[Apache 2.0](LICENSE).
