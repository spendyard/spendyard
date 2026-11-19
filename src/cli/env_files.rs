//! Loading `.env` files on startup, so API keys can live in a file instead
//! of the shell profile.
//!
//! In order, first wins: variables already set in the environment, then
//! `.env.local` and `.env` in the current folder, then `~/.spendyard/.env`.

use std::path::PathBuf;

use super::config;

/// The files to look in, most important first.
fn candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(".env.local"),
        PathBuf::from(".env"),
        config::home().join(".env"),
    ]
}

/// Loads every file that exists, without overriding anything already set,
/// and returns the ones loaded. Must run before any other thread starts:
/// it sets environment variables.
pub fn load() -> Vec<PathBuf> {
    let mut loaded = Vec::new();

    for path in candidates() {
        if !path.is_file() {
            continue;
        }

        match dotenvy::from_path(&path) {
            Ok(()) => loaded.push(path),
            Err(error) => eprintln!("spendyard: ignoring {}: {error}", path.display()),
        }
    }

    loaded
}
