//! OAuth2 for IMAP/SMTP, with Gmail as the worked case.
//!
//! Three constraints that are not obvious and are easy to undo:
//!
//! - **Loopback redirect.** Google shut off `urn:ietf:wg:oauth:2.0:oob` in
//!   2022, so the only supported desktop option is `http://127.0.0.1:<port>`
//!   on a listener we open. The port cannot be registered ahead of time;
//!   Google special-cases loopback and ignores the port when matching.
//! - **PKCE, despite the client secret.** A secret embedded in a binary the
//!   user can read is not a secret. PKCE is what binds the code to the client.
//! - **Only the refresh token is persisted**, under its own keyring entry
//!   (`oauth2:<username>`), separate from any password for the same account.
//!   The access token never touches disk.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::{boxed, AuthAdapter, AuthContext, AuthError, AuthFuture, Credentials};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuth2Endpoints {
    pub auth_url: String,
    pub token_url: String,
    pub scope: String,
}

impl OAuth2Endpoints {
    /// `https://mail.google.com/` is the only scope granting IMAP and SMTP.
    /// `gmail.readonly` and friends authenticate, then fail every IMAP command.
    pub fn google() -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scope: "https://mail.google.com/".into(),
        }
    }

    pub fn microsoft() -> Self {
        Self {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            scope: "https://outlook.office.com/IMAP.AccessAsUser.All \
                    https://outlook.office.com/SMTP.Send offline_access"
                .into(),
        }
    }

    pub fn parse_provider(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "google" | "gmail" => Some(Self::google()),
            "microsoft" | "outlook" | "office365" => Some(Self::microsoft()),
            _ => None,
        }
    }
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

pub struct OAuth2Adapter {
    endpoints: OAuth2Endpoints,
    client_id: String,
    client_secret: Option<String>,
    username: String,
    service: String,
    cached: Mutex<Option<CachedToken>>,
}

/// Refreshed this early, so a token cannot lapse mid-connection.
const EXPIRY_SKEW: Duration = Duration::from_secs(120);

impl OAuth2Adapter {
    pub fn new(
        endpoints: OAuth2Endpoints,
        client_id: impl Into<String>,
        client_secret: Option<String>,
        username: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            endpoints,
            client_id: client_id.into(),
            client_secret,
            username: username.into(),
            service: service.into(),
            cached: Mutex::new(None),
        }
    }

    /// Prefixed so it can never collide with the password entry for the same
    /// address.
    pub fn refresh_token_ref(username: &str) -> String {
        format!("oauth2:{username}")
    }

    fn cached_token(&self) -> Option<String> {
        let cached = self.cached.lock().ok()?;
        let token = cached.as_ref()?;
        (Instant::now() < token.expires_at).then(|| token.access_token.clone())
    }

    fn store_token(&self, access_token: &str, expires_in: u64) {
        if let Ok(mut cached) = self.cached.lock() {
            let lifetime = Duration::from_secs(expires_in).saturating_sub(EXPIRY_SKEW);
            *cached = Some(CachedToken {
                access_token: access_token.to_string(),
                expires_at: Instant::now() + lifetime,
            });
        }
    }
}

impl AuthAdapter for OAuth2Adapter {
    fn name(&self) -> &'static str {
        "oauth2"
    }

    fn credentials<'a>(&'a self, _ctx: &'a AuthContext) -> AuthFuture<'a> {
        if let Some(access_token) = self.cached_token() {
            let username = self.username.clone();
            return boxed(async move {
                Ok(Credentials::OAuth2 {
                    username,
                    access_token,
                })
            });
        }

        let endpoints = self.endpoints.clone();
        let client_id = self.client_id.clone();
        let client_secret = self.client_secret.clone();
        let username = self.username.clone();
        let service = self.service.clone();
        boxed(async move {
            let refresh_ref = Self::refresh_token_ref(&username);
            // Both the keyring read and the token POST block.
            let (access_token, expires_in) = tokio::task::spawn_blocking(move || {
                // The refresh token is a keyring entry like any other, so it
                // needs the same rename fallback the password path has.
                // Only a genuinely absent entry means "go and authorize". Every
                // other keyring failure is reported as itself: a locked or
                // unavailable keyring answered here as "no refresh token", which
                // sends the reader off to redo a browser consent that was never
                // the problem. macOS in dark wake produces exactly that.
                let refresh_token = crate::adapters::read_or_adopt(&service, &refresh_ref)
                    .map_err(|err| match err {
                        AuthError::NotFound(_) => AuthError::NotFound(format!(
                            "no OAuth2 refresh token for {username}. Run `birdman authorize <account>` first"
                        )),
                        other => other,
                    })?;
                refresh_access_token(&endpoints, &client_id, client_secret.as_deref(), &refresh_token)
            })
            .await
            .map_err(|_| AuthError::Failed("oauth2 refresh task panicked".into()))??;

            self.store_token(&access_token, expires_in);
            Ok(Credentials::OAuth2 {
                username: self.username.clone(),
                access_token,
            })
        })
    }
}

