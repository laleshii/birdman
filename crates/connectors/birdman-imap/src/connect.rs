use std::sync::Arc;

use async_imap::Session;
use async_native_tls::TlsStream;
use tokio::net::TcpStream;

use birdman_auth::{AuthAdapter, AuthContext, Credentials};

use crate::{AccountConfig, CoreError};

pub type ImapStream = TlsStream<TcpStream>;
pub type ImapSession = Session<ImapStream>;

/// Implicit TLS only; STARTTLS upgrade is not implemented.
pub async fn connect_and_login(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    danger_accept_invalid_certs: bool,
) -> Result<ImapSession, CoreError> {
    connect_and_authenticate(
        host,
        port,
        &Credentials::Password(password.to_string()),
        username,
        danger_accept_invalid_certs,
    )
    .await
}

/// Yields the payload exactly once, then empty strings. On failure Gmail sends
/// a base64 JSON error challenge and expects an empty response before it will
/// return the tagged `NO`; re-sending the payload hangs the exchange.
struct XOAuth2(Option<String>);

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        self.0.take().unwrap_or_default()
    }
}

pub async fn connect_and_authenticate(
    host: &str,
    port: u16,
    credentials: &Credentials,
    fallback_username: &str,
    danger_accept_invalid_certs: bool,
) -> Result<ImapSession, CoreError> {
    log::debug!("connecting to {host}:{port}");
    let tcp = TcpStream::connect((host, port)).await?;
    let tls = async_native_tls::TlsConnector::new()
        .danger_accept_invalid_certs(danger_accept_invalid_certs);
    let tls_stream = tls.connect(host, tcp).await?;
    log::debug!("tls established with {host}, reading greeting");
    let mut client = async_imap::Client::new(tls_stream);
    // `login` consumes the greeting internally; `authenticate` does not, and
    // without this XOAUTH2 reads the greeting as its own response and hangs.
    let _greeting = client.read_response().await?;
    log::debug!("greeting read, authenticating");

    let session = match credentials {
        Credentials::Password(password) => client
            .login(fallback_username, password)
            .await
            .map_err(|(e, _client)| e)?,
        Credentials::OAuth2 {
            username,
            access_token,
        } => {
            let payload = Credentials::xoauth2_payload(username, access_token);
            client
                .authenticate("XOAUTH2", XOAuth2(Some(payload)))
                .await
                .map_err(|(e, _client)| e)?
        }
    };
    Ok(session)
}

/// The adapter is consulted on every connection, never cached here: that is
/// what lets an OAuth adapter refresh an expired token.
pub async fn connect_for_account(
    config: &AccountConfig,
    auth: &Arc<dyn AuthAdapter>,
) -> Result<ImapSession, CoreError> {
    let ctx = AuthContext {
        account_id: config.keyring_ref.clone(),
        username: config.username.clone(),
    };
    let credentials = auth.credentials(&ctx).await?;

    connect_and_authenticate(
        &config.imap_host,
        config.imap_port,
        &credentials,
        &config.username,
        config.danger_accept_invalid_certs,
    )
    .await
}
