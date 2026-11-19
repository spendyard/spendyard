//! The Spendyard pipeline: read a document, decide what it is, extract the
//! details and validate them.
//!
//! This crate holds everything that works on a single document. It must
//! compile to `wasm32-unknown-unknown` so the same code can run in a
//! Cloudflare Worker: no filesystem, threads or blocking I/O here. Those
//! belong in `spendyard-cli`.

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
