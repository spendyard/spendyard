//! `spendyard mailbox login`, `logout`, `list` and `scan`.

use std::sync::Arc;

use anyhow::{Result, bail};

use super::config::{self, Config};
use super::mailbox::{self, ScanOptions, accounts};
use super::models::Models;
use super::pretty;
use super::store::Store;

/// The Outlook app and tenant to sign in with: flags first, then
/// `[mailboxes.outlook]` in the config.
fn outlook_app(
    config: &Config,
    client_id: Option<String>,
    tenant: Option<String>,
) -> Result<(String, String)> {
    let settings = config.mailboxes.get("outlook");

    let client_id = client_id.or_else(|| settings.and_then(|settings| settings.client_id.clone()));

    let Some(client_id) = client_id else {
        bail!(
            "Outlook needs an app (client) ID: --client-id, or client_id under [mailboxes.outlook] in the config. \
             Register one in Microsoft Entra with delegated Mail.Read and public client flows allowed."
        );
    };

    let tenant = tenant
        .or_else(|| settings.and_then(|settings| settings.tenant.clone()))
        .unwrap_or_else(|| "organizations".to_string());

    Ok((client_id, tenant))
}

pub struct LoginOptions {
    pub provider: String,

    /// The mailbox address, for IMAP.
    pub address: Option<String>,

    pub client_id: Option<String>,
    pub tenant: Option<String>,

    /// `host:port`, for IMAP; guessed from the address otherwise.
    pub server: Option<String>,

    /// For IMAP, when it isn't the address.
    pub username: Option<String>,
}

pub async fn login(options: LoginOptions) -> Result<()> {
    let LoginOptions {
        provider,
        address,
        client_id,
        tenant,
        server,
        username,
    } = options;

    let provider = provider.as_str();

    let config = config::load()?;

    match provider {
        "outlook" => {
            let (client_id, tenant) = outlook_app(&config, client_id, tenant)?;

            let account = mailbox::outlook::login(&client_id, &tenant).await?;

            println!(
                "signed in as {} (token in the {})",
                account.address, account.token_store
            );

            Ok(())
        }

        "imap" => {
            let Some(address) = address else {
                bail!("which mailbox? `spendyard mailbox login imap you@example.com`");
            };

            login_imap(&address.to_lowercase(), server, username).await
        }

        "gmail" => bail!(
            "Gmail goes through IMAP with an app password: `spendyard mailbox login imap you@gmail.com`. Create the app password at myaccount.google.com/apppasswords (2-Step Verification must be on)"
        ),

        other => bail!("unknown mailbox {other:?}; use outlook or imap"),
    }
}

/// Signs in to an IMAP mailbox: asks for the (app) password, checks it by
/// signing in, and keeps it in the keychain.
async fn login_imap(address: &str, server: Option<String>, username: Option<String>) -> Result<()> {
    let server = match server {
        Some(server) => server,
        None => mailbox::imap::server_for(address)?,
    };

    let username = username.unwrap_or_else(|| address.to_string());

    let password = read_password(&format!(
        "Password (or app password) for {username} at {server}: "
    ))?;

    let session = mailbox::imap::connect(&server, &username, &password).await?;

    session.logout().await;

    let account = accounts::Account {
        provider: "imap".to_string(),
        address: address.to_string(),
        client_id: String::new(),
        tenant: None,
        server: Some(server.clone()),
        token_store: String::new(),
    };

    let account = accounts::add(account, &password)?;

    println!(
        "signed in to {} at {server} (password in the {})",
        account.address, account.token_store
    );

    Ok(())
}

/// A password from the terminal without echoing it, or from standard input
/// when it's piped (`echo $APP_PASSWORD | spendyard mailbox login imap …`).
fn read_password(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;

    let password = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(prompt)?
    } else {
        let mut line = String::new();

        std::io::stdin().read_line(&mut line)?;

        line
    };

    let password = password.trim().to_string();

    if password.is_empty() {
        bail!("no password given");
    }

    Ok(password)
}

pub fn logout(account: &str) -> Result<()> {
    let key = if account.contains(':') {
        account.to_string()
    } else {
        format!("outlook:{account}")
    };

    if accounts::remove(&key)? {
        println!("signed out of {key}");
    } else {
        println!("no account {key}");
    }

    Ok(())
}

pub fn list_accounts() -> Result<()> {
    let accounts = accounts::load()?;

    if accounts.is_empty() {
        println!("no accounts; `spendyard mailbox login outlook` signs in");

        return Ok(());
    }

    for account in accounts {
        println!(
            "{:<8} {:<40} token in {}",
            account.provider, account.address, account.token_store
        );
    }

    Ok(())
}

pub struct ScanCommand {
    pub provider: String,
    pub options: ScanOptions,
    pub extractor: Option<String>,
    pub decider: Option<String>,
    pub pretty: bool,

    /// Don't use the store: no position, no skipping, nothing saved.
    pub no_store: bool,
}

/// Returns how many receipts or invoices were found and how many errors
/// there were.
pub async fn scan(command: ScanCommand) -> Result<(usize, usize)> {
    let config = config::load()?;

    let models = if command.options.dry_run {
        None
    } else {
        Some(Arc::new(Models::load(
            &config,
            command.extractor.as_deref(),
            command.decider.as_deref(),
        )?))
    };

    let dry_run = command.options.dry_run;

    let mut store = if command.no_store {
        None
    } else {
        Some(Store::open()?)
    };

    let report = mailbox::scan(&command.provider, command.options, models, store.as_mut()).await?;

    let stored_note = if report.already_stored > 0 {
        format!(" ({} more already in the store)", report.already_stored)
    } else {
        String::new()
    };

    eprintln!(
        "{} · {} · since {}: {} messages, {} likely about money to read{stored_note}",
        report.account,
        report.folder,
        report.since,
        report.listed,
        report.candidates.len()
    );

    if dry_run {
        for header in &report.candidates {
            let date = header.received.get(..10).unwrap_or("");

            let attachment = if header.has_attachments { "📎" } else { "  " };

            println!(
                "{date}  {attachment} {:<36} {}",
                truncate(&header.from, 36),
                header.subject
            );
        }

        return Ok((0, 0));
    }

    let mut found = 0;
    let mut errors = 0;

    for scanned in &report.scanned {
        let name = format!(
            "{} {} — {}",
            scanned.header.received.get(..10).unwrap_or(""),
            scanned.header.from,
            scanned.header.subject
        );

        match &scanned.result {
            Ok(result) => {
                if result.document.is_receipt || result.document.is_invoice {
                    found += 1;
                }

                if command.pretty {
                    pretty::print_result(&name, result);
                } else {
                    let line = mailbox::result_line(&report, scanned, result);

                    println!("{}", serde_json::to_string(&line)?);
                }
            }

            Err(error) => {
                errors += 1;

                eprintln!("spendyard: {name}: {error:#}");
            }
        }
    }

    eprintln!(
        "{found} receipts or invoices in {} messages read",
        report.scanned.len()
    );

    Ok((found, errors))
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}
