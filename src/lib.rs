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

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4);
        assert!(
            parts
                .iter()
                .all(|part| part.chars().all(|c| c.is_ascii_digit()))
        );
    }
}
