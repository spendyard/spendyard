//! `spendyard vendors`: the vendor catalog.

use std::collections::BTreeMap;

use anyhow::Result;
use spendyard_core::catalog::{self, Vendor};

use super::config;
use super::store::Store;

/// Where local vendor files live: `~/.spendyard/vendors/`.
pub fn local_dir() -> std::path::PathBuf {
    config::home().join("vendors")
}

/// Reads `~/.spendyard/vendors/*.toml` and adds them to the catalog. Runs
/// once at startup, before anything looks a vendor up; problems are
/// warnings, never errors.
pub fn load_local() {
    let Ok(entries) = std::fs::read_dir(local_dir()) else {
        return;
    };

    let mut files = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        let is_toml = path
            .extension()
            .is_some_and(|extension| extension == "toml");

        let Some(id) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
        else {
            continue;
        };

        if !is_toml || id.starts_with('_') || id.starts_with('.') {
            continue;
        }

        match std::fs::read_to_string(&path) {
            Ok(text) => files.push(catalog::LocalFile { id, text }),
            Err(error) => eprintln!("spendyard: ignoring {}: {error}", path.display()),
        }
    }

    for problem in catalog::use_local(files) {
        eprintln!("spendyard: ignoring local vendor {problem}");
    }
}

pub fn summary() {
    let catalog = catalog::catalog();

    let mut by_category: BTreeMap<&str, usize> = BTreeMap::new();

    for vendor in &catalog.vendors {
        *by_category.entry(vendor.category.as_str()).or_default() += 1;
    }

    let local = catalog.vendors.iter().filter(|vendor| vendor.local).count();

    println!(
        "{} vendors and {} payment processors in the catalog",
        catalog.vendors.len(),
        catalog.processors.len()
    );

    if local > 0 {
        println!("{local} of them from {}", local_dir().display());
    }
    println!();

    for (category, count) in by_category {
        println!("  {category:<16} {count}");
    }

    println!();
    println!(
        "`spendyard vendors list --category ai` lists them; `spendyard vendors unknown` shows vendors in your store the catalog lacks."
    );
}

pub fn list(category: Option<&str>) {
    for vendor in catalog::catalog()
        .vendors
        .iter()
        .filter(|vendor| category.is_none_or(|category| vendor.category == category))
    {
        print_vendor(vendor);
    }
}

pub fn lookup(query: &str) {
    let catalog = catalog::catalog();

    if let Some(processor) = catalog.processor(query) {
        println!(
            "{query}: a payment processor ({}); its receipts name the real vendor",
            processor.name
        );

        return;
    }

    match catalog.by_domain(query).or_else(|| catalog.by_name(query)) {
        Some(vendor) => print_vendor(vendor),
        None => println!("{query}: not in the catalog"),
    }
}

fn print_vendor(vendor: &Vendor) {
    let mut details = vec![vendor.category.clone()];

    if !vendor.pricing.is_empty() {
        details.push(vendor.pricing.join("+"));
    }

    if vendor.local {
        details.push("local".to_string());
    }

    println!(
        "{:<24} {:<26} {}",
        vendor.id,
        vendor.name,
        details.join(", ")
    );

    if !vendor.aliases.is_empty() {
        println!("{:<24} also: {}", "", vendor.aliases.join("; "));
    }
}

/// Vendors on stored receipts that the catalog doesn't know, most
/// receipts first.
pub fn unknown() -> Result<()> {
    let store = Store::open()?;

    let catalog = catalog::catalog();

    let mut statement = store.connection.prepare(
        "SELECT r.vendor_name, r.vendor_id, count(*) FROM receipts r
         WHERE r.vendor_name IS NOT NULL AND r.vendor_name != ''
         GROUP BY lower(r.vendor_name), r.vendor_id",
    )?;

    let rows: Vec<(String, Option<String>, i64)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;

    let mut missing: BTreeMap<String, (String, Option<String>, i64)> = BTreeMap::new();

    for (name, id, count) in rows {
        if catalog.find(id.as_deref(), Some(&name)).is_some() {
            continue;
        }

        let entry =
            missing
                .entry(catalog::name_key(&name))
                .or_insert((name.clone(), id.clone(), 0));

        entry.2 += count;
    }

    if missing.is_empty() {
        println!("every vendor in the store is in the catalog");

        return Ok(());
    }

    let mut ordered: Vec<_> = missing.into_values().collect();

    ordered.sort_by_key(|(_, _, count)| std::cmp::Reverse(*count));

    for (name, id, count) in ordered {
        println!("{count:>5}  {:<40} {}", name, id.unwrap_or_default());
    }

    println!();
    println!(
        "Add one as {}/<domain>.toml (see vendors/README.md in the repo for the format), and send it upstream as a pull request.",
        local_dir().display()
    );

    Ok(())
}
