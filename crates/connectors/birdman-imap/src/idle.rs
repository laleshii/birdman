use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;

use crate::connect::ImapSession;
use crate::CoreError;

/// Must stay under the ~29 minutes RFC 2177 allows before a server may drop
/// the connection. Gmail and corporate proxies also drop earlier, arbitrarily.
pub const IDLE_REFRESH_INTERVAL: Duration = Duration::from_secs(25 * 60);

pub enum IdleOutcome {
    Activity,
    RefreshTimeout,
}

/// An `Err` means the connection itself is suspect: reconnect rather than
/// idle again on the same session.
pub async fn idle_once(session: ImapSession) -> Result<(IdleOutcome, ImapSession), CoreError> {
    idle_once_for(session, IDLE_REFRESH_INTERVAL).await
}

/// A shorter ceiling keeps IDLE from swallowing the supervisor's periodic
/// sync. Must stay under RFC 2177's 29 minutes.
pub async fn idle_once_for(
    session: ImapSession,
    timeout: std::time::Duration,
) -> Result<(IdleOutcome, ImapSession), CoreError> {
    let mut idle = session.idle();
    idle.init().await?;
    let (wait, _interrupt) = idle.wait_with_timeout(timeout);
    let outcome = wait.await?;
    let session = idle.done().await?;

    let outcome = match outcome {
        IdleResponse::NewData(_) => IdleOutcome::Activity,
        IdleResponse::Timeout => IdleOutcome::RefreshTimeout,
        IdleResponse::ManualInterrupt => IdleOutcome::RefreshTimeout,
    };
    Ok((outcome, session))
}

pub async fn server_supports_idle(session: &mut ImapSession) -> Result<bool, CoreError> {
    let caps = session.capabilities().await?;
    Ok(caps.has_str("IDLE"))
}
