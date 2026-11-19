//! `spendyard login`, `logout`, `accounts` and `scan`.

use std::sync::Arc;

use anyhow::{Result, bail};

use super::config::{self, Config};
use super::mailbox::{self, ScanOptions, accounts};
use super::models::Models;
use super::pretty;

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

pub async fn login(
    provider: &str,
    client_id: Option<String>,
    tenant: Option<String>,
) -> Result<()> {
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

        "gmail" | "imap" => bail!("{provider} isn't supported yet; Outlook is"),

        other => bail!("unknown mailbox {other:?}; use outlook"),
    }
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
        println!("no accounts; `spendyard login outlook` signs in");

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
}

/// Returns how many receipts or invoices were found and how many errors
/// there were.
pub async fn scan(command: ScanCommand) -> Result<(usize, usize)> {
    if command.provider != "outlook" {
        bail!("{} isn't supported yet; Outlook is", command.provider);
    }

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

    let report = mailbox::scan_outlook(command.options, models).await?;

    eprintln!(
        "{} · {} · since {}: {} messages, {} likely about money",
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
