use std::collections::HashMap;

use crate::{boxed, AuthAdapter, AuthContext, AuthError, AuthFuture, Credentials};

/// Keyring calls block (D-Bus round trip; macOS Keychain can prompt), so the
/// `spawn_blocking` lives here once rather than in every caller.
pub struct KeyringAdapter {
    service: String,
}

impl KeyringAdapter {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

/// A slow read almost always means macOS is showing an authorization dialog --
/// it does this the first time each newly-built binary touches an item, and
/// until it is answered the read never returns.
const SLOW_KEYRING_READ: std::time::Duration = std::time::Duration::from_secs(5);

impl AuthAdapter for KeyringAdapter {
    fn name(&self) -> &'static str {
        "keyring"
    }

    fn credentials<'a>(&'a self, ctx: &'a AuthContext) -> AuthFuture<'a> {
        let service = self.service.clone();
        let username = ctx.username.clone();
        boxed(async move {
            let warn_for = username.clone();
            let looked_up = with_slow_warning(
                &warn_for,
                tokio::task::spawn_blocking(move || read_or_adopt(&service, &username)),
            )
            .await
            .map_err(|_| AuthError::Failed("keyring lookup task panicked".into()))??;
            Ok(Credentials::Password(looked_up))
        })
    }
}

/// Not a timeout: the dialog is legitimate and cancelling would turn a slow
/// success into a failure. This only makes the wait visible.
async fn with_slow_warning<T>(
    username: &str,
    task: tokio::task::JoinHandle<T>,
) -> Result<T, tokio::task::JoinError> {
    tokio::pin!(task);
    tokio::select! {
        result = &mut task => return result,
        _ = tokio::time::sleep(SLOW_KEYRING_READ) => {
            log::warn!(
                "still waiting on the keyring for {username} after {}s -- if your OS is showing a \
                 keychain authorization dialog, it needs an answer before this account can sync",
                SLOW_KEYRING_READ.as_secs()
            );
        }
    }
    task.await
}

/// A keyring read that falls back to the project's old service name once.
pub(crate) fn read_or_adopt(service: &str, key: &str) -> Result<String, AuthError> {
    match read_entry(service, key) {
        Err(AuthError::NotFound(_)) => adopt_legacy_secret(service, key),
        other => other,
    }
}

fn read_entry(service: &str, username: &str) -> Result<String, AuthError> {
    keyring::Entry::new(service, username)
        .and_then(|entry| entry.get_password())
        .map_err(|err| match err {
            keyring::Error::NoEntry => AuthError::NotFound(username.to_string()),
            other => AuthError::Failed(other.to_string()),
        })
}

/// Copies a secret stored under the project's old name across to the current
/// one, once, on the first lookup that misses.
///
/// A keyring entry is keyed by `(service, username)` and cannot be renamed, so
/// unlike the config and data directories this is a read-and-rewrite rather
/// than a move. The old entry is deliberately left in place: removing a
/// credential is not something to do on a best-effort path, and an abandoned
/// entry costs nothing but a line in a keyring listing.
///
/// Kept here rather than read from `birdman-config`, which depends on this
/// crate: the config layer builds adapters, so the arrow cannot point back.
/// Delete it alongside `birdman_config::LEGACY_NAME`.
const LEGACY_KEYRING_SERVICE: &str = "osprey";

fn adopt_legacy_secret(service: &str, username: &str) -> Result<String, AuthError> {
    let legacy = read_entry(LEGACY_KEYRING_SERVICE, username)?;
    match store_password(service, username, &legacy) {
        Ok(()) => log::info!("moved the keyring entry for {username} to {service}"),
        Err(err) => log::warn!("could not move the keyring entry for {username}: {err}"),
    }
    Ok(legacy)
}

pub fn store_password(service: &str, username: &str, password: &str) -> Result<(), AuthError> {
    keyring::Entry::new(service, username)
        .and_then(|entry| entry.set_password(password))
        .map_err(|err| AuthError::Failed(err.to_string()))
}

/// Trailing newlines are trimmed: `pass`, `gopass`, `bw` and friends all emit
/// one and none of them mean it as part of the secret.
pub struct CommandAdapter {
    program: String,
    args: Vec<String>,
}

impl CommandAdapter {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

impl AuthAdapter for CommandAdapter {
    fn name(&self) -> &'static str {
        "command"
    }

