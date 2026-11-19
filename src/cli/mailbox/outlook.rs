//! Outlook and Microsoft 365 through Microsoft Graph: device-code sign-in,
//! listing messages, and downloading them as MIME.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use super::Header;
use super::accounts::{self, Account};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";

/// Read-only mail, and a refresh token so you sign in once.
const SCOPES: &str = "offline_access User.Read Mail.Read";

/// Fields fetched per message when listing: enough to decide whether to
/// download it.
const LIST_FIELDS: &str =
    "id,internetMessageId,subject,from,receivedDateTime,bodyPreview,hasAttachments";

fn authority(tenant: &str) -> String {
    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0")
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    message: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
}

/// Signs in with a device code: prints a URL and a code to enter there,
/// and waits until it has been.
pub async fn login(client_id: &str, tenant: &str) -> Result<Account> {
    let client = reqwest::Client::new();

    let device: DeviceCode = client
        .post(format!("{}/devicecode", authority(tenant)))
        .form(&[("client_id", client_id), ("scope", SCOPES)])
        .send()
        .await?
        .error_for_status()
        .context("asking Microsoft for a sign-in code")?
        .json()
        .await?;

    eprintln!("{}", device.message);

    let tokens = poll_for_tokens(&client, client_id, tenant, &device).await?;

    let refresh_token = tokens
        .refresh_token
        .context("Microsoft didn't return a refresh token")?;

    let me: Value = graph_get(
        &client,
        &tokens.access_token,
        "/me?$select=mail,userPrincipalName",
    )
    .await?;

    let address = me["mail"]
        .as_str()
        .or_else(|| me["userPrincipalName"].as_str())
        .context("no address for the signed-in account")?
        .to_lowercase();

    let account = Account {
        provider: "outlook".to_string(),
        address,
        client_id: client_id.to_string(),
        tenant: Some(tenant.to_string()),
        token_store: String::new(),
    };

    accounts::add(account, &refresh_token)
}

async fn poll_for_tokens(
    client: &reqwest::Client,
    client_id: &str,
    tenant: &str,
    device: &DeviceCode,
) -> Result<Tokens> {
    let mut interval = Duration::from_secs(device.interval.max(1));

    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(interval).await;

        if std::time::Instant::now() > deadline {
            bail!("the sign-in code expired; run login again");
        }

        let response = client
            .post(format!("{}/token", authority(tenant)))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device.device_code.as_str()),
            ])
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(response.json().await?);
        }

        let body: Value = response.json().await.unwrap_or_default();

        match body["error"].as_str() {
            Some("authorization_pending") => continue,

            Some("slow_down") => interval += Duration::from_secs(5),

            _ => bail!(
                "sign-in failed: {}",
                body["error_description"]
                    .as_str()
                    .unwrap_or("unknown error")
            ),
        }
    }
}

/// A Graph session for one account, with a fresh access token.
pub struct Session {
    client: reqwest::Client,
    access_token: String,
}

impl Session {
    /// Trades the stored refresh token for an access token, and stores the
    /// new refresh token Microsoft hands back.
    pub async fn open(account: &Account) -> Result<Session> {
        let client = reqwest::Client::new();

        let refresh_token = accounts::token(account)?;

        let tenant = account.tenant.as_deref().unwrap_or("organizations");

        let response = client
            .post(format!("{}/token", authority(tenant)))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", account.client_id.as_str()),
                ("refresh_token", refresh_token.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let body: Value = response.json().await.unwrap_or_default();

            bail!(
                "{} needs to sign in again ({}); run `spendyard login outlook`",
                account.address,
                body["error_description"]
                    .as_str()
                    .unwrap_or("refresh failed")
            );
        }

        let tokens: Tokens = response.json().await?;

        if let Some(rotated) = &tokens.refresh_token {
            accounts::update_token(account, rotated)?;
        }

        Ok(Session {
            client,
            access_token: tokens.access_token,
        })
    }

