use std::future::Future;
use std::pin::Pin;

mod compose;
mod message;

pub use compose::{forward_draft, parsed_from_summary, reply_draft, split_addrs, ComposeDraft};
pub use message::{OutgoingMessage, Recipient};

use birdman_store::{FolderId, MessageFlags, MessageId};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Command {
    ListFolders,
    SyncFolder {
        folder: FolderId,
    },
    BackfillBodies {
        folder: FolderId,
        budget: usize,
    },
    FetchBody {
        message: MessageId,
    },
    OpenMessage {
        message: MessageId,
        fetch_body: bool,
        mark_read: bool,
    },
    SetFlags {
        message: MessageId,
        flags: MessageFlags,
    },
    MoveMessage {
        message: MessageId,
        to_folder: FolderId,
    },
    DeleteMessage {
        message: MessageId,
    },
}

impl Command {
    pub fn describe(&self) -> &'static str {
        match self {
            Command::ListFolders => "refresh folders",
            Command::SyncFolder { .. } => "sync folder",
            Command::BackfillBodies { .. } => "download bodies",
            Command::FetchBody { .. } => "download message",
            Command::OpenMessage { .. } => "open message",
            Command::SetFlags { .. } => "update flags",
            Command::MoveMessage { .. } => "move message",
            Command::DeleteMessage { .. } => "delete message",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("{0} is not supported by this account")]
    Unsupported(&'static str),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Outcome {
    pub bodies_fetched: usize,
}

pub type BackendFuture = Pin<Box<dyn Future<Output = Result<Outcome, BackendError>> + Send>>;

/// Object-safe: the UI holds `Arc<dyn MailReceiver>`, hence the boxed future.
pub trait MailReceiver: Send + Sync + 'static {
    fn execute(&self, command: Command) -> BackendFuture;

    fn name(&self) -> &'static str;
}

pub trait MailSender: Send + Sync + 'static {
    fn send(&self, message: OutgoingMessage) -> SendFuture;

    fn name(&self) -> &'static str;
}

pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), BackendError>> + Send>>;

pub fn boxed_send(
    future: impl Future<Output = Result<(), BackendError>> + Send + 'static,
) -> SendFuture {
    Box::pin(future)
}

pub fn boxed(
    future: impl Future<Output = Result<Outcome, BackendError>> + Send + 'static,
) -> BackendFuture {
    Box::pin(future)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl MailReceiver for RecordingBackend {
        fn execute(&self, command: Command) -> BackendFuture {
            self.seen
                .lock()
                .unwrap()
                .push(command.describe().to_string());
            boxed(async { Ok(Outcome::default()) })
        }
        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[test]
    fn a_backend_is_usable_behind_a_trait_object() {
        let backend = std::sync::Arc::new(RecordingBackend::default());
        let as_dyn: std::sync::Arc<dyn MailReceiver> = backend.clone();
        drop(as_dyn.execute(Command::ListFolders));
        drop(as_dyn.execute(Command::DeleteMessage {
            message: MessageId(1),
        }));
        assert_eq!(
            *backend.seen.lock().unwrap(),
            vec!["refresh folders", "delete message"]
        );
    }

    #[test]
    fn unsupported_reads_as_a_limitation_not_a_failure() {
        let err = BackendError::Unsupported("move message");
        assert_eq!(
            err.to_string(),
            "move message is not supported by this account"
        );
    }
}