fn refresh_access_token(
    endpoints: &OAuth2Endpoints,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> Result<(String, u64), AuthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    post_token_endpoint(&endpoints.token_url, &form)
        .map(|response| (response.access_token, response.expires_in.unwrap_or(3600)))
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
}

fn post_token_endpoint(token_url: &str, form: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
    let response = ureq::post(token_url)
        .send_form(form.to_vec())
        .map_err(|err| AuthError::Failed(format!("token endpoint request failed: {err}")))?
        .body_mut()
        .read_to_string()
        .map_err(|err| AuthError::Failed(format!("token endpoint response unreadable: {err}")))?;

    serde_json::from_str::<TokenResponse>(&response).map_err(|_| {
        // The JSON error body is the only place the actual reason appears
        // (invalid_grant, unauthorized_client). It carries no secret.
        AuthError::Failed(format!(
            "token endpoint rejected the request: {}",
            response.trim()
        ))
    })
}

pub struct PendingAuthorization {
    pub authorize_url: String,
    listener: TcpListener,
    state: String,
    code_verifier: String,
    endpoints: OAuth2Endpoints,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: String,
}

impl PendingAuthorization {
    pub fn wait_for_refresh_token(self) -> Result<String, AuthError> {
        let (mut stream, _) = self
            .listener
            .accept()
            .map_err(|err| AuthError::Failed(format!("no redirect received: {err}")))?;

        let outcome = read_redirect(&mut stream).and_then(|query| {
            let code = query
                .iter()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    let denied = query
                        .iter()
                        .find(|(k, _)| k == "error")
                        .map(|(_, v)| v.clone());
                    AuthError::Failed(match denied {
                        Some(reason) => format!("authorization was refused: {reason}"),
                        None => "the redirect carried no authorization code".into(),
                    })
                })?;
            let returned_state = query
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.clone());
            // A mismatched state means the redirect did not come from the
            // request we made.
            if returned_state.as_deref() != Some(self.state.as_str()) {
                return Err(AuthError::Failed(
                    "state mismatch on the OAuth2 redirect".into(),
                ));
            }
            Ok(code)
        });

        let _ = respond_to_browser(&mut stream, outcome.is_ok());
        let code = outcome?;

        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_verifier", self.code_verifier.as_str()),
        ];
        if let Some(secret) = self.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        let response = post_token_endpoint(&self.endpoints.token_url, &form)?;
        response.refresh_token.ok_or_else(|| {
            AuthError::Failed(
                "the provider returned no refresh token. For Google this means the account had \
                 already granted consent -- revoke it at myaccount.google.com/permissions and retry"
                    .into(),
            )
        })
    }
}

