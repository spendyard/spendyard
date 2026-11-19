//! Signed-in mailboxes. What isn't secret (provider, address, app) lives in
//! `~/.spendyard/accounts.json`; refresh tokens live in the OS keychain,
//! or, where there is none, in `~/.spendyard/tokens.json`, readable only by
//! you.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cli::config;

/// The keychain service name every token is stored under.
const KEYCHAIN_SERVICE: &str = "spendyard";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// `outlook` or `imap`.
    pub provider: String,

    /// The mailbox address.
    pub address: String,

    /// The OAuth app signed in with (Outlook); empty for IMAP.
    #[serde(default)]
    pub client_id: String,

    /// Microsoft tenant, for Outlook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,

    /// `host:port`, for IMAP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,

    /// Where the refresh token is: `keychain` or `file`.
    pub token_store: String,
}

impl Account {
    /// `outlook:you@example.com`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.provider, self.address)
    }
}

fn accounts_path() -> PathBuf {
    config::home().join("accounts.json")
}

fn tokens_path() -> PathBuf {
    config::home().join("tokens.json")
}

pub fn load() -> Result<Vec<Account>> {
    let path = accounts_path();

    if !path.exists() {
        return Ok(Vec::new());
    }

    let text = std::fs::read_to_string(&path)?;

    serde_json::from_str(&text).with_context(|| format!("in {}", path.display()))
}

fn save(accounts: &[Account]) -> Result<()> {
    std::fs::create_dir_all(config::home())?;

    std::fs::write(
        accounts_path(),
        serde_json::to_string_pretty(accounts)? + "\n",
    )?;

    Ok(())
}

/// Adds or replaces an account and stores its refresh token.
pub fn add(mut account: Account, refresh_token: &str) -> Result<Account> {
    account.token_store = store_token(&account.key(), refresh_token)?;

    let mut accounts = load()?;

    accounts.retain(|existing| existing.key() != account.key());
    accounts.push(account.clone());

    save(&accounts)?;

    Ok(account)
}

/// Removes an account and its token.
pub fn remove(key: &str) -> Result<bool> {
    let mut accounts = load()?;

    let before = accounts.len();

    accounts.retain(|account| account.key() != key);

    if accounts.len() == before {
        return Ok(false);
    }

    save(&accounts)?;

    if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, key) {
        let _ = entry.delete_credential();
    }

    let mut file_tokens = read_file_tokens()?;

    if file_tokens.remove(key).is_some() {
        write_file_tokens(&file_tokens)?;
    }

    Ok(true)
}

/// The one account for `provider`, or the one named by `address`.
pub fn find(provider: &str, address: Option<&str>) -> Result<Account> {
    let accounts: Vec<Account> = load()?
        .into_iter()
        .filter(|account| account.provider == provider)
        .filter(|account| {
            address.is_none_or(|address| account.address.eq_ignore_ascii_case(address))
        })
        .collect();

    match accounts.as_slice() {
        [account] => Ok(account.clone()),
        [] => bail!("no {provider} account signed in; run `spendyard login {provider}`"),
        _ => bail!("several {provider} accounts are signed in; pick one with --account"),
    }
}

pub fn token(account: &Account) -> Result<String> {
    if account.token_store == "keychain" {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &account.key())?;

        return entry
            .get_password()
            .with_context(|| format!("reading the {} token from the keychain", account.key()));
    }

    read_file_tokens()?
        .remove(&account.key())
        .with_context(|| format!("no token for {}; sign in again", account.key()))
}

/// Stores a (new) refresh token. Microsoft rotates them on every use.
pub fn update_token(account: &Account, refresh_token: &str) -> Result<()> {
    if account.token_store == "keychain" {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &account.key())?;

        entry.set_password(refresh_token)?;

        return Ok(());
    }

    let mut tokens = read_file_tokens()?;

    tokens.insert(account.key(), refresh_token.to_string());

    write_file_tokens(&tokens)
}

/// Stores a token in the keychain, or the file when the keychain fails.
/// `SPENDYARD_TOKEN_STORE=file` forces the file.
fn store_token(key: &str, token: &str) -> Result<String> {
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
