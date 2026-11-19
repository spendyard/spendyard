//! The vendor catalog: who a receipt is from, under one ID.
//!
//! Each vendor is a file in `vendors/`, named by its ID (its main domain),
//! compiled into the binary by `build.rs`:
//!
//! ```toml
//! name = "Anthropic"
//! aliases = ["Anthropic, PBC"]
//! domains = ["anthropic.com", "claude.com"]
//! category = "ai"
//! ```
//!
//! Payment processors that send receipts for other vendors are in
//! `vendors/_processors.toml`; a receipt from one of them names its vendor
//! in the text, not in the sender.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

const CATALOG: &str = include_str!(concat!(env!("OUT_DIR"), "/catalog.toml"));

/// One vendor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vendor {
    /// The vendor's ID: its main domain, the file's name.
    #[serde(rename = "file")]
    pub id: String,

    /// The name to show.
    pub name: String,

    /// Other names on receipts: legal names, old names, trading names.
    #[serde(default)]
    pub aliases: Vec<String>,

    /// Domains the vendor's site and mail come from. Subdomains match too.
    #[serde(default)]
    pub domains: Vec<String>,

    pub category: String,

    /// How it usually charges: `seat`, `usage`, `plan`, `one_time`.
    #[serde(default)]
    pub pricing: Vec<String>,
}

/// A payment processor or billing platform.
#[derive(Debug, Clone, Deserialize)]
pub struct Processor {
    pub name: String,

    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    vendor: Vec<Vendor>,

    #[serde(default)]
    processor: Vec<Processor>,
}

pub struct Catalog {
    pub vendors: Vec<Vendor>,
    pub processors: Vec<Processor>,
    by_domain: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

static LOADED: LazyLock<Catalog> = LazyLock::new(|| {
    let file: File = toml::from_str(CATALOG).expect("the compiled-in vendor catalog is valid");

    Catalog::new(file.vendor, file.processor)
});

/// The compiled-in catalog.
pub fn catalog() -> &'static Catalog {
    &LOADED
}

impl Catalog {
    fn new(vendors: Vec<Vendor>, processors: Vec<Processor>) -> Catalog {
        let mut by_domain = HashMap::new();
        let mut by_name = HashMap::new();

        for (index, vendor) in vendors.iter().enumerate() {
            by_domain.insert(vendor.id.to_lowercase(), index);

            for domain in &vendor.domains {
                by_domain.insert(domain.to_lowercase(), index);
            }

            by_name.insert(name_key(&vendor.name), index);

            for alias in &vendor.aliases {
                by_name.insert(name_key(alias), index);
            }
        }

        Catalog {
            vendors,
            processors,
            by_domain,
            by_name,
        }
    }

    /// The vendor a domain, URL or email address belongs to. Subdomains
    /// match their parent: `mail.anthropic.com` is Anthropic.
    pub fn by_domain(&self, domain_or_address: &str) -> Option<&Vendor> {
        let host = host_of(domain_or_address)?;

        let mut candidate = host.as_str();

        loop {
            if let Some(index) = self.by_domain.get(candidate) {
                return Some(&self.vendors[*index]);
            }

            let (_, parent) = candidate.split_once('.')?;

            if !parent.contains('.') {
                return None;
            }

            candidate = parent;
        }
    }

    /// The vendor a name on a receipt belongs to, by its name or an alias,
    /// ignoring legal suffixes.
    pub fn by_name(&self, name: &str) -> Option<&Vendor> {
        let key = name_key(name);

        if key.is_empty() {
            return None;
        }

        self.by_name.get(&key).map(|index| &self.vendors[*index])
    }

    /// The vendor a receipt names, by domain first, then by name.
    pub fn find(&self, domain: Option<&str>, name: Option<&str>) -> Option<&Vendor> {
        domain
            .and_then(|domain| self.by_domain(domain))
            .or_else(|| name.and_then(|name| self.by_name(name)))
    }

    /// The processor a domain or address belongs to, if any.
    pub fn processor(&self, domain_or_address: &str) -> Option<&Processor> {
        let host = host_of(domain_or_address)?;

        self.processors.iter().find(|processor| {
            processor
                .domains
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        })
    }
}

/// `Name <billing@mail.example.com>`, `https://www.example.com/x` and
/// `example.com` all become `mail.example.com` / `example.com`.
fn host_of(text: &str) -> Option<String> {
    let lower = text.trim().to_lowercase();

    let address = match (lower.rfind('<'), lower.rfind('>')) {
        (Some(open), Some(close)) if open < close => lower[open + 1..close].to_string(),
        _ => lower,
    };

    let after_at = address.rsplit('@').next().unwrap_or_default();

    let without_scheme = after_at
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let host = without_scheme
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .trim_end_matches('.');

    (host.contains('.') && !host.contains(' ')).then(|| host.to_string())
}

