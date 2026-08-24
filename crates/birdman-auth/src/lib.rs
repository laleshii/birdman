use std::future::Future;
use std::pin::Pin;

mod adapters;
mod oauth2;

pub use adapters::{store_password, CommandAdapter, EnvAdapter, KeyringAdapter, StaticAdapter};
pub use oauth2::{
    begin_authorization, store_refresh_token, OAuth2Adapter, OAuth2Endpoints, PendingAuthorization,
};

#[derive(Clone)]
pub enum Credentials {
    Password(String),
    OAuth2 {
        username: String,
        access_token: String,
    },
}

impl Credentials {
    /// SASL XOAUTH2 initial client response, byte-exact per Google/Microsoft:
    /// `user=<user>^Aauth=Bearer <token>^A^A`, where `^A` is `\x01`.
    pub fn xoauth2_payload(username: &str, access_token: &str) -> String {
        format!("user={username}\x01auth=Bearer {access_token}\x01\x01")
    }
}

/// Hand-written `Debug` redacts the secret: these reach logs and error paths.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Password(_) => f.write_str("Credentials::Password(<redacted>)"),
            Credentials::OAuth2 { username, .. } => {
                write!(
                    f,
                    "Credentials::OAuth2 {{ username: {username:?}, access_token: <redacted> }}"
                )
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub account_id: String,
    pub username: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no credential found for {0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
}

pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<Credentials, AuthError>> + Send + 'a>>;

pub trait AuthAdapter: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// Called per connection attempt: an expensive impl must cache internally.
    fn credentials<'a>(&'a self, ctx: &'a AuthContext) -> AuthFuture<'a>;
}

pub fn boxed<'a>(
    future: impl Future<Output = Result<Credentials, AuthError>> + Send + 'a,
) -> AuthFuture<'a> {
    Box::pin(future)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoauth2_payload_matches_the_specified_layout() {
        let payload = Credentials::xoauth2_payload("me@example.com", "tok");
        assert_eq!(payload, "user=me@example.com\x01auth=Bearer tok\x01\x01");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let password = format!("{:?}", Credentials::Password("hunter2".into()));
        assert!(!password.contains("hunter2"), "{password}");

        let token = format!(
            "{:?}",
            Credentials::OAuth2 {
                username: "me@example.com".into(),
                access_token: "s3cret".into()
            }
        );
        assert!(!token.contains("s3cret"), "{token}");
        assert!(token.contains("me@example.com"));
    }
}
