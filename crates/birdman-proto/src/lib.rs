use birdman_store::{Account, SpecialUse};
use birdman_store::{AccountId, Folder, FolderId, MessageId, MessageSummary, PageCursor};

pub use birdman_backend::Command;

mod wire;
pub use wire::{socket_path, Frame, Request, RequestKind, WireResult, PROTOCOL_VERSION};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Query {
    Accounts,
    Folders {
        account: Option<AccountId>,
    },
    UnreadCounts,
    Messages {
        folders: Vec<FolderId>,
        cursor: Option<PageCursor>,
        limit: u32,
        filter: birdman_store::MessageFilter,
    },
    MessageCounts {
        folders: Vec<FolderId>,
    },
    Contacts {
        limit: u32,
    },
    Search {
        text: String,
        filter: birdman_store::MessageFilter,
        limit: u32,
    },
    Message {
        message: MessageId,
    },
    Body {
        message: MessageId,
    },
    InlineAttachments {
        message: MessageId,
    },
    Attachments {
        message: MessageId,
    },
    /// The one query that writes. Separate from `Attachments` so the copying
    /// does not make every other read wait behind it; send it on its own
    /// connection.
    MaterialiseAttachments {
        message: MessageId,
    },
    SyncStatus,
    /// Outgoing mail the daemon has not delivered yet.
    Outbox,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyncState {
    Idle,
    Syncing { folder: Option<String> },
    Failed { message: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
// Not boxed: serialized to a line either way, so boxing the large variant
// trades a stack copy for a heap allocation per response.
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Accounts(Vec<Account>),
    Folders(Vec<Folder>),
    UnreadCounts(Vec<(FolderId, u32)>),
    Messages(Vec<MessageSummary>),
    Message(Option<MessageSummary>),
    MessageCounts { total: u32, unread: u32 },
    Body(Option<MessageBody>),
    InlineAttachments(Vec<InlineAttachment>),
    Attachments(Vec<birdman_store::Attachment>),
    Contacts(Vec<birdman_store::Contact>),
    SyncStatus(Vec<(AccountId, SyncState)>),
    Outbox(Vec<birdman_store::OutboxEntry>),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessageBody {
    pub text: Option<String>,
    pub html: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InlineAttachment {
    pub content_id: String,
    pub content_type: Option<String>,
    pub cached_path: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Event {
    FoldersChanged {
        account: AccountId,
    },
    MessagesChanged {
        folder: FolderId,
    },
    SyncProgress {
        account: AccountId,
        folder: Option<String>,
    },
    SyncFailed {
        account: AccountId,
        message: String,
    },
    SyncIdle {
        account: AccountId,
    },
    /// A send was queued, delivered, failed or retried.
    OutboxChanged {
        account: AccountId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("{0}")]
    Store(String),
    #[error("{0}")]
    Backend(String),
    #[error("server answered {asked} with {got}")]
    Mismatch {
        asked: &'static str,
        got: &'static str,
    },
}

impl Query {
    pub fn describe(&self) -> &'static str {
        match self {
            Query::Accounts => "accounts",
            Query::Folders { .. } => "folders",
            Query::UnreadCounts => "unread counts",
            Query::Messages { .. } => "messages",
            Query::MessageCounts { .. } => "message counts",
            Query::Search { .. } => "search",
            Query::Message { .. } => "message",
            Query::Body { .. } => "message body",
            Query::InlineAttachments { .. } => "inline attachments",
            Query::Attachments { .. } => "attachments",
            Query::Contacts { .. } => "contacts",
            Query::MaterialiseAttachments { .. } => "materialise attachments",
            Query::SyncStatus => "sync status",
            Query::Outbox => "outbox",
        }
    }
}

impl Response {
    pub fn describe(&self) -> &'static str {
        match self {
            Response::Accounts(_) => "accounts",
            Response::Folders(_) => "folders",
            Response::UnreadCounts(_) => "unread counts",
            Response::Messages(_) => "messages",
            Response::MessageCounts { .. } => "message counts",
            Response::Message(_) => "message",
            Response::Body(_) => "message body",
            Response::InlineAttachments(_) => "inline attachments",
            Response::Attachments(_) => "attachments",
            Response::Contacts(_) => "contacts",
            Response::SyncStatus(_) => "sync status",
            Response::Outbox(_) => "outbox",
        }
    }
}

pub fn sidebar_folder_rank(folder: &Folder) -> u8 {
    if folder.imap_path.eq_ignore_ascii_case("INBOX") {
        return 0;
    }
    match folder.special_use {
        Some(SpecialUse::Flagged) => 1,
        Some(SpecialUse::Drafts) => 2,
        Some(SpecialUse::Sent) => 3,
        Some(SpecialUse::Trash) => 4,
        _ => OTHER_FOLDER_RANK,
    }
}

pub const OTHER_FOLDER_RANK: u8 = 5;

pub fn is_default_folder(folder: &Folder) -> bool {
    sidebar_folder_rank(folder) < OTHER_FOLDER_RANK
}
