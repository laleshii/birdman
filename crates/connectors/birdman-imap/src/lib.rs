mod connect;
mod idle;
mod session_cache;
mod supervisor;
mod sync;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod backend;
pub use backend::ImapBackend;
pub use connect::{
    connect_and_authenticate, connect_and_login, connect_for_account, ImapSession, ImapStream,
};

pub use idle::{
    idle_once, idle_once_for, server_supports_idle, IdleOutcome, IDLE_REFRESH_INTERVAL,
};
pub use session_cache::{SessionCache, SessionGuard};
pub use supervisor::{backfill_folder_bodies, BODY_BUDGET_PER_SYNC};
pub use sync::{
    append_message, delete_message_remote, fetch_message_body, list_folder_paths,
    move_message_remote, set_flags_remote, sync_folder, sync_folder_list, FolderSyncResult,
};

use birdman_store::Store;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls error: {0}")]
    Tls(#[from] async_native_tls::Error),
    #[error("imap error: {0}")]
    Imap(#[from] async_imap::error::Error),
    #[error("store error: {0}")]
    Store(#[from] birdman_store::StoreError),
    #[error("credential error: {0}")]
    Credential(#[from] birdman_auth::AuthError),
    #[error("mime parse error: {0}")]
    MimeParse(#[from] birdman_mime::ParseError),
    #[error("credential lookup task panicked")]
    CredentialTaskPanicked,
    #[error("account has no INBOX")]
    NoInbox,
    #[error("server returned no data for this message (wrong mailbox selected, or it was moved/deleted remotely)")]
    MessageMissing,
    #[error("timed out talking to the server")]
    Timeout,
}

/// Gmail stalls a connection when several are opened in quick succession, and
/// a stalled one never returns. Without this cap the reading pane sits on
/// "Loading message..." forever.
pub const ON_DEMAND_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn with_timeout<F, T>(fut: F) -> Result<T, CoreError>
where
    F: Future<Output = Result<T, CoreError>>,
{
    tokio::time::timeout(ON_DEMAND_TIMEOUT, fut)
        .await
        .unwrap_or(Err(CoreError::Timeout))
}

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub account_id: birdman_store::AccountId,
    pub imap_host: String,
    pub imap_port: u16,
    pub username: String,
    pub keyring_ref: String,
    /// Never set for a real account: self-signed local/test servers only.
    pub danger_accept_invalid_certs: bool,
}

#[derive(Debug, Clone)]
pub enum SyncEvent {
    FoldersListed {
        account_id: birdman_store::AccountId,
    },
    FolderSyncing {
        account_id: birdman_store::AccountId,
        folder_name: String,
    },
    NewMessages {
        account_id: birdman_store::AccountId,
        folder_id: birdman_store::FolderId,
        uids: Vec<u32>,
    },
    SyncComplete {
        account_id: birdman_store::AccountId,
    },
    SyncError {
        account_id: birdman_store::AccountId,
        message: String,
    },
}

pub struct EngineHandle {
    pub events: async_channel::Receiver<SyncEvent>,
    /// Safe to `spawn` on from any thread, including gpui's non-tokio
    /// executors: it schedules onto birdman-imap's runtime regardless.
    pub runtime: tokio::runtime::Handle,
}

pub fn spawn(
    accounts: Vec<(AccountConfig, Arc<dyn birdman_auth::AuthAdapter>)>,
    store: Arc<Mutex<Store>>,
) -> EngineHandle {
    let (tx, rx) = async_channel::unbounded();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build birdman-imap tokio runtime");
    let runtime = rt.handle().clone();

    std::thread::spawn(move || {
        rt.block_on(async move {
            let handles: Vec<_> = accounts
                .into_iter()
                .map(|(account, auth)| {
                    tokio::spawn(supervisor::run_account(
                        account,
                        auth,
                        store.clone(),
                        tx.clone(),
                    ))
                })
                .collect();
            futures_util::future::join_all(handles).await;
        });
    });

    EngineHandle {
        events: rx,
        runtime,
    }
}