    /// The folder's ID: `inbox` and other well-known names as they are,
    /// otherwise found by name at the top level or one level down.
    pub async fn folder_id(&self, name: &str) -> Result<String> {
        let well_known = ["inbox", "archive", "sentitems", "deleteditems", "junkemail"];

        if well_known.contains(&name.to_lowercase().as_str()) {
            return Ok(name.to_lowercase());
        }

        let top: Value = self
            .get("/me/mailFolders?$top=200&$select=id,displayName,childFolderCount")
            .await?;

        let folders = top["value"].as_array().cloned().unwrap_or_default();

        let matches = |folder: &Value| {
            folder["displayName"]
                .as_str()
                .is_some_and(|display| display.eq_ignore_ascii_case(name))
        };

        if let Some(folder) = folders.iter().find(|folder| matches(folder)) {
            return Ok(folder["id"].as_str().unwrap_or_default().to_string());
        }

        for parent in folders
            .iter()
            .filter(|folder| folder["childFolderCount"].as_u64().unwrap_or(0) > 0)
        {
            let id = parent["id"].as_str().unwrap_or_default();

            let children: Value = self
                .get(&format!(
                    "/me/mailFolders/{id}/childFolders?$top=200&$select=id,displayName"
                ))
                .await?;

            let found = children["value"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|folder| matches(folder));

            if let Some(folder) = found {
                return Ok(folder["id"].as_str().unwrap_or_default().to_string());
            }
        }

        bail!("no folder named {name:?}")
    }

    /// Message headers received at or after `since` (RFC 3339), newest
    /// first, at most `limit`.
    pub async fn list(
        &self,
        folder: Option<&str>,
        since: &str,
        limit: usize,
    ) -> Result<Vec<Header>> {
        let base = match folder {
            Some(folder) => format!("/me/mailFolders/{}/messages", self.folder_id(folder).await?),
            None => "/me/messages".to_string(),
        };

        let mut next = Some(format!(
            "{base}?$top=100&$select={LIST_FIELDS}&$orderby=receivedDateTime desc&$filter=receivedDateTime ge {since}"
        ));

        let mut headers = Vec::new();

        while let Some(path) = next.take() {
            let page: Value = self.get(&path).await?;

            for message in page["value"].as_array().into_iter().flatten() {
                headers.push(Header {
                    id: message["id"].as_str().unwrap_or_default().to_string(),
                    message_id: message["internetMessageId"].as_str().map(str::to_string),
                    subject: message["subject"].as_str().unwrap_or_default().to_string(),
                    from: message
                        .pointer("/from/emailAddress/address")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_lowercase(),
                    received: message["receivedDateTime"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    preview: message["bodyPreview"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    has_attachments: message["hasAttachments"].as_bool().unwrap_or(false),
                });

                if headers.len() >= limit {
                    return Ok(headers);
                }
            }

            next = page["@odata.nextLink"].as_str().map(str::to_string);
        }

        Ok(headers)
    }

    /// The whole message as MIME (`.eml`).
    pub async fn mime(&self, id: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(format!("{GRAPH}/me/messages/{id}/$value"))
            .bearer_auth(&self.access_token)
            .send()
            .await?
            .error_for_status()?;

        Ok(response.bytes().await?.to_vec())
    }

    async fn get(&self, path: &str) -> Result<Value> {
        graph_get(&self.client, &self.access_token, path).await
    }
}

/// GET from Graph, retrying when throttled.
async fn graph_get(client: &reqwest::Client, token: &str, path: &str) -> Result<Value> {
    let url = if path.starts_with("http") {
        path.to_string()
    } else {
        format!("{GRAPH}{path}")
    };

    for attempt in 1.. {
        let response = client.get(&url).bearer_auth(token).send().await?;

        let status = response.status();

        if status.as_u16() == 429 || status.is_server_error() {
            if attempt >= 5 {
                bail!("Graph kept answering {status}");
            }

            let wait = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(attempt * 2);

            tokio::time::sleep(Duration::from_secs(wait.min(60))).await;

            continue;
        }

        if !status.is_success() {
            let body: Value = response.json().await.unwrap_or_default();

            bail!(
                "Graph answered {status}: {}",
                body.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("no details")
            );
        }

        return Ok(response.json().await?);
    }

    unreachable!()
}
