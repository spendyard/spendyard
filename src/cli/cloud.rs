//! `spendyard cloud …`: signing this machine in to Spendyard Cloud, seeing
//! where it acts, and moving it to another organisation.
//!
//! Signing in is a device code (the server's `cli/device.ts`): the CLI shows
//! a code and a link, the person approves it in the browser for one of their
//! organisations, and the CLI's poll gets a token for this machine. The token
//! is kept by `secrets` (the keychain); what isn't secret (the server, who,
//! which organisation) in `~/.spendyard/cloud.json`. The token acts in one
//! organisation at a time, until `cloud switch` moves it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{config, secrets};

/// Spendyard Cloud, unless `--server` or `SPENDYARD_CLOUD_URL` says otherwise.
const DEFAULT_SERVER: &str = "https://app.spendyard.com";

/// Where this machine is signed in: everything but the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedIn {
    /// The server's origin, such as `https://app.spendyard.com`.
    pub server: String,

    /// Who approved it.
    pub email: String,

    /// The organisation it acts in, as last seen.
    pub org: Org,

    /// Where the token is: `keychain` or `file`.
    pub token_store: String,
}

/// An organisation, as the server names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Org {
    pub id: String,
    pub slug: String,
    pub name: String,
}

fn signed_in_path() -> PathBuf {
    config::home().join("cloud.json")
}

/// The keychain's name for a server's token.
fn secret_key(server: &str) -> String {
    format!("cloud:{server}")
}

/// Where this machine is signed in, or None.
pub fn signed_in() -> Result<Option<SignedIn>> {
    let path = signed_in_path();

    if !path.exists() {
        return Ok(None);
    }

    let text = std::fs::read_to_string(&path)?;

    let signed_in =
        serde_json::from_str(&text).with_context(|| format!("in {}", path.display()))?;

    Ok(Some(signed_in))
}

fn save(signed_in: &SignedIn) -> Result<()> {
    std::fs::create_dir_all(config::home())?;

    let text = serde_json::to_string_pretty(signed_in)? + "\n";

    std::fs::write(signed_in_path(), text)?;

    Ok(())
}

/// The server to sign in to: the one given, then `SPENDYARD_CLOUD_URL`, then Spendyard Cloud's.
fn server_from(given: Option<String>) -> String {
    let server = given
        .or_else(|| std::env::var("SPENDYARD_CLOUD_URL").ok())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string());

    server.trim_end_matches('/').to_string()
}

