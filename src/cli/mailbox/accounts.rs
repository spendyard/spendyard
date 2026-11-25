//! Signed-in mailboxes. What isn't secret (provider, address, app) lives in
//! `~/.spendyard/accounts.json`; refresh tokens are kept by `secrets`, in
//! `~/.spendyard/tokens.json`, readable only by you.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cli::{config, secrets};

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
pub fn add(account: Account, refresh_token: &str) -> Result<Account> {
    secrets::store(&account.key(), refresh_token)?;

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

    secrets::delete(key)?;

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
        [] => bail!("no {provider} account signed in; run `spendyard mailbox login {provider}`"),
        _ => bail!("several {provider} accounts are signed in; pick one with --account"),
    }
}

pub fn token(account: &Account) -> Result<String> {
    secrets::read(&account.key())
}

/// Stores a (new) refresh token. Microsoft rotates them on every use.
pub fn update_token(account: &Account, refresh_token: &str) -> Result<()> {
    secrets::store(&account.key(), refresh_token)
}
