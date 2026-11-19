//! The Spendyard pipeline: read a document, decide what it is, extract the
//! details and validate them.
//!
//! The library holds everything that works on a single document; `main.rs`
//! is the CLI around it. Keep filesystem, keychain and terminal code out of
//! the library: it moves into its own WebAssembly-ready crate when the cloud
//! work starts.

/// The version of this crate, reported as `meta.spendyard_version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the output format, reported as `meta.schema_version`.
///
/// Change it whenever the output changes.
pub const SCHEMA_VERSION: &str = "2026-11-19";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_a_date() {
        let parts: Vec<&str> = SCHEMA_VERSION.split('-').collect();

        assert_eq!(parts.len(), 3, "{SCHEMA_VERSION} is not YYYY-MM-DD");

        let year = parts[0];

        assert_eq!(year.len(), 4, "{SCHEMA_VERSION} has no four-digit year");

        for part in &parts {
            let is_number = part.chars().all(|c| c.is_ascii_digit());

            assert!(is_number, "{part} in {SCHEMA_VERSION} is not a number");
        }
    }
}
