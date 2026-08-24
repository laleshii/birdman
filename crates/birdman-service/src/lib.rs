use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use birdman_backend::{Command, MailReceiver, MailSender, Outcome, OutgoingMessage};
use birdman_proto::{Event, InlineAttachment, MessageBody, ProtoError, Query, Response, SyncState};
use birdman_store::{AccountId, Folder, FolderId, MessageId, MessageSummary, PageCursor, Store};

pub use birdman_proto::{is_default_folder, sidebar_folder_rank, OTHER_FOLDER_RANK};

pub struct AccountBackends {
    pub id: AccountId,
    pub receiver: Arc<dyn MailReceiver>,
    pub sender: Arc<dyn MailSender>,
}

pub type ServiceFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

pub struct Service {
    store: Arc<Mutex<Store>>,
    /// A second connection, used only by [`Self::query`]. WAL lets readers and
    /// one writer coexist; the `Mutex` around the shared store was the only
    /// thing serialising them, and a read behind a 23s folder sync waited the
    /// whole 23s. Do not collapse these back into one.
    reader: Mutex<Store>,
    backends: Vec<AccountBackends>,
    subscribers: Mutex<Vec<async_channel::Sender<Event>>>,
    /// Events are deltas and are never replayed, so a client connecting after
    /// a sync finished needs this to learn that it did.
    sync_state: Mutex<std::collections::BTreeMap<AccountId, SyncState>>,
    /// Wakes the daemon's outbox worker the moment something is queued or
    /// retried, instead of leaving it to poll.
    outbox_wake: std::sync::Arc<tokio::sync::Notify>,
}

type Result<T> = std::result::Result<T, ProtoError>;

