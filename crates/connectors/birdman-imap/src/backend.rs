use std::sync::{Arc, Mutex};

use birdman_backend::{boxed, BackendError, BackendFuture, Command, MailReceiver, Outcome};
use birdman_store::{FolderId, MessageId, Store};

use crate::supervisor::{backfill_folder_bodies, BODY_BUDGET_PER_SYNC};
use birdman_auth::AuthAdapter;

use crate::{AccountConfig, CoreError, SessionCache};

pub struct ImapBackend {
    accounts: Vec<AccountConfig>,
    credentials: Arc<dyn AuthAdapter>,
    sessions: Arc<SessionCache>,
    store: Arc<Mutex<Store>>,
    runtime: tokio::runtime::Handle,
}

impl ImapBackend {
    pub fn new(
        accounts: Vec<AccountConfig>,
        credentials: Arc<dyn AuthAdapter>,
        sessions: Arc<SessionCache>,
        store: Arc<Mutex<Store>>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            accounts,
            credentials,
            sessions,
            store,
            runtime,
        }
    }

    fn folder(&self, folder_id: FolderId) -> Result<(AccountConfig, String), BackendError> {
        let store = self
            .store
            .lock()
            .map_err(|_| BackendError::Failed("store is poisoned".into()))?;
        let folder = store
            .get_folder(folder_id)
            .map_err(|err| BackendError::Failed(err.to_string()))?
            .ok_or_else(|| BackendError::NotFound("that folder no longer exists".into()))?;
        let account = self
            .accounts
            .iter()
            .find(|a| a.account_id == folder.account_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound("that account is not configured".into()))?;
        Ok((account, folder.imap_path))
    }

    fn message(
        &self,
        message_id: MessageId,
    ) -> Result<
        (
            AccountConfig,
            String,
            FolderId,
            u32,
            birdman_store::MessageFlags,
        ),
        BackendError,
    > {
        let (folder_id, uid, flags) = {
            let store = self
                .store
                .lock()
                .map_err(|_| BackendError::Failed("store is poisoned".into()))?;
            let message = store
                .get_message(message_id)
                .map_err(|err| BackendError::Failed(err.to_string()))?
                .ok_or_else(|| BackendError::NotFound("that message is no longer here".into()))?;
            (message.folder_id, message.uid, message.flags)
        };
        let (account, path) = self.folder(folder_id)?;
        Ok((account, path, folder_id, uid, flags))
    }
}

impl MailReceiver for ImapBackend {
    fn name(&self) -> &'static str {
        "imap"
    }

    fn execute(&self, command: Command) -> BackendFuture {
        let sessions = self.sessions.clone();
        let credentials = self.credentials.clone();
        let store = self.store.clone();
        let runtime = self.runtime.clone();

        // Up front on the caller's thread, so a `NotFound` costs no connection.
        let resolved = match &command {
            Command::ListFolders => self
                .accounts
                .first()
                .cloned()
                .map(|account| (account, String::from("INBOX"), None))
                .ok_or_else(|| BackendError::NotFound("no account configured".into())),
            Command::SyncFolder { folder } | Command::BackfillBodies { folder, .. } => self
                .folder(*folder)
                .map(|(account, path)| (account, path, None)),
            Command::FetchBody { message }
            | Command::OpenMessage { message, .. }
            | Command::SetFlags { message, .. }
            | Command::MoveMessage { message, .. }
            | Command::DeleteMessage { message } => {
                self.message(*message)
                    .map(|(account, path, folder_id, uid, flags)| {
                        (account, path, Some((folder_id, uid, flags)))
                    })
            }
        };

        let destination = match &command {
            Command::MoveMessage { to_folder, .. } => match self.folder(*to_folder) {
                Ok((_, path)) => Some(path),
                Err(err) => return boxed(async move { Err(err) }),
            },
            _ => None,
        };

        let (account, mailbox, message) = match resolved {
            Ok(resolved) => resolved,
            Err(err) => return boxed(async move { Err(err) }),
        };

        boxed(async move {
            let task = runtime.spawn(async move {
                crate::with_timeout(async move {
                    let mut session = sessions.selected(&account, &credentials, &mailbox).await?;
                    let result = run(
                        &mut session,
                        &store,
                        &command,
                        account.account_id,
                        message,
                        destination,
                    )
                    .await;
                    // The connection may be in an unknown state after a failure.
                    if result.is_err() {
                        session.invalidate();
                    }
                    result
                })
                .await
            });
            match task.await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(err)) => Err(BackendError::Failed(err.to_string())),
                Err(_) => Err(BackendError::Failed(
                    "the backend task did not finish".into(),
                )),
            }
        })
    }
}

