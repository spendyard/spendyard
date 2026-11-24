# Spendyard

Spendyard reads receipts and invoices — emails, PDFs, images, text, or
straight from Gmail and Outlook — and works out what you're paying for:
the vendor, the card, the price, the price per seat, usage charges, and
whether it's billed monthly, yearly, by usage or a mix.

It is one binary. It runs as a CLI and as MCP tools for agents, with your
own model API keys. Nothing is sent anywhere except to the
model providers you configure.

> **Status:** early development. `parts`, `read`, `config`, `eval`,
> `mcp`, `mailbox login`/`mailbox scan` for Outlook and IMAP (Gmail with an app password),
> the store with its reports, and the cloud (`cloud login`, `push`, `pull`)
> work. The design is in
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
spendyard read *.eml -d clef-flash         # a cheap first model before the extractor
spendyard eval decide *.eml -d jev         # only the first model's answers
spendyard parts message.eml --text         # what's inside, no model
spendyard config                           # settings in effect, and which keys are set
spendyard config init                      # write a commented ~/.spendyard/config.toml
spendyard eval label ./eval                # draft expected answers with a careful model
spendyard eval run ./eval -d none,clef-flash # score model combinations against them
spendyard mcp                              # MCP tools over stdio, for agents
```

As an MCP server for Claude, Cursor or any other client:

```json
{ "mcpServers": { "spendyard": { "command": "spendyard", "args": ["mcp"] } } }
```

Any program can be a model: `provider = "exec"` and `command = [...]` under
`[extractor]` or `[decider]` in the config. It gets the request as JSON on
standard input and answers with JSON on standard output.

API keys come from the environment, or from `.env.local` / `.env` in the
current folder or `~/.spendyard/.env`; variables already set win.

Settings live in `~/.spendyard/config.toml`; keys come from the environment:

```toml
[extractor]
provider = "anthropic"            # ANTHROPIC_API_KEY; ANTHROPIC_BASE_URL and CLOUDFLARE_AI_GATEWAY_TOKEN for an AI Gateway
model    = "claude-haiku-5-5"

[decider]
provider  = "clef-flash"   # CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID; CLOUDFLARE_AI_GATEWAY to go through an AI Gateway
threshold = 0.85

[providers.openai-compatible]
base_url = "http://localhost:11434/v1"
```

Outlook and Microsoft 365, straight from the mailbox:

```bash
spendyard mailbox login outlook --client-id <app> --tenant <tenant>   # a code to enter in the browser
spendyard mailbox scan outlook --dry-run          # what it would read
spendyard mailbox scan outlook -d clef-flash      # read new mail since the last scan
spendyard mailbox scan outlook --folder Receipts --since 2026-01-01 --save ./receipts
spendyard mailbox list
```

Gmail, iCloud, Fastmail, Yahoo and most other mail over IMAP, with an app
password (Gmail: turn on 2-Step Verification, then create one at
myaccount.google.com/apppasswords):

```bash
spendyard mailbox login imap you@gmail.com        # asks for the app password, keeps it in the keychain
spendyard mailbox scan imap --dry-run
spendyard mailbox scan imap -d clef-flash
```

Everything read or scanned is kept in `~/.spendyard/spendyard.db`, each
original beside it in `~/.spendyard/originals/` (`--no-store` to skip
both), and reported from there:

```bash
spendyard report                           # spend per vendor, last 12 months
spendyard report --by month --currency NOK # converted with ECB rates
spendyard subscriptions                    # cost per month, seats, card, next renewal, status
spendyard documents list --vendor slack    # the receipts behind a number
spendyard documents get 412               # one whole: where it came from, what was read, its payments
spendyard documents edit 412 --personal    # leave one out of the spend
spendyard vendors unknown                  # vendors on your receipts the catalog lacks
```

Spendyard Cloud, for the whole company's spend, is optional. Sign this
machine in, then push what it reads, which the company sees together:

```bash
spendyard cloud login                      # opens the browser: check the code, pick the organisation
spendyard cloud status                     # who it's signed in as, and where it acts
spendyard cloud switch                     # where it acts, and your other organisations
spendyard cloud switch nordlys             # act in another
spendyard cloud push --dry-run             # what the cloud would take, keeping nothing
spendyard cloud push                       # what's new or changed since the last push, originals included
spendyard cloud pull --dry-run             # corrections made in the cloud, field by field, changing nothing
spendyard cloud pull                       # bring them to the documents this machine has
spendyard cloud logout
```

### Coming later

Gmail through its own API, for workspaces where IMAP is turned off.

## Installing

On macOS or Linux (amd64 and arm64), with Homebrew:

```bash
brew install spendyard/tap/spendyard
```

or the install script, which puts it in `~/.local/bin` after checking it
against the release's checksums (`SPENDYARD_VERSION=v0.1.0` for a given
release, `SPENDYARD_INSTALL_DIR` for another folder):

```bash
curl -fsSL https://spendyard.com/install.sh | bash
```

### Releasing

Set `version` in `Cargo.toml` (`[workspace.package]`), commit, and push a tag
that matches it:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` builds the four binaries, attaches them with
`install.sh` and `checksums.txt` to the tag's GitHub release, updates the
cask in [spendyard/homebrew-tap](https://github.com/spendyard/homebrew-tap)
(`scripts/homebrew.sh`, with the `HOMEBREW_TAP_TOKEN` secret), then installs
the release both ways to check it. spendyard.com/install.sh serves the latest
release's `install.sh`.

## Building

```bash
cargo build --release
```

The binary is `target/release/spendyard`.

The reading itself is a library, `spendyard-core` in `crates/core`, which
also builds for WebAssembly: `scripts/build-wasm.sh` makes it the npm package
`@spendyard/core` (in `crates/wasm/pkg`), which Spendyard Cloud reads
documents with.

## Licence

[Apache 2.0](LICENSE).
