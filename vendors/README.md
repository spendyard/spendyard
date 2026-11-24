# Vendor catalog

Every vendor it knows, by category: [VENDORS.md](VENDORS.md).

One file per vendor, named by the vendor's ID: its main domain
(`anthropic.com.toml`). `build.rs` compiles every file into the binary.

```toml
name = "Anthropic"                       # the name to show
aliases = ["Anthropic, PBC"]             # names on receipts: legal, old, trading names
domains = ["claude.com", "claude.ai"]    # other domains its site and mail use; subdomains match
category = "ai"
pricing = ["usage", "seat"]              # seat, usage, plan, one_time
descriptors = ["NOT JUST TI"]            # optional: how its charges start on card statements,
                                         # when the name or domain alone doesn't find it
related = ["activecampaign.com"]         # optional: vendors whose name can be on its receipts
                                         # or charges, such as a parent company that bills for it
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
known), run `scripts/vendors-md.py` to add it to [VENDORS.md](VENDORS.md),
and open a pull request.

## Local vendors

Without waiting for a release, put files in the same format in
`~/.spendyard/vendors/` (or `$SPENDYARD_HOME/vendors/`). They are read at
startup, on top of the built-in catalog:

- a file named like a built-in vendor (`plain.com.toml`) replaces it, to
  correct a name, add an alias or a domain;
- any other file adds a vendor;
- a name or domain a local file claims points at it;
- a file that can't be read is skipped with a warning.

`spendyard vendors` says how many came from there, and `vendors list` and
`vendors lookup` mark them `local`. A local file that helps others is the
pull request: copy it into `vendors/` here.

Logos aren't stored here. Anything showing vendors can fetch the icon for a
vendor's ID: `https://www.google.com/s2/favicons?domain={id}&sz={size}`.