async fn run(
    session: &mut crate::ImapSession,
    store: &Arc<Mutex<Store>>,
    command: &Command,
    // The account *this connector serves*, not whichever the store lists
    // first -- that bug wrote one account's folders under another's id.
    account_id: birdman_store::AccountId,
    message: Option<(FolderId, u32, birdman_store::MessageFlags)>,
    destination: Option<String>,
) -> Result<Outcome, CoreError> {
    match command {
        Command::ListFolders => {
            crate::sync::sync_folder_list(session, store, account_id).await?;
            Ok(Outcome::default())
        }
        Command::SyncFolder { folder } => {
            crate::sync::sync_folder(
                session,
                store,
                account_id,
                *folder,
                &mailbox_of(store, *folder),
            )
            .await?;
            let fetched =
                backfill_folder_bodies(session, store, *folder, BODY_BUDGET_PER_SYNC).await;
            Ok(Outcome {
                bodies_fetched: fetched,
            })
        }
        Command::BackfillBodies { folder, budget } => {
            let fetched = backfill_folder_bodies(session, store, *folder, *budget).await;
            Ok(Outcome {
                bodies_fetched: fetched,
            })
        }
        Command::FetchBody { message: id } => {
            let (_, uid, _) = message.expect("resolved above");
            crate::sync::fetch_message_body(session, store, *id, uid).await?;
            Ok(Outcome { bodies_fetched: 1 })
        }
        Command::OpenMessage {
            message: id,
            fetch_body,
            mark_read,
        } => {
            let (_, uid, flags) = message.expect("resolved above");
            let mut fetched = 0;
            if *fetch_body {
                crate::sync::fetch_message_body(session, store, *id, uid).await?;
                fetched = 1;
            }
            if *mark_read {
                let mut seen = flags;
                seen.seen = true;
                crate::sync::set_flags_remote(session, store, *id, uid, seen).await?;
            }
            Ok(Outcome {
                bodies_fetched: fetched,
            })
        }
        Command::SetFlags { message: id, flags } => {
            let (_, uid, _) = message.expect("resolved above");
            crate::sync::set_flags_remote(session, store, *id, uid, *flags).await?;
            Ok(Outcome::default())
        }
        Command::MoveMessage { message: id, .. } => {
            let (_, uid, flags) = message.expect("resolved above");
            let target = destination.expect("resolved above");
            crate::sync::move_message_remote(session, store, *id, uid, flags, &target).await?;
            Ok(Outcome::default())
        }
        Command::DeleteMessage { message: id } => {
            // Deleting moves to Trash, so it needs to know where the message
            // is now to avoid moving it to where it already is.
            let (folder_id, uid, flags) = message.expect("resolved above");
            crate::sync::delete_message_remote(session, store, *id, folder_id, uid, flags).await?;
            Ok(Outcome::default())
        }
    }
}

fn mailbox_of(store: &Arc<Mutex<Store>>, folder: FolderId) -> String {
    store
        .lock()
        .ok()
        .and_then(|s| s.get_folder(folder).ok().flatten())
        .map(|f| f.imap_path)
        .unwrap_or_default()
}
