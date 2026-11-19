//! IMAP: any mailbox that takes a password or an app password. Gmail,
//! iCloud, Fastmail, Yahoo, self-hosted servers. (Outlook and Microsoft 365
//! refuse passwords for IMAP; they go through Microsoft Graph.)

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result, bail};
use async_imap::types::NameAttribute;
use futures_util::TryStreamExt;
use mail_parser::MessageParser;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::ClientConfig;
use tokio_rustls::rustls::pki_types::ServerName;

use super::Header;

/// The IMAP server for an address's domain, when it's a known provider.
/// Others default to `imap.<domain>:993`.
pub fn server_for(address: &str) -> Result<String> {
    let domain = address
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .to_lowercase();

    let known = match domain.as_str() {
        "gmail.com" | "googlemail.com" => "imap.gmail.com:993",
        "icloud.com" | "me.com" | "mac.com" => "imap.mail.me.com:993",
        "yahoo.com" | "ymail.com" => "imap.mail.yahoo.com:993",
        "fastmail.com" | "fastmail.fm" => "imap.fastmail.com:993",
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => {
            bail!(
                "Outlook and Microsoft 365 don't accept passwords over IMAP; use `spendyard login outlook`"
            )
        }
        "" => bail!("{address:?} isn't an email address"),
        _ => "",
    };

    if known.is_empty() {
        return Ok(format!("imap.{domain}:993"));
    }

    Ok(known.to_string())
}

/// Something to talk IMAP over: TLS, or plain TCP to this machine.
trait Transport: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> Transport for T {}

/// The connection, TLS or not, behind one type.
#[derive(Debug)]
struct Stream(Box<dyn Transport>);

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(context, buffer)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(context, data)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(context)
    }
}

/// Plain IMAP is allowed only to this machine on a port other than 993:
/// local bridges such as Proton Bridge, and test servers. Everything else
/// is TLS.
fn plain_allowed(host: &str, port: u16) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1") && port != 993
}

pub struct ImapSession {
    session: async_imap::Session<Stream>,

    /// The server speaks Gmail's extensions (`X-GM-RAW` search).
    gmail: bool,
}

/// Connects over TLS (port 993), checking the certificate against the
/// system's trust store, and signs in.
pub async fn connect(server: &str, username: &str, password: &str) -> Result<ImapSession> {
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>().context("the port isn't a number")?,
        ),
        None => (server.to_string(), 993),
    };

    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("connecting to {host}:{port}"))?;

    let stream = if plain_allowed(&host, port) {
        Stream(Box::new(tcp))
    } else {
        let config =
            <ClientConfig as rustls_platform_verifier::ConfigVerifierExt>::with_platform_verifier()
                .context("loading the system's certificates")?;

        let connector = TlsConnector::from(Arc::new(config));

        let name = ServerName::try_from(host.clone()).context("the server name isn't valid")?;

        let tls: TlsStream<TcpStream> = connector
            .connect(name, tcp)
            .await
            .with_context(|| format!("TLS with {host}"))?;

        Stream(Box::new(tls))
    };

    let mut client = async_imap::Client::new(stream);

    client
        .read_response()
        .await
        .context("reading the server's greeting")?
        .context("the server closed the connection before greeting")?;

    let mut session = client.login(username, password).await.map_err(|(error, _)| {
        anyhow::anyhow!(
            "signing in to {host} as {username} failed: {error}. Gmail, iCloud, Yahoo and Fastmail need an app password, not your normal one"
        )
    })?;

    let capabilities = session.capabilities().await?;

    let gmail = capabilities.has_str("X-GM-EXT-1");

    Ok(ImapSession { session, gmail })
}

impl ImapSession {
    /// The folder to scan: the one asked for, else Gmail's "All Mail"
    /// (found by its special-use flag, since its name is translated), else
    /// INBOX.
    pub async fn default_folder(&mut self) -> Result<String> {
        if !self.gmail {
            return Ok("INBOX".to_string());
        }

        let names: Vec<_> = self
            .session
            .list(Some(""), Some("*"))
            .await?
            .try_collect()
            .await?;

        let all_mail = names.iter().find(|name| {
            name.attributes()
                .iter()
                .any(|attribute| matches!(attribute, NameAttribute::All))
        });

        Ok(all_mail
            .map(|name| name.name().to_string())
            .unwrap_or_else(|| "INBOX".to_string()))
    }

