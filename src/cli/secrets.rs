//! Secrets the CLI keeps: mailboxes' refresh tokens and app passwords, and
//! the cloud's token. In `~/.spendyard/tokens.json`, readable only by you, as
//! `gh` and the cloud CLIs keep theirs. `SPENDYARD_TOKEN_STORE=keychain`
//! keeps them in the OS keychain instead; on macOS it asks again for each new
//! spendyard, which is why it isn't the default.
//!
//! A token an older spendyard kept in the keychain moves to the file the first
//! time it's read, unless the keychain is asked for.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cli::config;

/// The keychain service name every secret is stored under.
const KEYCHAIN_SERVICE: &str = "spendyard";

fn tokens_path() -> PathBuf {
    config::home().join("tokens.json")
}

/// Whether `SPENDYARD_TOKEN_STORE=keychain` asks for the keychain.
fn keychain_wanted() -> bool {
    std::env::var("SPENDYARD_TOKEN_STORE").is_ok_and(|store| store == "keychain")
}

/// Where secrets are kept, to say so: `the keychain`, or the file's path.
pub fn kept_in() -> String {
    if keychain_wanted() {
        return "the keychain".to_string();
    }

    tokens_path().display().to_string()
}

/// Keeps a secret, replacing one under the same key.
pub fn store(key: &str, token: &str) -> Result<()> {
    if keychain_wanted() {
        let stored =
            keyring::Entry::new(KEYCHAIN_SERVICE, key).and_then(|entry| entry.set_password(token));

        match stored {
            Ok(()) => return Ok(()),

            Err(error) => eprintln!(
                "spendyard: the keychain didn't work ({error}); keeping the token in {} instead",
                tokens_path().display()
            ),
        }
    }

    let mut tokens = read_file_tokens()?;

    tokens.insert(key.to_string(), token.to_string());

    write_file_tokens(&tokens)
}

/// A secret: from the file, or the keychain, where an older spendyard kept
/// it, moving it to the file unless the keychain is asked for.
pub fn read(key: &str) -> Result<String> {
    if let Some(token) = read_file_tokens()?.remove(key) {
        return Ok(token);
    }

    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, key)?;

    let token = entry
        .get_password()
        .with_context(|| format!("no token for {key}; sign in again"))?;

    if !keychain_wanted() {
        let mut tokens = read_file_tokens()?;

        tokens.insert(key.to_string(), token.clone());

        write_file_tokens(&tokens)?;

        let _ = entry.delete_credential();
    }

    Ok(token)
}

/// Forgets a secret, wherever it is.
pub fn delete(key: &str) -> Result<()> {
    let mut file_tokens = read_file_tokens()?;

    let was_in_file = file_tokens.remove(key).is_some();

    if was_in_file {
        write_file_tokens(&file_tokens)?;
    }

    // The keychain only when it may hold it: asking it may prompt.
    if (keychain_wanted() || !was_in_file)
        && let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, key)
    {
        let _ = entry.delete_credential();
    }

    Ok(())
}

fn read_file_tokens() -> Result<BTreeMap<String, String>> {
    read_tokens_at(&tokens_path())
}

fn read_tokens_at(path: &std::path::Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

/// Writes the tokens to a file readable by you only from the start, then puts
/// it in place.
fn write_file_tokens(tokens: &BTreeMap<String, String>) -> Result<()> {
    std::fs::create_dir_all(config::home())?;

    write_tokens_at(&tokens_path(), tokens)
}

fn write_tokens_at(path: &std::path::Path, tokens: &BTreeMap<String, String>) -> Result<()> {
    let staged = path.with_extension(format!("json.{}", std::process::id()));

    let mut file = owner_only(&staged)?;

    file.write_all(serde_json::to_string_pretty(tokens)?.as_bytes())?;
    file.sync_all()?;

    drop(file);

    std::fs::rename(&staged, path)?;

    Ok(())
}

/// A new file, readable and writable by the owner only.
#[cfg(unix)]
fn owner_only(path: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;

    Ok(file)
}

#[cfg(not(unix))]
fn owner_only(path: &std::path::Path) -> Result<std::fs::File> {
    Ok(std::fs::File::create(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_the_owners_only_and_reads_back() {
        let folder = std::env::temp_dir().join(format!("spendyard-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();

        let path = folder.join("tokens.json");
        let tokens =
            BTreeMap::from([("outlook:you@example.com".to_string(), "secret".to_string())]);

        write_tokens_at(&path, &tokens).unwrap();

        assert_eq!(read_tokens_at(&path).unwrap(), tokens);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&path).unwrap().permissions().mode();

            assert_eq!(mode & 0o777, 0o600);
        }

        assert!(
            read_tokens_at(&folder.join("none.json"))
                .unwrap()
                .is_empty()
        );

        std::fs::remove_dir_all(&folder).unwrap();
    }
}