/// Legal and generic words that differ between receipts from the same
/// vendor: "Neon, LLC" and "Neon Inc." are one vendor.
const NAME_NOISE: &[&str] = &[
    "inc",
    "llc",
    "ltd",
    "limited",
    "pbc",
    "sas",
    "srl",
    "sro",
    "s",
    "r",
    "o",
    "a",
    "gmbh",
    "ag",
    "as",
    "asa",
    "ab",
    "oy",
    "bv",
    "nv",
    "plc",
    "pte",
    "pty",
    "corp",
    "corporation",
    "co",
    "company",
    "the",
    "technologies",
    "technology",
    "software",
    "labs",
    "studio",
    "studios",
    "services",
    "products",
    "systems",
    "holdings",
    "group",
    "international",
    "emea",
    "europe",
    "norge",
    "unlimited",
];

/// A name without legal suffixes and punctuation, using the trading name
/// after "dba" or "t/a": the key names are matched and grouped by.
pub fn name_key(name: &str) -> String {
    let lower = name.to_lowercase();

    let trading = [" dba ", " d/b/a ", " t/a ", " trading as "]
        .iter()
        .find_map(|marker| lower.split_once(marker).map(|(_, after)| after.to_string()))
        .unwrap_or(lower);

    let words: Vec<&str> = trading
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .filter(|word| !NAME_NOISE.contains(word))
        .collect();

    if words.is_empty() {
        return trading.trim().to_string();
    }

    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiled_catalog_loads_and_has_no_duplicate_ids() {
        let catalog = catalog();

        assert!(
            catalog.vendors.len() >= 150,
            "{} vendors",
            catalog.vendors.len()
        );

        let mut ids: Vec<&str> = catalog
            .vendors
            .iter()
            .map(|vendor| vendor.id.as_str())
            .collect();

        ids.sort();

        let before = ids.len();

        ids.dedup();

        assert_eq!(before, ids.len(), "duplicate vendor IDs");
    }

    #[test]
    fn every_vendor_has_a_known_category() {
        let known = [
            "ai",
            "dev-tools",
            "infrastructure",
            "observability",
            "security",
            "data",
            "design",
            "productivity",
            "communication",
            "analytics",
            "marketing",
            "sales",
            "support",
            "finance",
            "hr",
            "legal",
            "education",
            "other",
        ];

        for vendor in &catalog().vendors {
            assert!(
                known.contains(&vendor.category.as_str()),
                "{}: {}",
                vendor.id,
                vendor.category
            );
        }
    }

    #[test]
    fn no_name_or_domain_points_at_two_vendors() {
        let catalog = catalog();

        let mut names: HashMap<String, &str> = HashMap::new();
        let mut domains: HashMap<String, &str> = HashMap::new();

        for vendor in &catalog.vendors {
            for name in std::iter::once(&vendor.name).chain(&vendor.aliases) {
                if let Some(other) = names.insert(name_key(name), &vendor.id) {
                    assert_eq!(other, vendor.id, "name {name:?} is in two vendors");
                }
            }

            for domain in std::iter::once(&vendor.id).chain(&vendor.domains) {
                if let Some(other) = domains.insert(domain.clone(), &vendor.id) {
                    assert_eq!(other, vendor.id, "domain {domain} is in two vendors");
                }
            }
        }
    }

    #[test]
    fn finds_vendors_by_domain_address_and_name() {
        let catalog = catalog();

        assert_eq!(
            catalog
                .by_domain("Anthropic <invoice+statements@mail.anthropic.com>")
                .unwrap()
                .id,
            "anthropic.com"
        );
        assert_eq!(
            catalog
                .by_domain("https://www.figma.com/pricing")
                .unwrap()
                .id,
            "figma.com"
        );
        assert_eq!(
            catalog.by_name("RealtimeBoard Inc. dba Miro").unwrap().id,
            "miro.com"
        );
        assert_eq!(catalog.by_name("Neon, LLC").unwrap().id, "neon.tech");
        assert_eq!(
            catalog
                .by_name("Not Just Tickets Ltd t/a Plain")
                .unwrap()
                .id,
            "plain.com"
        );
        assert!(catalog.by_domain("example.com").is_none());
        assert!(catalog.by_domain("com").is_none());
    }

    #[test]
    fn processors_are_not_vendors() {
        let catalog = catalog();

        assert_eq!(
            catalog
                .processor("Stripe <invoice+statements@stripe.com>")
                .unwrap()
                .name,
            "Stripe"
        );
        assert!(catalog.by_domain("stripe.com").is_none());
    }

    #[test]
    fn name_keys() {
        assert_eq!(name_key("Neon, LLC"), name_key("Neon Inc."));
        assert_eq!(name_key("RealtimeBoard Inc. dba Miro"), "miro");
    }
}