    /// Messages newest first, at most `limit`: those after `cursor` (a
    /// `uidvalidity:uid` from the last scan) when it's still valid, else
    /// those since `since` (`YYYY-MM-DD`). Returns the headers, the cursor
    /// to save, and whether the listing was cut short by `limit`.
    pub async fn list(
        &mut self,
        folder: &str,
        cursor: Option<&str>,
        since: &str,
        limit: usize,
    ) -> Result<(Vec<Header>, Option<String>, bool)> {
        let mailbox = self
            .session
            .examine(folder)
            .await
            .with_context(|| format!("opening the folder {folder:?}"))?;

        let validity = mailbox.uid_validity.unwrap_or(0);

        let last_uid = cursor
            .and_then(|cursor| cursor.split_once(':'))
            .filter(|(cursor_validity, _)| cursor_validity.parse::<u32>().ok() == Some(validity))
            .and_then(|(_, uid)| uid.parse::<u32>().ok());

        let query = match last_uid {
            Some(uid) => format!("UID {}:*", uid + 1),
            None => format!("SINCE {}", imap_date(since)?),
        };

        let found = self.session.uid_search(&query).await?;

        // "UID n:*" always includes the newest message, even when it's older
        // than n.
        let mut uids: Vec<u32> = found
            .into_iter()
            .filter(|uid| last_uid.is_none_or(|last| *uid > last))
            .collect();

        uids.sort_unstable_by(|a, b| b.cmp(a));

        let cut_short = uids.len() > limit;

        uids.truncate(limit);

        let newest = uids.first().copied().or(last_uid);

        let cursor_after = newest.map(|uid| format!("{validity}:{uid}"));

        let mut headers = Vec::with_capacity(uids.len());

        for chunk in uids.chunks(200) {
            let set = chunk
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");

            let fetches: Vec<_> = self
                .session
                .uid_fetch(&set, "(UID INTERNALDATE BODY.PEEK[HEADER])")
                .await?
                .try_collect()
                .await?;

            for fetch in fetches {
                let (Some(uid), Some(raw)) = (fetch.uid, fetch.header()) else {
                    continue;
                };

                let received = fetch.internal_date().map(|date| date.to_rfc3339());

                headers.push(header_from(uid, raw, received));
            }
        }

        headers.sort_by(|a, b| b.received.cmp(&a.received));

        Ok((headers, cursor_after, cut_short))
    }

    /// The whole message as MIME, without marking it read.
    pub async fn mime(&mut self, uid: &str) -> Result<Vec<u8>> {
        let fetches: Vec<_> = self
            .session
            .uid_fetch(uid, "BODY.PEEK[]")
            .await?
            .try_collect()
            .await?;

        fetches
            .first()
            .and_then(|fetch| fetch.body())
            .map(<[u8]>::to_vec)
            .with_context(|| format!("message {uid} came back empty"))
    }

    pub async fn logout(mut self) {
        let _ = self.session.logout().await;
    }
}

/// A [`Header`] from a message's raw header block.
fn header_from(uid: u32, raw: &[u8], received: Option<String>) -> Header {
    let parsed = MessageParser::default().parse_headers(raw);

    let message = parsed.as_ref().map(|message| message.root());

    let from = message
        .and_then(|message| message.from())
        .and_then(|from| from.first())
        .and_then(|mailbox| mailbox.address().map(str::to_lowercase))
        .unwrap_or_default();

    let content_type = String::from_utf8_lossy(raw).to_lowercase();

    let has_attachments = content_type.contains("multipart/mixed")
        || content_type.contains("application/pdf")
        || content_type.contains("multipart/related");

    Header {
        id: uid.to_string(),
        message_id: message
            .and_then(|message| message.message_id())
            .map(str::to_string),
        subject: message
            .and_then(|message| message.subject())
            .unwrap_or_default()
            .to_string(),
        from,
        received: received
            .or_else(|| {
                message
                    .and_then(|message| message.date())
                    .map(|date| date.to_rfc3339())
            })
            .unwrap_or_default(),
        preview: String::new(),
        has_attachments,
    }
}

/// `2026-01-05` → `5-Jan-2026`, the date form IMAP's SEARCH takes.
fn imap_date(date: &str) -> Result<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let day_part = date.get(..10).unwrap_or(date);

    let parts: Vec<u32> = day_part
        .split('-')
        .filter_map(|part| part.parse().ok())
        .collect();

    let [year, month, day] = parts[..] else {
        bail!("{date:?} isn't a YYYY-MM-DD date");
    };

    let month_name = MONTHS.get(month as usize - 1).context("no such month")?;

    Ok(format!("{day}-{month_name}-{year}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_for_known_providers() {
        assert_eq!(server_for("you@gmail.com").unwrap(), "imap.gmail.com:993");
        assert_eq!(
            server_for("you@icloud.com").unwrap(),
            "imap.mail.me.com:993"
        );
        assert_eq!(server_for("you@example.no").unwrap(), "imap.example.no:993");
        assert!(server_for("you@outlook.com").is_err());
    }

    #[test]
    fn plain_imap_only_to_this_machine() {
        assert!(plain_allowed("127.0.0.1", 1143));
        assert!(plain_allowed("localhost", 3143));
        assert!(!plain_allowed("127.0.0.1", 993));
        assert!(!plain_allowed("imap.gmail.com", 143));
    }

    #[test]
    fn imap_dates() {
        assert_eq!(imap_date("2026-01-05").unwrap(), "5-Jan-2026");
        assert_eq!(imap_date("2026-11-19T00:00:00Z").unwrap(), "19-Nov-2026");
    }

    #[test]
    fn headers_from_raw_header_blocks() {
        let raw = b"From: Acme Billing <Billing@Acme.example>\r\nSubject: =?utf-8?q?Your_receipt?=\r\nMessage-ID: <abc@acme.example>\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n";

        let header = header_from(42, raw, Some("2026-11-19T09:00:00+00:00".to_string()));

        assert_eq!(header.id, "42");
        assert_eq!(header.from, "billing@acme.example");
        assert_eq!(header.subject, "Your receipt");
        assert_eq!(header.message_id.as_deref(), Some("abc@acme.example"));
        assert!(header.has_attachments);
    }
}
