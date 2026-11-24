//! Secrets the CLI keeps: mailboxes' refresh tokens and the cloud's token.
//! Each in the OS keychain, under the service `spendyard`, or, where there is
//! none, in `~/.spendyard/tokens.json`, readable only by you.
//! `SPENDYARD_TOKEN_STORE=file` forces the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cli::config;

/// The keychain service name every secret is stored under.
const KEYCHAIN_SERVICE: &str = "spendyard";

fn tokens_path() -> PathBuf {
    config::home().join("tokens.json")
}

/// Stores a secret in the keychain, or the file when the keychain fails, and
/// says which: `keychain` or `file`.
pub fn store(key: &str, token: &str) -> Result<String> {
    let force_file = std::env::var("SPENDYARD_TOKEN_STORE").is_ok_and(|store| store == "file");

    if !force_file {
        let stored =
            keyring::Entry::new(KEYCHAIN_SERVICE, key).and_then(|entry| entry.set_password(token));

        match stored {
            Ok(()) => return Ok("keychain".to_string()),

            Err(error) => eprintln!(
                "spendyard: the keychain didn't work ({error}); keeping the token in {} instead",
                tokens_path().display()
            ),
        }
    }

    let mut tokens = read_file_tokens()?;

    tokens.insert(key.to_string(), token.to_string());

    write_file_tokens(&tokens)?;

    Ok("file".to_string())
}

/// A secret, from where `store` said it put it.
pub fn read(key: &str, store: &str) -> Result<String> {
    if store == "keychain" {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, key)?;

        return entry
            .get_password()
            .with_context(|| format!("reading the {key} token from the keychain"));
    }

    read_file_tokens()?
        .remove(key)
        .with_context(|| format!("no token for {key}; sign in again"))
}

/// Replaces a secret where it is.
pub fn update(key: &str, store: &str, token: &str) -> Result<()> {
    if store == "keychain" {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, key)?;

        entry.set_password(token)?;

        return Ok(());
    }

    let mut tokens = read_file_tokens()?;

    tokens.insert(key.to_string(), token.to_string());

    write_file_tokens(&tokens)
}

/// Forgets a secret, wherever it is.
pub fn delete(key: &str) -> Result<()> {
    if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, key) {
        let _ = entry.delete_credential();
    }

    let mut file_tokens = read_file_tokens()?;

    if file_tokens.remove(key).is_some() {
        write_file_tokens(&file_tokens)?;
    }

    Ok(())
}

fn read_file_tokens() -> Result<BTreeMap<String, String>> {
    let path = tokens_path();

    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    Ok(serde_json::from_str(&std::fs::read_to_string(&path)?)?)
}

fn write_file_tokens(tokens: &BTreeMap<String, String>) -> Result<()> {
    std::fs::create_dir_all(config::home())?;

    let path = tokens_path();

    std::fs::write(&path, serde_json::to_string_pretty(tokens)?)?;

    restrict(&path)?;

    Ok(())
}

/// Readable and writable by the owner only.
#[cfg(unix)]
fn restrict(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
