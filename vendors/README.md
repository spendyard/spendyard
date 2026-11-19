# Vendor catalog

One file per vendor, named by the vendor's ID: its main domain
(`anthropic.com.toml`). `build.rs` compiles every file into the binary.

```toml
name = "Anthropic"                       # the name to show
aliases = ["Anthropic, PBC"]             # names on receipts: legal, old, trading names
domains = ["claude.com", "claude.ai"]    # other domains its site and mail use; subdomains match
category = "ai"
pricing = ["usage", "seat"]              # seat, usage, plan, one_time
```

Categories: ai, dev-tools, infrastructure, observability, security, data,
design, productivity, communication, analytics, marketing, sales, support,
finance, hr, legal, education, other.

`_processors.toml` lists payment processors and billing platforms (Stripe,
Paddle, Orb, …). Their receipts are for other vendors, so their domains
never identify a vendor.

To add a vendor: `spendyard vendors unknown` lists vendors on your receipts
that the catalog lacks. Add a file, run `cargo test catalog` (it checks
that no name or domain points at two vendors and that the category is
known), and open a pull request.

Logos aren't stored here. Anything showing vendors can fetch the icon for a
vendor's ID: `https://www.google.com/s2/favicons?domain={id}&sz={size}`.