/// What the CLI says about itself when it signs in: shown on the page that approves it, and in Settings → Account.
fn about_this_client() -> Value {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let device_name = gethostname::gethostname().to_string_lossy().into_owned();

    json!({
        "client_version": spendyard_core::VERSION,
        "client_platform": platform,
        "device_name": device_name,
    })
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct Granted {
    access_token: String,
    email: String,
    org: Org,
}

/// `spendyard cloud login`: a code to approve in the browser, then this machine's token. Opens the page itself
/// unless told not to.
pub async fn login(server: Option<String>, open_browser: bool) -> Result<()> {
    let server = server_from(server);
    let client = reqwest::Client::new();

    let device: DeviceCode = client
        .post(format!("{server}/cli/device/code"))
        .json(&about_this_client())
        .send()
        .await
        .with_context(|| format!("reaching {server}"))?
        .error_for_status()
        .context("asking for a sign-in code")?
        .json()
        .await?;

    eprintln!("Open {}", device.verification_uri_complete);
    eprintln!("and check that it shows {}.", device.user_code);

    if open_browser {
        open_in_browser(&device.verification_uri_complete);
    }

    let granted = poll_for_token(&client, &server, &device).await?;

    // Signed in elsewhere before: that token goes, on the server too.
    if let Some(before) = signed_in()? {
        sign_out(&before).await;
    }

    let token_store = secrets::store(&secret_key(&server), &granted.access_token)?;

    let signed_in = SignedIn {
        server,
        email: granted.email,
        org: granted.org,
        token_store,
    };

    save(&signed_in)?;

    println!(
        "Signed in as {}, acting in {} ({})",
        signed_in.email, signed_in.org.name, signed_in.org.slug
    );

    Ok(())
}

async fn poll_for_token(
    client: &reqwest::Client,
    server: &str,
    device: &DeviceCode,
) -> Result<Granted> {
    let mut interval = Duration::from_secs(device.interval.max(1));

    let deadline = Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(interval).await;

        if Instant::now() > deadline {
            bail!("the sign-in code ran out; run `spendyard cloud login` again");
        }

        let response = client
            .post(format!("{server}/cli/device/token"))
            .json(&json!({ "device_code": device.device_code }))
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

/// Opens the page in the browser, where there is one. Printing the link is enough when there isn't.
fn open_in_browser(url: &str) {
    let opener = match std::env::consts::OS {
        "macos" => Some(("open", vec![url])),
        "windows" => Some(("cmd", vec!["/C", "start", "", url])),
        "linux" | "freebsd" | "openbsd" => Some(("xdg-open", vec![url])),
        _ => None,
    };

    if let Some((program, args)) = opener {
        let _ = std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Calls the cloud's API (Connect, JSON) as this machine.
pub struct Cloud {
    client: reqwest::Client,
    signed_in: SignedIn,
    token: String,
}

impl Cloud {
    /// As this machine, signed in; an error saying how to sign in when it isn't.
    pub fn connect() -> Result<Cloud> {
        let signed_in = signed_in()?
            .context("not signed in to Spendyard Cloud; run `spendyard cloud login`")?;

        let token = secrets::read(&secret_key(&signed_in.server), &signed_in.token_store)?;

        Ok(Cloud {
            client: reqwest::Client::new(),
            signed_in,
            token,
        })
    }

    /// Where this machine is signed in, and as whom.
    pub fn signed_in_as(&self) -> &SignedIn {
        &self.signed_in
    }

    /// One method of the API, `Service/Method`, such as `OrgService/ListOrgs`.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, request: Value) -> Result<T> {
        let url = format!("{}/spendyard.api.v1.{method}", self.signed_in.server);

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .header("connect-protocol-version", "1")
            .json(&request)
            .send()
            .await
            .with_context(|| format!("reaching {}", self.signed_in.server))?;

        if response.status().is_success() {
            return Ok(response.json().await?);
        }

        let error: Value = response.json().await.unwrap_or_default();

        let code = error["code"].as_str().unwrap_or("unknown");
        let message = error["message"].as_str().unwrap_or("no reason given");

        if code == "unauthenticated" {
            bail!("this machine was signed out of Spendyard Cloud; run `spendyard cloud login`");
        }

        bail!("{method}: {message}")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListOrgs {
    #[serde(default)]
    active_org_id: String,

    #[serde(default)]
    memberships: Vec<Membership>,
}

#[derive(Deserialize)]
struct Membership {
    org: Org,

    #[serde(default)]
    role: String,
}

#[derive(Deserialize)]
struct GetAccount {
    account: Account,
}

#[derive(Deserialize)]
struct Account {
    user: User,
}

#[derive(Deserialize)]
struct User {
    email: String,
}

/// `ROLE_ADMIN` as people say it: admin.
fn role_name(role: &str) -> String {
    role.trim_start_matches("ROLE_").to_lowercase()
}

/// `spendyard cloud status`: who this machine is signed in as, and where it acts. False when it isn't signed in.
pub async fn status() -> Result<bool> {
    let Some(signed_in) = signed_in()? else {
        println!("Not signed in to Spendyard Cloud; `spendyard cloud login` signs in");

        return Ok(false);
    };

    let cloud = Cloud::connect()?;

    let account: GetAccount = cloud.call("AccountService/GetAccount", json!({})).await?;
    let orgs: ListOrgs = cloud.call("OrgService/ListOrgs", json!({})).await?;

    println!(
        "Signed in to {} as {} (token in {})",
        signed_in.server, account.account.user.email, signed_in.token_store
    );

    let active = orgs
        .memberships
        .iter()
        .find(|membership| membership.org.id == orgs.active_org_id);

    match active {
        Some(membership) => {
            println!(
                "Acting in {} ({}), as {}",
                membership.org.name,
                membership.org.slug,
                role_name(&membership.role)
            );

            remember_org(signed_in, &membership.org)?;
        }

        None => println!(
            "Acting in no organisation: it left {}; `spendyard cloud switch` lists the others",
            signed_in.org.name
        ),
    }

    Ok(true)
}

/// The organisation this machine acts in, then the others they're in.
fn print_orgs(orgs: &ListOrgs) {
    let (current, others): (Vec<&Membership>, Vec<&Membership>) = orgs
        .memberships
        .iter()
        .partition(|membership| membership.org.id == orgs.active_org_id);

    match current.first() {
        Some(membership) => println!(
            "Acting in {} ({}), as {}",
            membership.org.name,
            membership.org.slug,
            role_name(&membership.role)
        ),

        None => println!("Acting in no organisation"),
    }

    if others.is_empty() {
        println!("You're in no other organisation");

        return;
    }

    println!();
    println!("Others:");

    let widest_slug = others
        .iter()
        .map(|membership| membership.org.slug.chars().count())
        .max()
        .unwrap_or(0);

    let widest_name = others
        .iter()
        .map(|membership| membership.org.name.chars().count())
        .max()
        .unwrap_or(0);

    for membership in others {
        println!(
            "  {:<widest_slug$}  {:<widest_name$}  {}",
            membership.org.slug,
            membership.org.name,
            role_name(&membership.role)
        );
    }

    println!();
    println!("`spendyard cloud switch <slug>` acts in one of them");
}

/// Keeps the organisation's name and slug as the server last gave them.
fn remember_org(mut signed_in: SignedIn, org: &Org) -> Result<()> {
    signed_in.org = org.clone();

    save(&signed_in)
}

#[derive(Deserialize)]
struct Switched {
    membership: Membership,
}

/// `spendyard cloud switch [slug]`: this machine acts in another of their organisations. Without a slug, shows
/// the one it acts in and the others.
pub async fn switch(slug: Option<&str>) -> Result<()> {
    let cloud = Cloud::connect()?;

    let Some(slug) = slug else {
        let orgs: ListOrgs = cloud.call("OrgService/ListOrgs", json!({})).await?;

        print_orgs(&orgs);

        return Ok(());
    };

    let switched: Switched = cloud
        .call("OrgService/SwitchOrg", json!({ "slug": slug }))
        .await?;

    let org = switched.membership.org;

    println!("Acting in {} ({})", org.name, org.slug);

    remember_org(cloud.signed_in, &org)
}

/// `spendyard cloud logout`: this machine's token gone, here and on the server.
pub async fn logout() -> Result<()> {
    let Some(signed_in) = signed_in()? else {
        println!("Not signed in to Spendyard Cloud");

        return Ok(());
    };

    sign_out(&signed_in).await;

    println!("Signed out of {} ({})", signed_in.server, signed_in.email);

    Ok(())
}

/// Ends a sign-in: on the server when it can be reached, and here whether or not.
async fn sign_out(signed_in: &SignedIn) {
    let key = secret_key(&signed_in.server);

    if let Ok(token) = secrets::read(&key, &signed_in.token_store) {
        let revoked = reqwest::Client::new()
            .post(format!("{}/cli/logout", signed_in.server))
            .bearer_auth(token)
            .send()
            .await;

        if let Err(error) = revoked {
            eprintln!(
                "spendyard: couldn't reach {} ({error}); signed out here only, sign it out in Settings → Account",
                signed_in.server
            );
        }
    }

    let _ = secrets::delete(&key);
    let _ = std::fs::remove_file(signed_in_path());
}