pub fn begin_authorization(
    endpoints: OAuth2Endpoints,
    client_id: &str,
    client_secret: Option<String>,
    login_hint: Option<&str>,
) -> Result<PendingAuthorization, AuthError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|err| AuthError::Failed(format!("could not open a loopback listener: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| AuthError::Failed(err.to_string()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let state = random_token(24);
    let code_verifier = random_token(64);
    let code_challenge = pkce_challenge(&code_verifier);

    let mut url = url::Url::parse(&endpoints.auth_url)
        .map_err(|err| AuthError::Failed(format!("bad auth_url: {err}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &endpoints.scope)
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256")
            // Without both, Google returns no refresh token on re-consent.
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent");
        if let Some(hint) = login_hint {
            query.append_pair("login_hint", hint);
        }
    }

    Ok(PendingAuthorization {
        authorize_url: url.to_string(),
        listener,
        state,
        code_verifier,
        endpoints,
        client_id: client_id.to_string(),
        client_secret,
        redirect_uri,
    })
}

pub fn store_refresh_token(
    service: &str,
    username: &str,
    refresh_token: &str,
) -> Result<(), AuthError> {
    crate::store_password(
        service,
        &OAuth2Adapter::refresh_token_ref(username),
        refresh_token,
    )
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// RFC 7636 requires the verifier to be 43-128 unreserved characters.
fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill(&mut buf[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn read_redirect(stream: &mut TcpStream) -> Result<Vec<(String, String)>, AuthError> {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| AuthError::Failed(format!("could not read the redirect: {err}")))?;

    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AuthError::Failed("malformed redirect request".into()))?;
    let parsed = url::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|err| AuthError::Failed(format!("malformed redirect target: {err}")))?;
    Ok(parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect())
}

fn respond_to_browser(stream: &mut TcpStream, ok: bool) -> std::io::Result<()> {
    let body = if ok {
        "<h1>Birdman is authorized</h1><p>You can close this tab.</p>"
    } else {
        "<h1>Authorization failed</h1><p>Check the terminal for details.</p>"
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B. If this drifts, every authorization fails with an
    /// opaque `invalid_grant`.
    #[test]
    fn pkce_challenge_matches_the_rfc_test_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_url_safe_and_long_enough_for_the_rfc() {
        let verifier = random_token(64);
        assert!(
            (43..=128).contains(&verifier.len()),
            "length {}",
            verifier.len()
        );
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)),
            "{verifier}"
        );
    }

    #[test]
    fn two_tokens_differ() {
        assert_ne!(random_token(24), random_token(24));
    }

    #[test]
    fn the_google_authorize_url_carries_everything_the_flow_needs() {
        let pending = begin_authorization(
            OAuth2Endpoints::google(),
            "client-123",
            None,
            Some("me@gmail.com"),
        )
        .unwrap();
        let url = url::Url::parse(&pending.authorize_url).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(query["client_id"], "client-123");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["scope"], "https://mail.google.com/");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["login_hint"], "me@gmail.com");
        assert_eq!(query["access_type"], "offline");
        assert_eq!(query["prompt"], "consent");
        assert!(
            query["redirect_uri"].starts_with("http://127.0.0.1:"),
            "{}",
            query["redirect_uri"]
        );
        assert_eq!(
            query["code_challenge"],
            pkce_challenge(&pending.code_verifier)
        );
    }

    fn endpoints_with_dead_token_url() -> OAuth2Endpoints {
        OAuth2Endpoints {
            token_url: "http://127.0.0.1:1/token".into(),
            ..OAuth2Endpoints::google()
        }
    }

    fn hit_redirect(port: u16, query: &str) -> String {
        let mut stream =
            TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect to loopback");
        write!(
            stream,
            "GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        stream.flush().ok();
        let mut response = String::new();
        use std::io::Read;
        stream.read_to_string(&mut response).ok();
        response
    }

    #[test]
    fn the_loopback_redirect_captures_the_code_and_answers_the_browser() {
        let pending =
            begin_authorization(endpoints_with_dead_token_url(), "cid", None, None).unwrap();
        let port = pending.listener.local_addr().unwrap().port();
        let state = pending.state.clone();

        let waiting = std::thread::spawn(move || pending.wait_for_refresh_token());
        let response = hit_redirect(port, &format!("code=the-code&state={state}"));

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "browser should get a page: {response}"
        );
        assert!(response.contains("authorized"), "{response}");

        let err = waiting.join().unwrap().unwrap_err().to_string();
        assert!(
            err.contains("token endpoint"),
            "should have got past redirect parsing and the state check, got: {err}"
        );
    }

    #[test]
    fn a_redirect_with_the_wrong_state_is_refused() {
        let pending =
            begin_authorization(endpoints_with_dead_token_url(), "cid", None, None).unwrap();
        let port = pending.listener.local_addr().unwrap().port();

        let waiting = std::thread::spawn(move || pending.wait_for_refresh_token());
        hit_redirect(port, "code=the-code&state=not-the-state");

        let err = waiting.join().unwrap().unwrap_err().to_string();
        assert!(err.contains("state mismatch"), "{err}");
    }

    #[test]
    fn a_denied_consent_reports_the_providers_reason() {
        let pending =
            begin_authorization(endpoints_with_dead_token_url(), "cid", None, None).unwrap();
        let port = pending.listener.local_addr().unwrap().port();

        let waiting = std::thread::spawn(move || pending.wait_for_refresh_token());
        hit_redirect(port, "error=access_denied");

        let err = waiting.join().unwrap().unwrap_err().to_string();
        assert!(err.contains("access_denied"), "{err}");
    }

    #[test]
    fn refresh_token_entries_cannot_collide_with_password_entries() {
        assert_eq!(
            OAuth2Adapter::refresh_token_ref("me@gmail.com"),
            "oauth2:me@gmail.com"
        );
    }

    #[test]
    fn providers_parse_by_common_aliases() {
        assert_eq!(
            OAuth2Endpoints::parse_provider("Gmail"),
            Some(OAuth2Endpoints::google())
        );
        assert_eq!(
            OAuth2Endpoints::parse_provider(" outlook "),
            Some(OAuth2Endpoints::microsoft())
        );
        assert_eq!(OAuth2Endpoints::parse_provider("yahoo"), None);
    }

    #[test]
    fn a_cached_token_is_reused_until_it_nears_expiry() {
        let adapter = OAuth2Adapter::new(
            OAuth2Endpoints::google(),
            "id",
            None,
            "me@gmail.com",
            "birdman-test",
        );
        assert!(adapter.cached_token().is_none(), "nothing cached yet");

        adapter.store_token("tok", 3600);
        assert_eq!(adapter.cached_token().as_deref(), Some("tok"));

        adapter.store_token("tok", 30);
        assert!(
            adapter.cached_token().is_none(),
            "should refresh rather than risk a mid-connection lapse"
        );
    }
}