impl Service {
    pub fn new(store: Arc<Mutex<Store>>, reader: Store, backends: Vec<AccountBackends>) -> Self {
        Self {
            store,
            reader: Mutex::new(reader),
            backends,
            subscribers: Mutex::new(Vec::new()),
            sync_state: Mutex::new(std::collections::BTreeMap::new()),
            outbox_wake: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn execute(
        self: &Arc<Self>,
        account: AccountId,
        command: Command,
    ) -> ServiceFuture<Outcome> {
        let label = command.describe();
        let Some(receiver) = self.receiver(account) else {
            return Box::pin(async move { Err(no_connector(label, account)) });
        };
        // While the message is still where it was: a move or delete makes its
        // own folder unfindable afterwards.
        let touched = self.folders_touched(account, &command);
        let service = self.clone();
        Box::pin(async move {
            let outcome = receiver
                .execute(command)
                .await
                .map_err(|err| ProtoError::Backend(err.to_string()))?;
            for event in touched {
                service.publish(event);
            }
            Ok(outcome)
        })
    }

    fn folders_touched(&self, account: AccountId, command: &Command) -> Vec<Event> {
        let folder_of = |message: birdman_store::MessageId| -> Option<FolderId> {
            let store = self.store.lock().ok()?;
            Some(store.get_message(message).ok()??.folder_id)
        };
        match command {
            Command::ListFolders => vec![Event::FoldersChanged { account }],
            Command::SyncFolder { folder } | Command::BackfillBodies { folder, .. } => {
                vec![Event::MessagesChanged { folder: *folder }]
            }
            // The source folder is only knowable while the message is in it.
            Command::MoveMessage { message, to_folder } => folder_of(*message)
                .into_iter()
                .chain(std::iter::once(*to_folder))
                .map(|folder| Event::MessagesChanged { folder })
                .collect(),
            // Announcing these made every arrow keypress refresh the list that
            // caused it.
            Command::FetchBody { .. } => Vec::new(),
            Command::OpenMessage {
                message, mark_read, ..
            } => (*mark_read)
                .then(|| folder_of(*message))
                .flatten()
                .map(|folder| vec![Event::MessagesChanged { folder }])
                .unwrap_or_default(),
            Command::SetFlags { message, .. } | Command::DeleteMessage { message } => {
                folder_of(*message)
                    .map(|folder| vec![Event::MessagesChanged { folder }])
                    .unwrap_or_default()
            }
        }
    }

    /// Durably queues the message and returns at once; delivery happens on
    /// the daemon's outbox worker, with retries. What a client is told here
    /// is "your mail cannot be lost", not "the server has it" -- the latter
    /// arrives later as [`Event::OutboxChanged`].
    pub fn queue_send(
        &self,
        account: AccountId,
        mut message: OutgoingMessage,
    ) -> Result<birdman_store::OutboxId> {
        if !self.has_account(account) {
            return Err(no_connector("send message", account));
        }
        let recipients = message.to.len() + message.cc.len() + message.bcc.len();
        if recipients == 0 {
            return Err(ProtoError::Backend(
                "message must have at least one recipient".into(),
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        message.date.get_or_insert(now.as_secs() as i64);
        message.message_id.get_or_insert_with(|| {
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let domain = message
                .from
                .address
                .rsplit_once('@')
                .map(|(_, domain)| domain)
                .unwrap_or("localhost");
            format!(
                "{}.{}.{}@{domain}",
                now.as_secs(),
                now.subsec_nanos(),
                sequence
            )
        });
        let payload = serde_json::to_string(&message)
            .map_err(|err| ProtoError::Backend(format!("could not encode the message: {err}")))?;
        let entry = self
            .store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .queue_outgoing(account, &payload)
            .map_err(failed)?;
        log::info!(
            "queued outgoing message {} for account {}",
            entry.id.0,
            account.0
        );
        self.publish(Event::OutboxChanged { account });
        self.outbox_wake.notify_one();
        Ok(entry.id)
    }

    /// One delivery attempt, straight through the connector. The outbox
    /// worker decides retry policy around this.
    pub fn deliver(&self, account: AccountId, message: OutgoingMessage) -> ServiceFuture<()> {
        let Some(sender) = self.sender(account) else {
            return Box::pin(async move { Err(no_connector("send message", account)) });
        };
        Box::pin(async move {
            sender
                .send(message)
                .await
                .map_err(|err| ProtoError::Backend(err.to_string()))
        })
    }

    pub fn outbox_entries(&self) -> Result<Vec<birdman_store::OutboxEntry>> {
        self.store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .list_outbox()
            .map_err(failed)
    }

    pub fn outbox_has_automatic_work(&self) -> bool {
        self.outbox_entries().is_ok_and(|entries| {
            entries.iter().any(|entry| {
                matches!(
                    entry.state,
                    birdman_store::OutboxState::Queued | birdman_store::OutboxState::Sending
                ) || (entry.state == birdman_store::OutboxState::Failed
                    && entry.next_attempt_at != i64::MAX)
            })
        })
    }

    pub fn sweep_sent_outbox(&self, before: i64) -> Result<usize> {
        self.store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .sweep_sent_outbox(before)
            .map_err(failed)
    }

    fn wake_outbox(&self) {
        self.outbox_wake.notify_one();
    }

    pub fn outbox_retry(&self, id: birdman_store::OutboxId) -> Result<bool> {
        let changed = self
            .store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .retry_outgoing(id)
            .map_err(failed)?;
        if changed {
            if let Some(account) = self.outbox_account(id) {
                self.publish(Event::OutboxChanged { account });
            }
            self.wake_outbox();
        }
        Ok(changed)
    }

    pub fn outbox_cancel(&self, id: birdman_store::OutboxId) -> Result<bool> {
        let account = self.outbox_account(id);
        let removed = self
            .store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .cancel_outgoing(id)
            .map_err(failed)?;
        if removed {
            if let Some(account) = account {
                self.publish(Event::OutboxChanged { account });
            }
        }
        Ok(removed)
    }

    /// The worker needs due rows without going through [`Self::query`],
    /// because it also holds no opinion about reader/writer separation --
    /// it *is* the writer's client here.
    pub fn due_outgoing(&self, now: i64) -> Result<Vec<birdman_store::OutboxEntry>> {
        self.store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .due_outgoing(now)
            .map_err(failed)
    }

    pub fn outbox_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        self.outbox_wake.clone()
    }

    fn outbox_account(&self, id: birdman_store::OutboxId) -> Option<AccountId> {
        self.store
            .lock()
            .ok()?
            .list_outbox()
            .ok()?
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.account_id)
    }

    pub fn has_account(&self, account: AccountId) -> bool {
        self.backends.iter().any(|b| b.id == account)
    }

    fn receiver(&self, account: AccountId) -> Option<Arc<dyn MailReceiver>> {
        self.backends
            .iter()
            .find(|b| b.id == account)
            .map(|b| b.receiver.clone())
    }

    fn sender(&self, account: AccountId) -> Option<Arc<dyn MailSender>> {
        self.backends
            .iter()
            .find(|b| b.id == account)
            .map(|b| b.sender.clone())
    }

    pub fn subscribe(&self) -> async_channel::Receiver<Event> {
        let (tx, rx) = async_channel::unbounded();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(tx);
        }
        rx
    }

    /// Unbounded and non-blocking: a slow client must not stall the sync engine
    /// publishing to it.
    pub fn publish(&self, event: Event) {
        // Before it is sent: a client querying immediately after delivery must
        // not see a state older than the event it just received.
        if let Ok(mut state) = self.sync_state.lock() {
            match &event {
                Event::SyncProgress { account, folder } => {
                    state.insert(
                        *account,
                        SyncState::Syncing {
                            folder: folder.clone(),
                        },
                    );
                }
                Event::SyncIdle { account } => {
                    state.insert(*account, SyncState::Idle);
                }
                Event::SyncFailed { account, message } => {
                    state.insert(
                        *account,
                        SyncState::Failed {
                            message: message.clone(),
                        },
                    );
                }
                Event::FoldersChanged { .. }
                | Event::MessagesChanged { .. }
                | Event::OutboxChanged { .. } => {}
            }
        }

        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        subscribers.retain(|tx| tx.try_send(event.clone()).is_ok() || !tx.is_closed());
    }

    /// Uses the writer, since it deletes.
    pub fn sweep_attachments(&self) -> Result<birdman_store::SweepReport> {
        self.store
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?
            .sweep_attachment_cache()
            .map_err(|err| ProtoError::Store(err.to_string()))
    }

    pub fn query(&self, query: Query) -> Result<Response> {
        let store = self
            .reader
            .lock()
            .map_err(|_| ProtoError::Store("store is poisoned".into()))?;
        let store = &*store;
        let failed = |err: birdman_store::StoreError| ProtoError::Store(err.to_string());

        Ok(match query {
            Query::Accounts => Response::Accounts(store.list_accounts().map_err(failed)?),

            Query::Folders { account } => {
                let accounts = match account {
                    Some(id) => vec![id],
                    None => store
                        .list_accounts()
                        .map_err(failed)?
                        .into_iter()
                        .map(|a| a.id)
                        .collect(),
                };
                let mut folders = Vec::new();
                for id in accounts {
                    let mut theirs = store.list_folders(id).map_err(failed)?;
                    theirs.sort_by_key(sidebar_folder_rank);
                    folders.append(&mut theirs);
                }
                Response::Folders(folders)
            }

            Query::UnreadCounts => Response::UnreadCounts(store.unread_counts().map_err(failed)?),

            Query::Messages {
                folders,
                cursor,
                limit,
                filter,
            } => Response::Messages(
                store
                    .list_messages_page(&folders, cursor, limit, filter)
                    .map_err(failed)?,
            ),

            Query::MessageCounts { folders } => {
                let (total, unread) = store.count_messages(&folders).map_err(failed)?;
                Response::MessageCounts { total, unread }
            }

            Query::Search {
                text,
                filter,
                limit,
            } => Response::Messages(store.search(&text, filter, limit).map_err(failed)?),

            Query::Message { message } => {
                Response::Message(store.get_message(message).map_err(failed)?)
            }

            Query::Body { message } => Response::Body(
                store
                    .get_message_body(message)
                    .map_err(failed)?
                    .map(|(text, html)| MessageBody { text, html }),
            ),

            Query::SyncStatus => {
                let state = self
                    .sync_state
                    .lock()
                    .map_err(|_| ProtoError::Store("sync state is poisoned".into()))?;
                Response::SyncStatus(
                    self.backends
                        .iter()
                        .map(|b| (b.id, state.get(&b.id).cloned().unwrap_or(SyncState::Idle)))
                        .collect(),
                )
            }

            Query::Attachments { message } => {
                Response::Attachments(store.attachments(message).map_err(failed)?)
            }

            // Writes, so it takes the writer.
            Query::MaterialiseAttachments { message } => Response::Attachments(
                self.store
                    .lock()
                    .map_err(|_| ProtoError::Store("store is poisoned".into()))?
                    .materialise_attachments(message)
                    .map_err(failed)?,
            ),

            Query::Contacts { limit } => Response::Contacts(store.contacts(limit).map_err(failed)?),

            Query::Outbox => Response::Outbox(store.list_outbox().map_err(failed)?),

            Query::InlineAttachments { message } => Response::InlineAttachments(
                store
                    .get_inline_attachments(message)
                    .map_err(failed)?
                    .into_iter()
                    .map(|a| InlineAttachment {
                        content_id: a.content_id,
                        content_type: a.content_type,
                        cached_path: a.cached_path,
                    })
                    .collect(),
            ),
        })
    }

    pub fn accounts(&self) -> Result<Vec<birdman_store::Account>> {
        match self.query(Query::Accounts)? {
            Response::Accounts(accounts) => Ok(accounts),
            other => Err(mismatch("accounts", other)),
        }
    }

    pub fn folders(&self, account: Option<AccountId>) -> Result<Vec<Folder>> {
        match self.query(Query::Folders { account })? {
            Response::Folders(folders) => Ok(folders),
            other => Err(mismatch("folders", other)),
        }
    }

    pub fn unread_counts(&self) -> Result<Vec<(FolderId, u32)>> {
        match self.query(Query::UnreadCounts)? {
            Response::UnreadCounts(counts) => Ok(counts),
            other => Err(mismatch("unread counts", other)),
        }
    }

    pub fn messages(
        &self,
        folders: Vec<FolderId>,
        cursor: Option<PageCursor>,
        limit: u32,
        filter: birdman_store::MessageFilter,
    ) -> Result<Vec<MessageSummary>> {
        match self.query(Query::Messages {
            folders,
            cursor,
            limit,
            filter,
        })? {
            Response::Messages(messages) => Ok(messages),
            other => Err(mismatch("messages", other)),
        }
    }

    pub fn message_counts(&self, folders: Vec<FolderId>) -> Result<(u32, u32)> {
        match self.query(Query::MessageCounts { folders })? {
            Response::MessageCounts { total, unread } => Ok((total, unread)),
            other => Err(mismatch("message counts", other)),
        }
    }

    pub fn search(
        &self,
        text: impl Into<String>,
        filter: birdman_store::MessageFilter,
        limit: u32,
    ) -> Result<Vec<MessageSummary>> {
        match self.query(Query::Search {
            text: text.into(),
            filter,
            limit,
        })? {
            Response::Messages(messages) => Ok(messages),
            other => Err(mismatch("search", other)),
        }
    }

    pub fn message(&self, message: MessageId) -> Result<Option<MessageSummary>> {
        match self.query(Query::Message { message })? {
            Response::Message(found) => Ok(found),
            other => Err(mismatch("message", other)),
        }
    }

    pub fn body(&self, message: MessageId) -> Result<Option<MessageBody>> {
        match self.query(Query::Body { message })? {
            Response::Body(body) => Ok(body),
            other => Err(mismatch("message body", other)),
        }
    }

    pub fn inline_attachments(&self, message: MessageId) -> Result<Vec<InlineAttachment>> {
        match self.query(Query::InlineAttachments { message })? {
            Response::InlineAttachments(attachments) => Ok(attachments),
            other => Err(mismatch("inline attachments", other)),
        }
    }

    pub fn sync_status(&self) -> Result<Vec<(AccountId, SyncState)>> {
        match self.query(Query::SyncStatus)? {
            Response::SyncStatus(state) => Ok(state),
            other => Err(mismatch("sync status", other)),
        }
    }

    pub fn store(&self) -> &Arc<Mutex<Store>> {
        &self.store
    }
}

fn no_connector(label: &'static str, account: AccountId) -> ProtoError {
    ProtoError::Backend(format!("{label}: no connector for account {}", account.0))
}

fn failed(err: birdman_store::StoreError) -> ProtoError {
    ProtoError::Store(err.to_string())
}

fn mismatch(asked: &'static str, got: Response) -> ProtoError {
    ProtoError::Mismatch {
        asked,
        got: got.describe(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use birdman_store::{NewAccount, NewFolder, Security, SpecialUse};

    fn service() -> (Service, tempfile::TempDir, AccountId) {
        let dir = tempfile::tempdir().unwrap();
        // File-backed: the service holds two connections, and two in-memory
        // connections are two different empty databases.
        let store = Store::open(&dir.path().join("mail.db"), dir.path()).unwrap();
        let account = store
            .insert_account(&NewAccount {
                display_name: "Test",
                email: "me@example.com",
                imap_host: "imap.example.com",
                imap_port: 993,
                imap_security: Security::Tls,
                smtp_host: "smtp.example.com",
                smtp_port: 465,
                smtp_security: Security::Tls,
                username: "me@example.com",
                keyring_ref: "me@example.com",
            })
            .unwrap();
        let reader = Store::open(&dir.path().join("mail.db"), dir.path()).unwrap();
        (
            Service::new(Arc::new(Mutex::new(store)), reader, Vec::new()),
            dir,
            account,
        )
    }

    fn folder(
        service: &Service,
        account: AccountId,
        path: &str,
        special: Option<SpecialUse>,
    ) -> FolderId {
        service
            .store()
            .lock()
            .unwrap()
            .upsert_folder(&NewFolder {
                account_id: account,
                name: path,
                imap_path: path,
                delimiter: Some("/"),
                subscribed: true,
                special_use: special,
            })
            .unwrap()
    }

    #[test]
    fn folders_come_back_in_sidebar_order_regardless_of_insertion_order() {
        let (service, _dir, account) = service();
        folder(&service, account, "zzz-custom", None);
        folder(&service, account, "[Gmail]/Trash", Some(SpecialUse::Trash));
        folder(&service, account, "INBOX", None);
        folder(&service, account, "[Gmail]/Sent", Some(SpecialUse::Sent));

        let paths: Vec<_> = service
            .folders(None)
            .unwrap()
            .into_iter()
            .map(|f| f.imap_path)
            .collect();
        assert_eq!(
            paths,
            vec!["INBOX", "[Gmail]/Sent", "[Gmail]/Trash", "zzz-custom"]
        );
    }

    #[test]
    fn folders_can_be_scoped_to_one_account() {
        let (service, _dir, first) = service();
        let second = service
            .store()
            .lock()
            .unwrap()
            .insert_account(&NewAccount {
                display_name: "Other",
                email: "other@example.com",
                imap_host: "imap.example.com",
                imap_port: 993,
                imap_security: Security::Tls,
                smtp_host: "smtp.example.com",
                smtp_port: 465,
                smtp_security: Security::Tls,
                username: "other@example.com",
                keyring_ref: "other@example.com",
            })
            .unwrap();
        folder(&service, first, "INBOX", None);
        folder(&service, second, "INBOX", None);

        assert_eq!(service.folders(None).unwrap().len(), 2);
        assert_eq!(service.folders(Some(second)).unwrap().len(), 1);
    }

    #[test]
    fn a_query_and_its_response_are_matched_by_the_helper() {
        let (service, _dir, _account) = service();
        assert!(service.accounts().unwrap().len() == 1);
        assert!(service.unread_counts().unwrap().is_empty());
        assert_eq!(service.message_counts(vec![]).unwrap(), (0, 0));
        assert!(service.body(MessageId(1)).unwrap().is_none());
    }

    #[test]
    fn a_query_value_carries_its_own_label_for_logs() {
        assert_eq!(Query::Accounts.describe(), "accounts");
        assert_eq!(
            Query::Search {
                text: "x".into(),
                filter: Default::default(),
                limit: 1
            }
            .describe(),
            "search"
        );
    }

    #[test]
    fn default_folder_classification_is_served_not_guessed() {
        let (service, _dir, account) = service();
        let inbox = folder(&service, account, "INBOX", None);
        let custom = folder(&service, account, "receipts", None);
        let folders = service.folders(Some(account)).unwrap();

        let by_id = |id: FolderId| folders.iter().find(|f| f.id == id).unwrap();
        assert!(is_default_folder(by_id(inbox)));
        assert!(!is_default_folder(by_id(custom)));
    }

    /// The reason the second connection exists: a read must not queue behind
    /// whatever the sync engine is holding the write store for.
    #[test]
    fn a_query_is_answered_while_the_writer_holds_the_store() {
        let (service, _dir, account) = service();
        folder(&service, account, "INBOX", None);

        let held = service.store.lock().unwrap();

        let answered = service.query(Query::Folders { account: None });
        assert!(answered.is_ok(), "a read must not wait on the writer");
        match answered.unwrap() {
            Response::Folders(folders) => assert_eq!(folders.len(), 1),
            other => panic!("unexpected response: {other:?}"),
        }
        drop(held);
    }
}