    fn credentials<'a>(&'a self, _ctx: &'a AuthContext) -> AuthFuture<'a> {
        let program = self.program.clone();
        let args = self.args.clone();
        boxed(async move {
            let output = tokio::task::spawn_blocking(move || {
                std::process::Command::new(&program).args(&args).output()
            })
            .await
            .map_err(|_| AuthError::Failed("credential command task panicked".into()))?
            .map_err(|err| {
                AuthError::Failed(format!("credential command failed to start: {err}"))
            })?;

            if !output.status.success() {
                // stdout is the secret and must never be logged, even here.
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AuthError::Failed(format!(
                    "credential command exited with {}: {}",
                    output.status,
                    stderr.trim()
                )));
            }
            let secret = String::from_utf8_lossy(&output.stdout)
                .trim_end_matches(['\n', '\r'])
                .to_string();
            if secret.is_empty() {
                return Err(AuthError::NotFound(
                    "credential command produced no output".into(),
                ));
            }
            Ok(Credentials::Password(secret))
        })
    }
}

pub struct EnvAdapter {
    var: String,
}

impl EnvAdapter {
    pub fn new(var: impl Into<String>) -> Self {
        Self { var: var.into() }
    }
}

impl AuthAdapter for EnvAdapter {
    fn name(&self) -> &'static str {
        "env"
    }

    fn credentials<'a>(&'a self, _ctx: &'a AuthContext) -> AuthFuture<'a> {
        let var = self.var.clone();
        boxed(async move {
            match std::env::var(&var) {
                Ok(value) if !value.is_empty() => Ok(Credentials::Password(value)),
                _ => Err(AuthError::NotFound(var)),
            }
        })
    }
}

pub struct StaticAdapter(pub HashMap<String, Credentials>);

impl AuthAdapter for StaticAdapter {
    fn name(&self) -> &'static str {
        "static"
    }

    fn credentials<'a>(&'a self, ctx: &'a AuthContext) -> AuthFuture<'a> {
        let found = self.0.get(&ctx.username).cloned();
        boxed(
            async move { found.ok_or_else(|| AuthError::NotFound("no static credential".into())) },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AuthContext {
        AuthContext {
            account_id: "test".into(),
            username: "me@example.com".into(),
        }
    }

    #[tokio::test]
    async fn a_command_adapter_reads_stdout_and_trims_the_newline() {
        let adapter = CommandAdapter::new("printf", vec!["hunter2\\n".to_string()]);
        match adapter.credentials(&ctx()).await.unwrap() {
            Credentials::Password(p) => assert_eq!(p, "hunter2"),
            other => panic!("expected a password, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_stderr_not_its_stdout() {
        let adapter = CommandAdapter::new(
            "sh",
            vec!["-c".into(), "echo secret; echo boom >&2; exit 3".into()],
        );
        let err = adapter.credentials(&ctx()).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("boom"), "{message}");
        assert!(
            !message.contains("secret"),
            "stdout must never reach an error message: {message}"
        );
    }

    #[tokio::test]
    async fn an_empty_command_result_is_not_found_rather_than_an_empty_password() {
        let adapter = CommandAdapter::new("true", vec![]);
        assert!(matches!(
            adapter.credentials(&ctx()).await,
            Err(AuthError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn env_adapter_reads_the_named_variable() {
        // SAFETY: single-threaded test, no other thread reads this var.
        unsafe { std::env::set_var("BIRDMAN_TEST_SECRET", "from-env") };
        let adapter = EnvAdapter::new("BIRDMAN_TEST_SECRET");
        match adapter.credentials(&ctx()).await.unwrap() {
            Credentials::Password(p) => assert_eq!(p, "from-env"),
            other => panic!("expected a password, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_env_var_is_not_found() {
        let adapter = EnvAdapter::new("BIRDMAN_TEST_DEFINITELY_UNSET");
        assert!(matches!(
            adapter.credentials(&ctx()).await,
            Err(AuthError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn a_static_adapter_stands_in_for_any_of_them() {
        let adapter = StaticAdapter(HashMap::from([(
            "me@example.com".to_string(),
            Credentials::OAuth2 {
                username: "me@example.com".into(),
                access_token: "tok".into(),
            },
        )]));
        let as_dyn: &dyn AuthAdapter = &adapter;
        assert!(matches!(
            as_dyn.credentials(&ctx()).await.unwrap(),
            Credentials::OAuth2 { .. }
        ));
    }
}
