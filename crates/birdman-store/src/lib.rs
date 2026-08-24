use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use birdman_mime::{Attachment as ParsedAttachment, ParsedMessage};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use rusqlite_migration::Migrations;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("schema migration failed: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

macro_rules! id_type {
    ($name:ident) => {
        // Transparent, so an id crosses the socket as a bare number rather
        // than a wrapper object.
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);
    };
}
id_type!(AccountId);
id_type!(FolderId);
id_type!(MessageId);
id_type!(AttachmentId);
id_type!(OutboxId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Security {
    Tls,
    StartTls,
    None,
}

impl Security {
    fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
            Security::None => "none",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "tls" => Security::Tls,
            "starttls" => Security::StartTls,
            _ => Security::None,
        }
    }
}

pub struct NewAccount<'a> {
    pub display_name: &'a str,
    pub email: &'a str,
    pub imap_host: &'a str,
    pub imap_port: u16,
    pub imap_security: Security,
    pub smtp_host: &'a str,
    pub smtp_port: u16,
    pub smtp_security: Security,
    pub username: &'a str,
    pub keyring_ref: &'a str,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: Security,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    pub username: String,
    pub keyring_ref: String,
}

/// RFC 6154 `SPECIAL-USE`, stored as its lowercase attribute name. `Inbox` is
/// never one of these: servers do not tag it, and RFC 3501 identifies it by its
/// reserved case-insensitive `imap_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SpecialUse {
    Drafts,
    Sent,
    Flagged,
    Junk,
    Trash,
    Archive,
    All,
}

impl SpecialUse {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecialUse::Drafts => "drafts",
            SpecialUse::Sent => "sent",
            SpecialUse::Flagged => "flagged",
            SpecialUse::Junk => "junk",
            SpecialUse::Trash => "trash",
            SpecialUse::Archive => "archive",
            SpecialUse::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "drafts" => Some(SpecialUse::Drafts),
            "sent" => Some(SpecialUse::Sent),
            "flagged" => Some(SpecialUse::Flagged),
            "junk" => Some(SpecialUse::Junk),
            "trash" => Some(SpecialUse::Trash),
            "archive" => Some(SpecialUse::Archive),
            "all" => Some(SpecialUse::All),
            _ => None,
        }
    }
}

pub struct NewFolder<'a> {
    pub account_id: AccountId,
    pub name: &'a str,
    pub imap_path: &'a str,
    pub delimiter: Option<&'a str>,
    pub subscribed: bool,
    pub special_use: Option<SpecialUse>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub imap_path: String,
    pub delimiter: Option<String>,
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
    pub highest_modseq: Option<u64>,
    pub subscribed: bool,
    pub special_use: Option<SpecialUse>,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct MessageFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub deleted: bool,
    pub draft: bool,
}

/// A value rather than a run of booleans: this crosses four crates, and
/// `messages(folders, cursor, limit, false, true)` invites a swap the compiler
/// cannot catch. Empty means everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MessageFilter {
    pub unread: bool,
    pub attachments: bool,
}

impl MessageFilter {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// Literal SQL rather than bound parameters: every branch is a fixed string
    /// chosen here, and nothing from a caller reaches it.
    fn sql(self) -> &'static str {
        match (self.unread, self.attachments) {
            (false, false) => "",
            (true, false) => " AND flag_seen = 0",
            (false, true) => " AND has_attachments = 1",
            (true, true) => " AND flag_seen = 0 AND has_attachments = 1",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessageSummary {
    pub id: MessageId,
    pub folder_id: FolderId,
    pub uid: u32,
    pub subject: Option<String>,
    pub from_addr: Option<String>,
    /// `None` when `From` carried no display name; callers fall back to
    /// `from_addr`.
    pub from_name: Option<String>,
    /// Comma-separated as stored, never split into structured mailboxes.
    pub to_addrs: Option<String>,
    pub cc_addrs: Option<String>,
    pub reply_to_addrs: Option<String>,
    pub bcc_addrs: Option<String>,
    pub message_id_header: Option<String>,
    pub references: Vec<String>,
    pub date: Option<i64>,
    pub has_attachments: bool,
    pub flags: MessageFlags,
    pub body_fetched: bool,
    /// From the truncated `BODY.PEEK[TEXT]` taken during envelope sync. `None`
    /// for anything synced before previews existed.
    pub preview: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    pub name: Option<String>,
    pub address: String,
    pub seen: u32,
    pub last_seen: i64,
}

/// Distinct from [`InlineAttachment`], which only resolves a `cid:` in an HTML
/// body and is never shown as a file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub content_type: Option<String>,
    pub size: i64,
    /// The materialised copy, not the content-addressed blob. `None` until that
    /// copy exists -- name and size are known long before the bytes are in
    /// place, and no path means no drag.
    pub path: Option<String>,
}

/// Resolves an HTML body's `<img src="cid:...">` to bytes on disk.
#[derive(Debug, Clone)]
pub struct InlineAttachment {
    pub content_id: String,
    pub content_type: Option<String>,
    pub cached_path: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct PageCursor {
    pub date: i64,
    pub id: MessageId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutboxState {
    Queued,
    Sending,
    Sent,
    Failed,
}

impl OutboxState {
    fn as_str(self) -> &'static str {
        match self {
            OutboxState::Queued => "queued",
            OutboxState::Sending => "sending",
            OutboxState::Sent => "sent",
            OutboxState::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "sending" => OutboxState::Sending,
            "sent" => OutboxState::Sent,
            "failed" => OutboxState::Failed,
            _ => OutboxState::Queued,
        }
    }
}

/// One outgoing message on its way. `payload` is opaque JSON here -- the
/// store sits below the crate that defines `OutgoingMessage` and must not
/// depend upward to know its shape.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
    pub id: OutboxId,
    pub account_id: AccountId,
    pub state: OutboxState,
    pub payload: String,
    pub attempts: u32,
    pub last_error: Option<String>,
    /// Unix seconds; a queued row is due when this has passed.
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub sent_at: Option<i64>,
}

pub struct Store {
    conn: Connection,
    data_dir: PathBuf,
}

/// Repairs a whole tree, because a write only fixes its own shard and `init`
/// only fixed the top -- an older store kept `0755` shards of `0644` blobs.
/// Converges: later runs find nothing to do.
fn restrict_tree(root: &Path) {
    restrict_to_owner(root);
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            restrict_tree(&path);
        } else {
            restrict_to_owner(&path);
        }
    }
}

/// `0700` for a directory, `0600` for a file. A twin of
/// `birdman_config::restrict_to_owner`, duplicated because `birdman-store` sits
/// *below* the config crate and depending upward would invert the layering.
///
/// Infallible: a filesystem without Unix modes has nothing to tighten, and that
/// is no reason to refuse a store that opened fine.
fn restrict_to_owner(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mut perms = metadata.permissions();
    if perms.mode() & 0o777 != mode {
        perms.set_mode(mode);
        let _ = fs::set_permissions(path, perms);
    }
}

impl Store {
    pub fn open(db_path: &Path, data_dir: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let store = Self::init(conn, data_dir)?;
        // Every message body is in here in plaintext, and SQLite creates the
        // file at whatever the umask allows -- `0644` by default.
        restrict_to_owner(db_path);
        // `-wal` and `-shm` hold recently written mail until a checkpoint, and
        // are created on first write with the same umask.
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = db_path.as_os_str().to_owned();
            sidecar.push(suffix);
            restrict_to_owner(Path::new(&sidecar));
        }
        Ok(store)
    }

    pub fn open_in_memory(data_dir: &Path) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn, data_dir)
    }

    fn init(mut conn: Connection, data_dir: &Path) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // A checkpoint still takes the database briefly, and without a timeout
        // that surfaces as SQLITE_BUSY on an otherwise fine query.
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Self::bring_up_schema(&mut conn)?;
        let attachments = data_dir.join("attachments");
        fs::create_dir_all(&attachments)?;
        restrict_to_owner(data_dir);
        restrict_tree(&attachments);
        Ok(Store {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// How many migrations predate the framework. A database opened by a build
    /// before it carries `user_version = 0` *and* its tables already exist --
    /// those columns were added by the hand-rolled checks in
    /// [`Self::legacy_migrate`], which ran on every open. Stamping the count
    /// skips the baseline yet still hands the database to everything after it.
    const BASELINE_VERSION: i64 = 1;

    fn bring_up_schema(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let has_accounts: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'accounts'")?
            .exists([])?;
        if version == 0 && has_accounts {
            Self::legacy_migrate(conn)?;
            conn.pragma_update(None, "user_version", Self::BASELINE_VERSION)?;
        }
        migrations().to_latest(conn)?;
        // A daemon killed mid-delivery leaves its claim behind; releasing it
        // here is what makes that row retry instead of waiting forever.
        conn.execute(
            "UPDATE outbox SET state = 'queued' WHERE state = 'sending'",
            [],
        )?;
        Ok(())
    }

    /// Only for databases opened by a build before the migration framework,
    /// and only their *shape*: the columns that predate [`SCHEMA`]'s current
    /// form, checked via `PRAGMA table_info` rather than by ignoring a
    /// duplicate-column error, so a locked or corrupt database is not
    /// swallowed too. Data repairs are migration 2, which these databases
    /// still receive once [`Self::BASELINE_VERSION`] has been stamped.
    fn legacy_migrate(conn: &Connection) -> Result<()> {
        let has_reply_to: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'reply_to_addrs'")?
            .exists([])?;
        if !has_reply_to {
            conn.execute("ALTER TABLE messages ADD COLUMN reply_to_addrs TEXT", [])?;
        }

        let has_bcc: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'bcc_addrs'")?
            .exists([])?;
        if !has_bcc {
            conn.execute("ALTER TABLE messages ADD COLUMN bcc_addrs TEXT", [])?;
        }

        let has_from_name: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'from_name'")?
            .exists([])?;
        if !has_from_name {
            conn.execute("ALTER TABLE messages ADD COLUMN from_name TEXT", [])?;
        }

        let has_special_use: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('folders') WHERE name = 'special_use'")?
            .exists([])?;
        if !has_special_use {
            conn.execute("ALTER TABLE folders ADD COLUMN special_use TEXT", [])?;
        }

        // NULL for everything already synced: previews come from a fetch that
        // only runs for new UIDs, and are never backfilled.
        let has_preview: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'preview'")?
            .exists([])?;
        if !has_preview {
            conn.execute("ALTER TABLE messages ADD COLUMN preview TEXT", [])?;
        }

        // NULL means "never synced with CONDSTORE", read as "reconcile
        // everything" -- the safe direction.
        let has_modseq: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('folders') WHERE name = 'highest_modseq'")?
            .exists([])?;
        if !has_modseq {
            conn.execute("ALTER TABLE folders ADD COLUMN highest_modseq INTEGER", [])?;
        }

        // FTS5 has no `ALTER TABLE ... ADD COLUMN`, so widening means dropping
        // and rebuilding. Cheap: everything it reads is already local.
        let has_from_name: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages_fts') WHERE name = 'from_name'")?
            .exists([])?;
        if !has_from_name {
            conn.execute("DROP TABLE IF EXISTS messages_fts", [])?;
            conn.execute(
                "CREATE VIRTUAL TABLE messages_fts USING fts5(subject, from_addr, from_name, snippet)",
                [],
            )?;
            conn.execute(
                "INSERT INTO messages_fts (rowid, subject, from_addr, from_name, snippet)
                 SELECT m.id, m.subject, m.from_addr, m.from_name,
                        COALESCE(substr(b.text_body, 1, 200), '')
                 FROM messages m
                 LEFT JOIN message_bodies b ON b.message_id = m.id",
                [],
            )?;
        }
        Ok(())
    }

    pub fn insert_account(&self, account: &NewAccount<'_>) -> Result<AccountId> {
        self.conn.execute(
            "INSERT INTO accounts (
                display_name, email, imap_host, imap_port, imap_security,
                smtp_host, smtp_port, smtp_security, username, keyring_ref, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, unixepoch())",
            params![
                account.display_name,
                account.email,
                account.imap_host,
                account.imap_port,
                account.imap_security.as_str(),
                account.smtp_host,
                account.smtp_port,
                account.smtp_security.as_str(),
                account.username,
                account.keyring_ref,
            ],
        )?;
        Ok(AccountId(self.conn.last_insert_rowid()))
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, display_name, email, imap_host, imap_port, imap_security,
                    smtp_host, smtp_port, smtp_security, username, keyring_ref
             FROM accounts ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Account {
                    id: AccountId(row.get(0)?),
                    display_name: row.get(1)?,
                    email: row.get(2)?,
                    imap_host: row.get(3)?,
                    imap_port: row.get(4)?,
                    imap_security: Security::from_str(&row.get::<_, String>(5)?),
                    smtp_host: row.get(6)?,
                    smtp_port: row.get(7)?,
                    smtp_security: Security::from_str(&row.get::<_, String>(8)?),
                    username: row.get(9)?,
                    keyring_ref: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn upsert_folder(&self, folder: &NewFolder<'_>) -> Result<FolderId> {
        self.conn.execute(
            "INSERT INTO folders (account_id, name, imap_path, delimiter, subscribed, special_use)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, imap_path) DO UPDATE SET
                name = excluded.name,
                delimiter = excluded.delimiter,
                subscribed = excluded.subscribed,
                special_use = excluded.special_use",
            params![
                folder.account_id.0,
                folder.name,
                folder.imap_path,
                folder.delimiter,
                folder.subscribed,
                folder.special_use.map(SpecialUse::as_str),
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND imap_path = ?2",
            params![folder.account_id.0, folder.imap_path],
            |row| row.get(0),
        )?;
        Ok(FolderId(id))
    }

    pub fn list_folders(&self, account_id: AccountId) -> Result<Vec<Folder>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, name, imap_path, delimiter, uid_validity, uid_next, subscribed, special_use, highest_modseq
             FROM folders WHERE account_id = ?1 ORDER BY imap_path",
        )?;
        let rows = stmt
            .query_map(params![account_id.0], folder_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_folder(&self, folder_id: FolderId) -> Result<Option<Folder>> {
        self.conn
            .query_row(
                "SELECT id, account_id, name, imap_path, delimiter, uid_validity, uid_next, subscribed, special_use, highest_modseq
                 FROM folders WHERE id = ?1",
                params![folder_id.0],
                folder_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Where sync starts its next incremental `UID FETCH` range.
    pub fn max_uid(&self, folder_id: FolderId) -> Result<Option<u32>> {
        self.conn
            .query_row(
                "SELECT MAX(uid) FROM messages WHERE folder_id = ?1",
                params![folder_id.0],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn message_id_for_uid(&self, folder_id: FolderId, uid: u32) -> Result<Option<MessageId>> {
        self.conn
            .query_row(
                "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
                params![folder_id.0, uid],
                |row| row.get(0).map(MessageId),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Records only. A caller changing `uid_validity` is expected to have
    /// already wiped the folder's cached messages.
    pub fn set_folder_modseq(&self, folder_id: FolderId, modseq: Option<u64>) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET highest_modseq = ?2 WHERE id = ?1",
            params![folder_id.0, modseq.map(|m| m as i64)],
        )?;
        Ok(())
    }

    /// Keeps the folder's id and everything cached under it, which is what
    /// makes a server-side rename cheap rather than a full re-download.
    pub fn rename_folder(&self, folder_id: FolderId, imap_path: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET imap_path = ?2, name = ?3 WHERE id = ?1",
            params![folder_id.0, imap_path, name],
        )?;
        Ok(())
    }

    pub fn set_folder_uid_state(
        &self,
        folder_id: FolderId,
        uid_validity: u32,
        uid_next: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET uid_validity = ?1, uid_next = ?2, last_synced_at = unixepoch() WHERE id = ?3",
            params![uid_validity, uid_next, folder_id.0],
        )?;
        Ok(())
    }

    /// **`messages_fts` does not cascade** -- FTS5 virtual tables take no
    /// foreign keys -- so its rows are deleted explicitly first, or search
    /// keeps returning hits for mail that no longer exists.
    pub fn delete_folder(&mut self, folder_id: FolderId) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM messages_fts WHERE rowid IN (SELECT id FROM messages WHERE folder_id = ?1)",
            params![folder_id.0],
        )?;
        tx.execute("DELETE FROM folders WHERE id = ?1", params![folder_id.0])?;
        tx.commit()?;
        Ok(())
    }

    /// For a `UIDVALIDITY` change, where every cached uid is meaningless.
    /// Deletes the FTS rows too -- they have no foreign key to cascade from,
    /// the same trap as [`Store::delete_folder`].
    pub fn clear_folder_messages(&self, folder_id: FolderId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages_fts WHERE rowid IN (SELECT id FROM messages WHERE folder_id = ?1)",
            params![folder_id.0],
        )?;
        self.conn.execute(
            "DELETE FROM messages WHERE folder_id = ?1",
            params![folder_id.0],
        )?;
        Ok(())
    }

    /// For reconciling: a uid the server no longer lists was deleted or moved,
    /// and nothing else in the sync notices that.
    pub fn message_uids(&self, folder_id: FolderId) -> Result<Vec<(MessageId, u32)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, uid FROM messages WHERE folder_id = ?1")?;
        let rows = stmt
            .query_map(params![folder_id.0], |row| {
                Ok((MessageId(row.get(0)?), row.get::<_, i64>(1)? as u32))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Blobs on disk are deliberately left: other messages may share the same
    /// content-addressed file. [`Store::sweep_attachment_cache`] collects them.
    pub fn delete_message(&self, message_id: MessageId) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE id = ?1", params![message_id.0])?;
        self.conn.execute(
            "DELETE FROM messages_fts WHERE rowid = ?1",
            params![message_id.0],
        )?;
        Ok(())
    }

    /// Separate from [`Store::upsert_message_envelope`] because the preview
    /// comes from a truncated `BODY.PEEK[TEXT]`, not the header block.
    pub fn set_message_preview(&self, message_id: MessageId, preview: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET preview = ?2 WHERE id = ?1",
            params![message_id.0, preview],
        )?;
        Ok(())
    }

    pub fn upsert_message_envelope(
        &self,
        account_id: AccountId,
        folder_id: FolderId,
        uid: u32,
        parsed: &ParsedMessage,
        flags: MessageFlags,
    ) -> Result<MessageId> {
        let from_addr = parsed.from.first().map(|m| m.address.clone());
        let from_name = parsed.from.first().and_then(|m| m.name.clone());
        let to_addrs = join_addrs(&parsed.to);
        let cc_addrs = join_addrs(&parsed.cc);
        let reply_to_addrs = join_addrs(&parsed.reply_to);
        let bcc_addrs = join_addrs(&parsed.bcc);
        let refs_header = if parsed.references.is_empty() {
            None
        } else {
            Some(parsed.references.join(" "))
        };
        let in_reply_to = parsed.in_reply_to.first().cloned();

        self.conn.execute(
            "INSERT INTO messages (
                account_id, folder_id, uid, message_id_header, in_reply_to, refs_header,
                subject, from_addr, from_name, to_addrs, cc_addrs, reply_to_addrs, date, has_attachments,
                flag_seen, flag_flagged, flag_answered, flag_deleted, flag_draft, bcc_addrs
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
             ON CONFLICT(folder_id, uid) DO UPDATE SET
                message_id_header = excluded.message_id_header,
                in_reply_to = excluded.in_reply_to,
                refs_header = excluded.refs_header,
                subject = excluded.subject,
                from_addr = excluded.from_addr,
                from_name = excluded.from_name,
                to_addrs = excluded.to_addrs,
                cc_addrs = excluded.cc_addrs,
                reply_to_addrs = excluded.reply_to_addrs,
                bcc_addrs = excluded.bcc_addrs,
                date = excluded.date,
                flag_seen = excluded.flag_seen,
                flag_flagged = excluded.flag_flagged,
                flag_answered = excluded.flag_answered,
                flag_deleted = excluded.flag_deleted,
                flag_draft = excluded.flag_draft",
            params![
                account_id.0,
                folder_id.0,
                uid,
                parsed.message_id,
                in_reply_to,
                refs_header,
                parsed.subject,
                from_addr,
                from_name,
                to_addrs,
                cc_addrs,
                reply_to_addrs,
                parsed.date,
                // A placeholder, and excluded from the ON CONFLICT update so a
                // re-upsert cannot clobber the real value. With no body,
                // `mail-parser` reports a multipart's absent content as one
                // attachment -- 76% of a real mailbox flagged as having files.
                false,
                flags.seen,
                flags.flagged,
                flags.answered,
                flags.deleted,
                flags.draft,
                bcc_addrs,
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
            params![folder_id.0, uid],
            |row| row.get(0),
        )?;
        let message_id = MessageId(id);

        // FTS5 has no UPSERT. `INSERT OR REPLACE` evaluates its VALUES before
        // the delete, so the snippet subquery still sees the old row.
        self.conn.execute(
            "INSERT OR REPLACE INTO messages_fts (rowid, subject, from_addr, from_name, snippet)
             VALUES (?1, ?2, ?3, ?4, COALESCE((SELECT snippet FROM messages_fts WHERE rowid = ?1), ''))",
            params![message_id.0, parsed.subject, from_addr, from_name],
        )?;

        Ok(message_id)
    }

    pub fn set_flags(&self, message_id: MessageId, flags: MessageFlags) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET flag_seen=?1, flag_flagged=?2, flag_answered=?3, flag_deleted=?4, flag_draft=?5
             WHERE id = ?6",
            params![
                flags.seen,
                flags.flagged,
                flags.answered,
                flags.deleted,
                flags.draft,
                message_id.0
            ],
        )?;
        Ok(())
    }

    /// One grouped query rather than a `count_messages` per folder: the sidebar
    /// needs this on every refresh, and 30 folders would mean 30 round trips
    /// through the store mutex the sync engine is also contending for.
    ///
    /// Folders with nothing unread are absent rather than zero.
    pub fn unread_counts(&self) -> Result<Vec<(FolderId, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT folder_id, COUNT(*) FROM messages WHERE flag_seen = 0 GROUP BY folder_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((FolderId(row.get(0)?), row.get::<_, i64>(1)? as u32))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn count_messages(&self, folder_ids: &[FolderId]) -> Result<(u32, u32)> {
        if folder_ids.is_empty() {
            return Ok((0, 0));
        }
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN flag_seen = 0 THEN 1 ELSE 0 END), 0)
             FROM messages WHERE folder_id IN ({})",
            placeholders(folder_ids.len())
        );
        let (total, unread): (i64, i64) = self.conn.query_row(
            &sql,
            params_from_iter(folder_ids.iter().map(|id| id.0)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((total as u32, unread as u32))
    }

    /// One row per `message_id_header`: Gmail mirrors one message into INBOX,
    /// All Mail and Important as labels, so fetching per row triples the
    /// network. Rows with no `Message-ID` fall back to their row id rather than
    /// collapsing together.
    pub fn messages_missing_bodies(
        &self,
        folder_id: FolderId,
        since: i64,
        limit: u32,
    ) -> Result<Vec<(MessageId, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, uid FROM messages m
             WHERE m.folder_id = ?1 AND m.body_fetched = 0 AND m.date >= ?2
               -- Skip anything whose twin in another folder already has a
               -- body: Gmail mirrors one message into INBOX, All Mail and
               -- Important as labels, and `copy_body_to_siblings` shares the
               -- download between them.
               AND NOT EXISTS (
                   SELECT 1 FROM messages sib
                   WHERE sib.id != m.id
                     AND sib.body_fetched = 1
                     AND sib.message_id_header IS NOT NULL
                     AND sib.message_id_header = m.message_id_header
               )
             ORDER BY m.date DESC, m.id DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![folder_id.0, since, limit], |row| {
                Ok((MessageId(row.get(0)?), row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Matches on `Message-ID` only: a row without one is left alone rather
    /// than risking a wrong match.
    pub fn copy_body_to_siblings(&self, message_id: MessageId) -> Result<usize> {
        let copied = self.conn.execute(
            "INSERT OR REPLACE INTO message_bodies (message_id, text_body, html_body, cached_at)
             SELECT sib.id, b.text_body, b.html_body, unixepoch()
             FROM messages m
             JOIN message_bodies b ON b.message_id = m.id
             JOIN messages sib
               ON sib.message_id_header = m.message_id_header
              AND sib.id != m.id
              AND sib.body_fetched = 0
             WHERE m.id = ?1 AND m.message_id_header IS NOT NULL",
            params![message_id.0],
        )?;
        self.conn.execute(
            "UPDATE messages SET body_fetched = 1
             WHERE message_id_header IS NOT NULL
               AND message_id_header = (SELECT message_id_header FROM messages WHERE id = ?1)",
            params![message_id.0],
        )?;
        Ok(copied)
    }

    pub fn count_missing_bodies(&self, since: i64) -> Result<u32> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE body_fetched = 0 AND date >= ?1",
            params![since],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn list_messages_page(
        &self,
        folder_ids: &[FolderId],
        after: Option<PageCursor>,
        limit: u32,
        filter: MessageFilter,
    ) -> Result<Vec<MessageSummary>> {
        if folder_ids.is_empty() {
            return Ok(Vec::new());
        }
        let unread = filter.sql();
        const COLUMNS: &str =
            "id, folder_id, uid, subject, from_addr, from_name, to_addrs, cc_addrs, reply_to_addrs,
             message_id_header, refs_header, date, has_attachments,
             flag_seen, flag_flagged, flag_answered, flag_deleted, flag_draft, body_fetched,
             preview, bcc_addrs";
        let folders = placeholders(folder_ids.len());
        // Cursor parameter indices depend on how many folders were bound.
        let next = folder_ids.len() + 1;
        let sql = match after {
            None => format!(
                "SELECT {COLUMNS} FROM messages WHERE folder_id IN ({folders}){unread}
                 ORDER BY date DESC, id DESC LIMIT ?{next}"
            ),
            Some(_) => format!(
                "SELECT {COLUMNS} FROM messages WHERE folder_id IN ({folders}){unread}
                   AND (date, id) < (?{}, ?{})
                 ORDER BY date DESC, id DESC LIMIT ?{next}",
                next + 1,
                next + 2
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let ids = folder_ids.iter().map(|id| id.0);
        let rows = match after {
            None => stmt
                .query_map(
                    params_from_iter(ids.chain([i64::from(limit)])),
                    message_summary_row,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?,
            Some(cursor) => stmt
                .query_map(
                    params_from_iter(ids.chain([i64::from(limit), cursor.date, cursor.id.0])),
                    message_summary_row,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// Full-text search across every folder, **newest first, not by relevance**
    /// -- BM25 still picks which copy of a duplicate survives dedup, but a
    /// relevance ordering interleaves last week with 2014. Note this changes
    /// what `LIMIT` keeps: the newest matches, not the best-scoring.
    ///
    /// Deduplicated by `message_id_header`, since IMAP has no cross-folder
    /// identity and Gmail models labels as folders. Rows without one are kept
    /// as-is rather than collapsed together.
    pub fn search(
        &self,
        query: &str,
        filter: MessageFilter,
        limit: u32,
    ) -> Result<Vec<MessageSummary>> {
        let Some(match_query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        // The same clauses the folder list uses. Ignoring them made the filter
        // buttons look broken: lit, but changing nothing.
        let narrow = filter.sql();
        let sql = format!(
            "WITH ranked AS (
                SELECT m.id, m.folder_id, m.uid, m.subject, m.from_addr, m.from_name, m.to_addrs, m.cc_addrs, m.reply_to_addrs,
                       m.message_id_header, m.refs_header, m.date, m.has_attachments,
                       m.flag_seen, m.flag_flagged, m.flag_answered, m.flag_deleted, m.flag_draft, m.body_fetched,
                       m.preview, m.bcc_addrs,
                       f.rank AS search_rank,
                       ROW_NUMBER() OVER (
                           PARTITION BY COALESCE(m.message_id_header, 'row-' || m.id)
                           ORDER BY f.rank
                       ) AS dedup_rank
                FROM messages_fts f
                JOIN messages m ON m.id = f.rowid
                WHERE messages_fts MATCH ?1{narrow}
             )
             SELECT id, folder_id, uid, subject, from_addr, from_name, to_addrs, cc_addrs, reply_to_addrs,
                    message_id_header, refs_header, date, has_attachments,
                    flag_seen, flag_flagged, flag_answered, flag_deleted, flag_draft, body_fetched,
                    preview, bcc_addrs
             FROM ranked
             WHERE dedup_rank = 1
             ORDER BY date DESC, id DESC
             LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![match_query, limit], message_summary_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Attachment contents go to disk, content-addressed under
    /// `<data_dir>/attachments/`, never into SQLite.
    pub fn store_message_body(
        &mut self,
        message_id: MessageId,
        parsed: &ParsedMessage,
    ) -> Result<()> {
        let snippet: String = parsed
            .text_body
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO message_bodies (message_id, text_body, html_body, cached_at)
             VALUES (?1, ?2, ?3, unixepoch())
             ON CONFLICT(message_id) DO UPDATE SET
                text_body = excluded.text_body, html_body = excluded.html_body, cached_at = excluded.cached_at",
            params![message_id.0, parsed.text_body, parsed.html_body],
        )?;
        // Settled here, not during envelope sync: it needs the real body, and
        // BODYSTRUCTURE can abort a whole folder's sync (see `birdman_imap::sync`).
        // Inline parts do not count -- a logo is not what a paperclip means.
        let has_attachments = parsed
            .attachments
            .iter()
            .any(|attachment| !attachment.is_inline);
        tx.execute(
            "UPDATE messages SET body_fetched = 1, has_attachments = ?2 WHERE id = ?1",
            params![message_id.0, has_attachments],
        )?;
        tx.execute(
            "UPDATE messages_fts SET snippet = ?2 WHERE rowid = ?1",
            params![message_id.0, snippet],
        )?;
        tx.commit()?;

        // Cleared first: storing a body is idempotent, inserting is not, and a
        // re-fetch used to append a fresh set of rows -- one message had the
        // same PDF eight times. Only the rows go; the blobs are
        // content-addressed and the re-insert finds them already there.
        self.conn.execute(
            "DELETE FROM attachments WHERE message_id = ?1",
            params![message_id.0],
        )?;
        for attachment in &parsed.attachments {
            self.insert_attachment(message_id, attachment)?;
        }
        Ok(())
    }

    /// Resolves a single id, which is what `birdman-backend` commands need to turn
    /// a store id into a mailbox and UID.
    pub fn get_message(&self, message_id: MessageId) -> Result<Option<MessageSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, uid, subject, from_addr, from_name, to_addrs, cc_addrs, reply_to_addrs,
                    message_id_header, refs_header, date, has_attachments,
                    flag_seen, flag_flagged, flag_answered, flag_deleted, flag_draft, body_fetched,
                    preview, bcc_addrs
             FROM messages WHERE id = ?1",
        )?;
        stmt.query_row(params![message_id.0], message_summary_row)
            .optional()
            .map_err(Into::into)
    }

    /// Falls back to a sibling copy's body. [`copy_body_to_siblings`] shares
    /// one at fetch time, but only with copies that existed *then* -- a label
    /// applied later has nothing, which showed up as a search result opening to
    /// "(no plaintext body)" while another row held the same mail cached.
    ///
    /// The sibling query runs only on a miss, and matches on `Message-ID` only.
    ///
    /// [`copy_body_to_siblings`]: Self::copy_body_to_siblings
    pub fn get_message_body(
        &self,
        message_id: MessageId,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let own = self
            .conn
            .query_row(
                "SELECT text_body, html_body FROM message_bodies WHERE message_id = ?1",
                params![message_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::from)?;
        if own.is_some() {
            return Ok(own);
        }
        self.conn
            .query_row(
                "SELECT b.text_body, b.html_body
                 FROM messages m
                 JOIN messages sib
                   ON sib.message_id_header = m.message_id_header
                  AND sib.id != m.id
                 JOIN message_bodies b ON b.message_id = sib.id
                 WHERE m.id = ?1 AND m.message_id_header IS NOT NULL
                 LIMIT 1",
                params![message_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Two rules, because the two trees mean different things.
    ///
    /// **Materialised copies** are a convenience and expire on use: untouched
    /// for [`MATERIALISED_TTL`] and they go, at the cost of one file copy.
    ///
    /// **Blobs** are the only copy of an attachment, so age says nothing -- a
    /// three-year-old message still has to open. They go only when no row
    /// references them, which means the message was deleted.
    ///
    /// Safe to be wrong in one direction only, and is: a blob deleted while
    /// still wanted is written back by `insert_attachment` on the next fetch.
    pub fn sweep_attachment_cache(&self) -> Result<SweepReport> {
        let mut report = SweepReport::default();
        let cutoff = std::time::SystemTime::now()
            .checked_sub(MATERIALISED_TTL)
            .unwrap_or(std::time::UNIX_EPOCH);

        if let Ok(entries) = fs::read_dir(self.data_dir.join("attachment-files")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let used = entry.metadata().and_then(|m| m.modified()).ok();
                // Left alone: keeping it costs a duplicate file, guessing wrong
                // deletes something in use.
                if used.is_some_and(|used| used < cutoff) {
                    report.bytes_reclaimed += directory_size(&path);
                    if fs::remove_dir_all(&path).is_ok() {
                        report.stale_copies += 1;
                    }
                }
            }
        }

        let mut referenced = std::collections::HashSet::new();
        let mut stmt = self.conn.prepare("SELECT cached_path FROM attachments")?;
        for path in stmt.query_map([], |row| row.get::<_, String>(0))? {
            referenced.insert(path?);
        }
        for blob in blobs_on_disk(&self.data_dir.join("attachments")) {
            if referenced.contains(&blob.to_string_lossy().into_owned()) {
                continue;
            }
            let size = fs::metadata(&blob).map(|m| m.len()).unwrap_or(0);
            if fs::remove_file(&blob).is_ok() {
                report.orphaned_blobs += 1;
                report.bytes_reclaimed += size;
            }
        }
        Ok(report)
    }

    /// Aggregated per call rather than kept in a table: a `contacts` table
    /// would be a second copy of what the messages already say, and the caller
    /// asks once per compose window rather than per keystroke.
    ///
    /// Names come from `from_name` alone -- `to`/`cc`/`bcc` are stored as bare
    /// addresses.
    pub fn contacts(&self, limit: u32) -> Result<Vec<Contact>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_addr, from_name, to_addrs, cc_addrs, bcc_addrs, date FROM messages",
        )?;
        let mut found: std::collections::HashMap<String, Contact> =
            std::collections::HashMap::new();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;

        for row in rows {
            let (from_addr, from_name, to, cc, bcc, date) = row?;
            let date = date.unwrap_or(0);
            if let Some(address) = from_addr {
                record(&mut found, &address, from_name, date);
            }
            for list in [to, cc, bcc].into_iter().flatten() {
                for address in list.split(',') {
                    record(&mut found, address, None, date);
                }
            }
        }

        let mut contacts: Vec<Contact> = found.into_values().collect();
        contacts.sort_by(|a, b| {
            b.seen
                .cmp(&a.seen)
                .then(b.last_seen.cmp(&a.last_seen))
                .then(a.address.cmp(&b.address))
        });
        contacts.truncate(limit as usize);
        Ok(contacts)
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// The message is durable here before anyone has tried to deliver it:
    /// from this moment the composed text cannot be lost to a crash, a
    /// network drop or a daemon restart.
    pub fn queue_outgoing(&self, account_id: AccountId, payload: &str) -> Result<OutboxEntry> {
        let now = Self::now();
        self.conn.execute(
            "INSERT INTO outbox (account_id, state, payload, attempts, next_attempt_at, created_at)
             VALUES (?1, 'queued', ?2, 0, ?3, ?3)",
            params![account_id.0, payload, now],
        )?;
        Ok(OutboxEntry {
            id: OutboxId(self.conn.last_insert_rowid()),
            account_id,
            state: OutboxState::Queued,
            payload: payload.to_string(),
            attempts: 0,
            last_error: None,
            next_attempt_at: now,
            created_at: now,
            sent_at: None,
        })
    }

    /// Everything worth showing, newest first. Sent rows age out of interest
    /// quickly but stay until swept, so a send's outcome is always checkable.
    pub fn list_outbox(&self) -> Result<Vec<OutboxEntry>> {
        self.list_outbox_where("")
    }

    /// Rows whose retry time has passed, oldest first: mail composed first
    /// should also go out first.
    pub fn due_outgoing(&self, now: i64) -> Result<Vec<OutboxEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, state, payload, attempts, last_error, next_attempt_at, created_at, sent_at
             FROM outbox
             WHERE state IN ('queued', 'failed') AND next_attempt_at <= ?1
             ORDER BY next_attempt_at, id",
        )?;
        let rows = stmt
            .query_map(params![now], outbox_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn list_outbox_where(&self, suffix: &str) -> Result<Vec<OutboxEntry>> {
        let sql = format!(
            "SELECT id, account_id, state, payload, attempts, last_error, next_attempt_at, created_at, sent_at
             FROM outbox {suffix} ORDER BY id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], outbox_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn update_outbox_state(
        &self,
        id: OutboxId,
        state: OutboxState,
        error: Option<&str>,
        attempts: u32,
        next_attempt_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET state = ?2, last_error = ?3, attempts = ?4, next_attempt_at = ?5 WHERE id = ?1",
            params![id.0, state.as_str(), error, attempts, next_attempt_at],
        )?;
        Ok(())
    }

    /// Claimed before delivery starts, so a worker that dies mid-send leaves
    /// a row that says exactly what it was doing rather than one a second
    /// worker would pick up again immediately.
    pub fn mark_outgoing_sending(&self, id: OutboxId) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE outbox SET state = 'sending' WHERE id = ?1 AND state IN ('queued', 'failed')",
            params![id.0],
        )?;
        Ok(changed > 0)
    }

    pub fn mark_outgoing_sent(&self, id: OutboxId) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET state = 'sent', sent_at = ?2, last_error = NULL WHERE id = ?1",
            params![id.0, Self::now()],
        )?;
        Ok(())
    }

    /// `retry_at` schedules the next try; the caller owns the backoff policy.
    pub fn mark_outgoing_failed(
        &self,
        entry: &OutboxEntry,
        error: &str,
        retry_at: i64,
    ) -> Result<()> {
        self.update_outbox_state(
            entry.id,
            OutboxState::Failed,
            Some(error),
            entry.attempts + 1,
            retry_at,
        )
    }

    /// A failed or stuck row goes back on the queue immediately. Returns
    /// false for a row that is not waiting -- including one already being
    /// delivered.
    pub fn retry_outgoing(&self, id: OutboxId) -> Result<bool> {
        let now = Self::now();
        let changed = self.conn.execute(
            "UPDATE outbox
                SET state = 'queued', attempts = 0, last_error = NULL, next_attempt_at = ?2
              WHERE id = ?1 AND state IN ('queued', 'failed')",
            params![id.0, now],
        )?;
        Ok(changed > 0)
    }

    pub fn sweep_sent_outbox(&self, before: i64) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM outbox WHERE state = 'sent' AND sent_at < ?1",
                params![before],
            )
            .map_err(StoreError::from)
    }

    /// Deleting rather than a `cancelled` state: nothing downstream reads
    /// history that no longer has a row, and the compose text lives with the
    /// client if it is wanted again.
    pub fn cancel_outgoing(&self, id: OutboxId) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM outbox WHERE id = ?1 AND state <> 'sending'",
            params![id.0],
        )?;
        Ok(changed > 0)
    }

    /// Reads only. Split from
    /// [`materialise_attachments`](Self::materialise_attachments) because
    /// copying files inside a query made every other read queue behind it --
    /// the client serialises queries on one connection.
    ///
    /// `path` is filled in for anything already on disk.
    pub fn attachments(&self, message_id: MessageId) -> Result<Vec<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT filename, content_type, size, cached_path FROM attachments
             WHERE message_id = ?1 AND is_inline = 0
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![message_id.0], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let dir = self.materialised_dir(message_id);
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(index, (filename, content_type, size))| {
                let filename = display_name(filename.as_deref(), content_type.as_deref(), index);
                let path = dir.join(&filename);
                let path = path.exists().then(|| path.to_string_lossy().into_owned());
                Attachment {
                    filename,
                    content_type,
                    size,
                    path,
                }
            })
            .collect())
    }

    /// The blob is content-addressed, which is right for storage and useless
    /// for anything leaving the app: a dragged file has to arrive called
    /// `invoice.pdf`.
    ///
    /// Per message, so two attachments with the same name cannot overwrite each
    /// other. **Copied, never hard linked** -- an editor writing back through a
    /// link would corrupt the original every other copy shares.
    pub fn materialise_attachments(&self, message_id: MessageId) -> Result<Vec<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT filename, content_type, size, cached_path FROM attachments
             WHERE message_id = ?1 AND is_inline = 0
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![message_id.0], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let dir = self.materialised_dir(message_id);
        let mut out = Vec::with_capacity(rows.len());
        for (index, (filename, content_type, size, blob)) in rows.into_iter().enumerate() {
            let filename = display_name(filename.as_deref(), content_type.as_deref(), index);
            let path = dir.join(&filename);
            if !path.exists() {
                fs::create_dir_all(&dir)?;
                restrict_to_owner(&dir);
                fs::copy(&blob, &path)?;
                restrict_to_owner(&path);
                mark_as_downloaded(&path);
            }
            out.push(Attachment {
                filename,
                content_type,
                size,
                path: Some(path.to_string_lossy().into_owned()),
            });
        }
        // Materialising *is* reopening, which is what "recently used" means to
        // the sweep.
        touch(&dir);
        Ok(out)
    }

    fn materialised_dir(&self, message_id: MessageId) -> PathBuf {
        self.data_dir
            .join("attachment-files")
            .join(message_id.0.to_string())
    }

    pub fn get_inline_attachments(&self, message_id: MessageId) -> Result<Vec<InlineAttachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT content_id, content_type, cached_path FROM attachments
             WHERE message_id = ?1 AND is_inline = 1 AND content_id IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(params![message_id.0], |row| {
                Ok(InlineAttachment {
                    content_id: row.get(0)?,
                    content_type: row.get(1)?,
                    cached_path: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn insert_attachment(
        &self,
        message_id: MessageId,
        attachment: &ParsedAttachment,
    ) -> Result<AttachmentId> {
        let hash = Sha256::digest(&attachment.contents);
        let hex = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let dir = self.data_dir.join("attachments").join(&hex[0..2]);
        fs::create_dir_all(&dir)?;
        restrict_to_owner(&dir);
        let path = dir.join(&hex);
        if !path.exists() {
            fs::write(&path, &attachment.contents)?;
            restrict_to_owner(&path);
        }

        self.conn.execute(
            "INSERT INTO attachments (message_id, filename, content_type, content_id, is_inline, size, cached_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message_id.0,
                attachment.filename,
                attachment.content_type,
                attachment.content_id,
                attachment.is_inline,
                attachment.contents.len() as i64,
                path.to_string_lossy(),
            ],
        )?;
        Ok(AttachmentId(self.conn.last_insert_rowid()))
    }
}

fn outbox_row(row: &rusqlite::Row) -> rusqlite::Result<OutboxEntry> {
    Ok(OutboxEntry {
        id: OutboxId(row.get(0)?),
        account_id: AccountId(row.get(1)?),
        state: OutboxState::from_str(&row.get::<_, String>(2)?),
        payload: row.get(3)?,
        attempts: row.get::<_, i64>(4)? as u32,
        last_error: row.get(5)?,
        next_attempt_at: row.get(6)?,
        created_at: row.get(7)?,
        sent_at: row.get(8)?,
    })
}

/// The row mapper reads these **by index**, so the order here and the order in
/// every projection using it must match:
/// `id, account_id, name, imap_path, delimiter, uid_validity, uid_next,
/// subscribed, special_use, highest_modseq`.
fn folder_row(row: &rusqlite::Row) -> rusqlite::Result<Folder> {
    let special_use: Option<String> = row.get(8)?;
    let modseq: Option<i64> = row.get(9)?;
    Ok(Folder {
        id: FolderId(row.get(0)?),
        account_id: AccountId(row.get(1)?),
        name: row.get(2)?,
        imap_path: row.get(3)?,
        delimiter: row.get(4)?,
        uid_validity: row.get(5)?,
        uid_next: row.get(6)?,
        highest_modseq: modseq.map(|m| m as u64),
        subscribed: row.get(7)?,
        special_use: special_use.and_then(|s| SpecialUse::parse(&s)),
    })
}

/// Raw text cannot go to `MATCH` directly.
///
/// **Prefixes.** FTS5 matches whole tokens, so a half-typed word found nothing
/// while the whole word matched. Each token gets a `*`.
///
/// **Punctuation is syntax.** `MATCH` takes a query *language* -- a quote, a
/// colon, a bare `-` and the word `OR` all mean something, and an unparseable
/// one is an error rather than no results. Splitting on non-alphanumerics and
/// re-quoting each token means nothing typed is ever read as syntax, and the
/// tokens are alphanumeric by construction so there is no quote left to escape.
fn fts_query(text: &str) -> Option<String> {
    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        // Quoted so it is a string, starred so it is a prefix. Space-joined,
        // which FTS5 reads as AND.
        .map(|token| format!("\"{token}\"*"))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

/// A sender-chosen filename is untrusted input the moment it reaches the
/// filesystem, which materialising an attachment is the only thing that does.
/// Not theoretical: document scanners produce names shaped like
/// `9876543210 / 20240117 121339.PDF`, and `dir.join()` on that silently writes
/// into a subdirectory.
///
/// - **Last component only**, on both separators -- a Windows sender's `\` is
///   not a separator here and would otherwise survive into a filename.
/// - **No control characters or bidi overrides.** `U+202E` is how
///   `invoice<RLO>fdp.exe` displays as `invoiceexe.pdf`.
/// - **No `.` or `..`**, which name directories.
/// - **Bounded length**: the limit is per component, and an over-long name
///   fails the write rather than truncating.
///
/// `None` when nothing usable is left; the caller falls back to the content
/// hash, which is always a valid name.
pub fn safe_attachment_name(filename: &str) -> Option<String> {
    let last = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return None;
    }
    // Bytes, not characters: the limit is 255 bytes, and the cut still has to
    // land on a character boundary.
    const MAX: usize = 200;
    let mut end = cleaned.len().min(MAX);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    Some(cleaned[..end].to_string())
}

fn blobs_on_disk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(shards) = fs::read_dir(root) else {
        return found;
    };
    for shard in shards.flatten() {
        let Ok(entries) = fs::read_dir(shard.path()) else {
            continue;
        };
        found.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_file()),
        );
    }
    found
}

fn directory_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// How long a materialised copy outlives its last use. Reopening resets it.
pub const MATERIALISED_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub stale_copies: usize,
    pub orphaned_blobs: usize,
    pub bytes_reclaimed: u64,
}

/// `mtime`, not `atime`: `relatime` mounts will not update an access time that
/// is already recent, which is precisely the update this needs.
fn touch(path: &Path) {
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let now = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
    ];
    // SAFETY: `path` is a valid C string for the call, and `now` is the
    // two-element array `utimensat` expects.
    unsafe {
        libc::utimensat(libc::AT_FDCWD, path.as_ptr(), now.as_ptr(), 0);
    }
}

/// Shared by the metadata and materialising paths so the two cannot disagree
/// about where a file is.
fn display_name(filename: Option<&str>, content_type: Option<&str>, index: usize) -> String {
    filename
        .and_then(safe_attachment_name)
        .unwrap_or_else(|| format!("attachment-{}{}", index + 1, extension_for(content_type)))
}

/// A short list rather than a mime database: an unknown type with no extension
/// is a better answer than a wrong one.
fn extension_for(content_type: Option<&str>) -> &'static str {
    match content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "application/pdf" => ".pdf",
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "text/plain" => ".txt",
        "text/html" => ".html",
        "text/calendar" => ".ics",
        "application/zip" => ".zip",
        _ => "",
    }
}

/// Gatekeeper reads `com.apple.quarantine` when a file is opened, so an
/// executable that arrived by mail gets a confirmation dialog instead of
/// running. Nothing applies it for us -- we write these files ourselves, and
/// the whole point of materialising is that they leave the app.
///
/// Best-effort: no extended attributes is not a reason to hide an attachment.
#[cfg(target_os = "macos")]
fn mark_as_downloaded(path: &Path) {
    // flags;timestamp;agent;uuid. `0081` is QTN_FLAG_DOWNLOAD with
    // QTN_FLAG_USER_APPROVED unset -- what Safari and Mail write.
    let value = b"0081;00000000;Birdman;\x00";
    let name = c"com.apple.quarantine";
    let path_c = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(path_c) => path_c,
        Err(_) => return,
    };
    // SAFETY: both pointers are valid for the given length, and `setxattr`
    // does not retain them.
    unsafe {
        libc::setxattr(
            path_c.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn mark_as_downloaded(_path: &Path) {}

/// Keyed on the lowercased address: `Alice@` and `alice@` are one contact. The
/// name comes from the most recent message carrying one.
fn record(
    found: &mut std::collections::HashMap<String, Contact>,
    address: &str,
    name: Option<String>,
    date: i64,
) {
    let address = address.trim();
    // Not a validity check -- plenty of real addresses look odd -- just enough
    // to keep list separators and empty fields out.
    if address.is_empty() || !address.contains('@') {
        return;
    }
    let key = address.to_ascii_lowercase();
    let entry = found.entry(key).or_insert_with(|| Contact {
        name: None,
        address: address.to_string(),
        seen: 0,
        last_seen: i64::MIN,
    });
    entry.seen += 1;
    if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
        if date >= entry.last_seen {
            entry.name = Some(name);
        }
    }
    entry.last_seen = entry.last_seen.max(date);
}

/// rusqlite binds one value per placeholder and has no array parameter without
/// the `rarray` feature.
fn placeholders(count: usize) -> String {
    (1..=count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Reads **by index**, so a projection feeding this must append rather than
/// reorder: `id, folder_id, uid, subject, from_addr, from_name, to_addrs,
/// cc_addrs, reply_to_addrs, message_id_header, refs_header, date,
/// has_attachments, flag_seen, flag_flagged, flag_answered, flag_deleted,
/// flag_draft, body_fetched`.
fn message_summary_row(row: &rusqlite::Row) -> rusqlite::Result<MessageSummary> {
    let refs_header: Option<String> = row.get(10)?;
    Ok(MessageSummary {
        id: MessageId(row.get(0)?),
        folder_id: FolderId(row.get(1)?),
        uid: row.get(2)?,
        subject: row.get(3)?,
        from_addr: row.get(4)?,
        from_name: row.get(5)?,
        to_addrs: row.get(6)?,
        cc_addrs: row.get(7)?,
        reply_to_addrs: row.get(8)?,
        message_id_header: row.get(9)?,
        references: refs_header
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        date: row.get(11)?,
        has_attachments: row.get(12)?,
        flags: MessageFlags {
            seen: row.get(13)?,
            flagged: row.get(14)?,
            answered: row.get(15)?,
            deleted: row.get(16)?,
            draft: row.get(17)?,
        },
        body_fetched: row.get(18)?,
        preview: row.get(19)?,
        // Appended last: the row mappers read by index, so adding a column
        // anywhere else silently shifts every field after it.
        bcc_addrs: row.get(20)?,
    })
}

fn join_addrs(addrs: &[birdman_mime::Mailbox]) -> Option<String> {
    if addrs.is_empty() {
        None
    } else {
        Some(
            addrs
                .iter()
                .map(|m| m.address.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    display_name TEXT NOT NULL,
    email TEXT NOT NULL,
    imap_host TEXT NOT NULL,
    imap_port INTEGER NOT NULL,
    imap_security TEXT NOT NULL,
    smtp_host TEXT NOT NULL,
    smtp_port INTEGER NOT NULL,
    smtp_security TEXT NOT NULL,
    username TEXT NOT NULL,
    keyring_ref TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    imap_path TEXT NOT NULL,
    delimiter TEXT,
    uid_validity INTEGER,
    uid_next INTEGER,
    highest_modseq INTEGER,
    last_synced_at INTEGER,
    subscribed INTEGER NOT NULL DEFAULT 1,
    special_use TEXT,
    UNIQUE(account_id, imap_path)
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    message_id_header TEXT,
    in_reply_to TEXT,
    refs_header TEXT,
    subject TEXT,
    from_addr TEXT,
    from_name TEXT,
    to_addrs TEXT,
    cc_addrs TEXT,
    reply_to_addrs TEXT,
    bcc_addrs TEXT,
    date INTEGER,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    flag_seen INTEGER NOT NULL DEFAULT 0,
    flag_flagged INTEGER NOT NULL DEFAULT 0,
    flag_answered INTEGER NOT NULL DEFAULT 0,
    flag_deleted INTEGER NOT NULL DEFAULT 0,
    flag_draft INTEGER NOT NULL DEFAULT 0,
    body_fetched INTEGER NOT NULL DEFAULT 0,
    preview TEXT,
    UNIQUE(folder_id, uid)
);
CREATE INDEX IF NOT EXISTS idx_messages_folder_date ON messages(folder_id, date DESC, id DESC);

CREATE TABLE IF NOT EXISTS message_bodies (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    text_body TEXT,
    html_body TEXT,
    cached_at INTEGER NOT NULL
);

-- JWZ-style threading (Message-ID/References -> parent/root) is built once
-- birdman-imap is producing real sync data (Phase 2+); this table just holds
-- the shape for it.
CREATE TABLE IF NOT EXISTS message_threads (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    parent_message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    root_message_id INTEGER REFERENCES messages(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS attachments (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename TEXT,
    content_type TEXT,
    content_id TEXT,
    is_inline INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL,
    cached_path TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    subject, from_addr, from_name, snippet
);
";

/// One-time data repairs, kept as a migration so they run exactly once per
/// database instead of on every open. Both fix states older builds could
/// leave behind; on a fresh database they have nothing to do.
///
/// Alternative body renderings filed as attachments before
/// `birdman_mime::is_alternative_body` existed -- only the unnamed ones,
/// matching the parser: a named `report.html` is a real attachment. And rows
/// that accumulated on every body re-fetch until `store_message_body` learned
/// to clear them first.
const DATA_REPAIRS_SQL: &str = "
DELETE FROM attachments
 WHERE is_inline = 0 AND filename IS NULL
   AND lower(content_type) IN ('text/html', 'text/x-amp-html', 'text/watch-html', 'text/plain');

UPDATE messages
   SET has_attachments = EXISTS (
           SELECT 1 FROM attachments a
            WHERE a.message_id = messages.id AND a.is_inline = 0
       )
 WHERE has_attachments <> EXISTS (
           SELECT 1 FROM attachments a
            WHERE a.message_id = messages.id AND a.is_inline = 0
       );

UPDATE messages SET has_attachments = 0 WHERE body_fetched = 0 AND has_attachments = 1;

DELETE FROM attachments WHERE id NOT IN (
    SELECT MIN(id) FROM attachments
    GROUP BY message_id, filename, size, content_id, is_inline
);
";

/// Mail queued for delivery. `payload` is a JSON `OutgoingMessage`, held
/// opaque here: the store sits below the crate that defines it.
const OUTBOX_SCHEMA: &str = "
CREATE TABLE outbox (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'queued',
    payload TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    next_attempt_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    sent_at INTEGER
);
CREATE INDEX idx_outbox_due ON outbox(state, next_attempt_at);
";

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        rusqlite_migration::M::up(SCHEMA),
        rusqlite_migration::M::up(DATA_REPAIRS_SQL),
        rusqlite_migration::M::up(OUTBOX_SCHEMA),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn opening_a_store_leaves_nothing_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mail.db");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::open(&db, dir.path()).unwrap();
        drop(store);

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&db), 0o600, "database");
        assert_eq!(mode(dir.path()), 0o700, "data directory");
        assert_eq!(
            mode(&dir.path().join("attachments")),
            0o700,
            "attachment cache"
        );
    }

    #[test]
    fn a_store_opened_by_an_older_build_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mail.db");
        drop(Store::open(&db, dir.path()).unwrap());

        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        drop(Store::open(&db, dir.path()).unwrap());
        assert_eq!(
            fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    use super::*;
    use birdman_mime::Mailbox;

    fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(dir.path()).unwrap();
        (store, dir)
    }

    fn sample_message(subject: &str, from: &str, date: i64) -> ParsedMessage {
        ParsedMessage {
            subject: Some(subject.to_string()),
            from: vec![Mailbox {
                name: None,
                address: from.to_string(),
            }],
            date: Some(date),
            text_body: Some(format!("body of {subject}")),
            ..Default::default()
        }
    }

    #[test]
    fn account_and_folder_round_trip() {
        let (store, _dir) = test_store();
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Test",
                email: "test@example.com",
                imap_host: "imap.example.com",
                imap_port: 993,
                imap_security: Security::Tls,
                smtp_host: "smtp.example.com",
                smtp_port: 587,
                smtp_security: Security::StartTls,
                username: "test@example.com",
                keyring_ref: "account:1",
            })
            .unwrap();

        let accounts = store.list_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].email, "test@example.com");

        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: Some("/"),
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let folders = store.list_folders(account_id).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, folder_id);
    }

    #[test]
    fn keyset_pagination_returns_newest_first_without_gaps_or_dupes() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        for uid in 1..=25u32 {
            let parsed = sample_message(&format!("msg {uid}"), "a@example.com", uid as i64);
            store
                .upsert_message_envelope(
                    account_id,
                    folder_id,
                    uid,
                    &parsed,
                    MessageFlags::default(),
                )
                .unwrap();
        }

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = store
                .list_messages_page(
                    &[folder_id],
                    cursor,
                    10,
                    MessageFilter {
                        unread: false,
                        ..Default::default()
                    },
                )
                .unwrap();
            if page.is_empty() {
                break;
            }
            for m in &page {
                seen.push(m.uid);
            }
            let last = page.last().unwrap();
            cursor = Some(PageCursor {
                date: last.date.unwrap(),
                id: last.id,
            });
        }

        assert_eq!(seen.len(), 25);
        assert_eq!(seen, (1..=25u32).rev().collect::<Vec<_>>());
    }

    #[test]
    fn body_and_attachments_are_lazy() {
        let (mut store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        let mut parsed = sample_message("Hi", "a@example.com", 100);
        parsed.attachments.push(birdman_mime::Attachment {
            filename: Some("note.txt".into()),
            content_type: Some("text/plain".into()),
            content_id: None,
            is_inline: false,
            contents: b"hello attachment".to_vec(),
        });

        let message_id = store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();

        let page = store
            .list_messages_page(
                &[folder_id],
                None,
                10,
                MessageFilter {
                    unread: false,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!page[0].body_fetched);
        assert!(store.get_message_body(message_id).unwrap().is_none());

        store.store_message_body(message_id, &parsed).unwrap();

        let page = store
            .list_messages_page(
                &[folder_id],
                None,
                10,
                MessageFilter {
                    unread: false,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(page[0].body_fetched);
        let (text, _html) = store.get_message_body(message_id).unwrap().unwrap();
        assert_eq!(text.as_deref(), Some("body of Hi"));
    }

    #[test]
    fn renaming_a_folder_keeps_its_id_and_its_mail() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "Old Name",
                imap_path: "Old Name",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let parsed = sample_message("kept", "a@example.com", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();

        store
            .rename_folder(folder_id, "New Name", "New Name")
            .unwrap();

        let folder = store.get_folder(folder_id).unwrap().unwrap();
        assert_eq!(folder.imap_path, "New Name");
        assert_eq!(folder.id, folder_id, "same row, so nothing under it moved");
        assert_eq!(
            store
                .list_messages_page(
                    &[folder_id],
                    None,
                    10,
                    MessageFilter {
                        unread: false,
                        ..Default::default()
                    }
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn modseq_round_trips_and_clears() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        assert_eq!(
            store.get_folder(folder_id).unwrap().unwrap().highest_modseq,
            None
        );

        store
            .set_folder_modseq(folder_id, Some(90_060_115_205_545_359))
            .unwrap();
        assert_eq!(
            store.get_folder(folder_id).unwrap().unwrap().highest_modseq,
            Some(90_060_115_205_545_359),
            "must survive the i64 round trip -- real modseqs are large"
        );

        store.set_folder_modseq(folder_id, None).unwrap();
        assert_eq!(
            store.get_folder(folder_id).unwrap().unwrap().highest_modseq,
            None
        );
    }

    #[test]
    fn clearing_a_folder_takes_its_search_rows_with_it() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let parsed = sample_message("reissued", "a@example.com", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();
        assert_eq!(
            store
                .search("reissued", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );

        store.clear_folder_messages(folder_id).unwrap();

        assert!(
            store.get_folder(folder_id).unwrap().is_some(),
            "the folder itself survives"
        );
        assert_eq!(
            store
                .search("reissued", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn message_uids_lists_what_reconcile_compares_against() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        for uid in [4u32, 9, 11] {
            let parsed = sample_message(&format!("m{uid}"), "a@example.com", uid as i64);
            store
                .upsert_message_envelope(
                    account_id,
                    folder_id,
                    uid,
                    &parsed,
                    MessageFlags::default(),
                )
                .unwrap();
        }

        let mut uids: Vec<u32> = store
            .message_uids(folder_id)
            .unwrap()
            .into_iter()
            .map(|(_, uid)| uid)
            .collect();
        uids.sort_unstable();
        assert_eq!(uids, vec![4, 9, 11]);
    }

    #[test]
    fn deleting_a_folder_takes_its_messages_and_its_search_rows() {
        let (mut store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "Old",
                imap_path: "[Gmail]/Old",
                delimiter: Some("/"),
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let parsed = sample_message("findme", "a@example.com", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();
        assert_eq!(
            store
                .search("findme", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );

        store.delete_folder(folder_id).unwrap();

        assert!(store.get_folder(folder_id).unwrap().is_none());
        assert_eq!(
            store
                .list_messages_page(
                    &[folder_id],
                    None,
                    10,
                    MessageFilter {
                        unread: false,
                        ..Default::default()
                    }
                )
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .search("findme", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            0,
            "stale FTS rows would outlive the folder"
        );
    }

    #[test]
    fn unread_counts_reports_only_folders_with_unread() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let read_only = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "Sent",
                imap_path: "Sent",
                delimiter: None,
                subscribed: true,
                special_use: Some(SpecialUse::Sent),
            })
            .unwrap();

        for uid in 1..=5u32 {
            let parsed = sample_message(&format!("m{uid}"), "a@example.com", uid as i64);
            let flags = MessageFlags {
                seen: uid > 3,
                ..MessageFlags::default()
            };
            store
                .upsert_message_envelope(account_id, folder_id, uid, &parsed, flags)
                .unwrap();
        }
        let parsed = sample_message("sent", "a@example.com", 99);
        let seen = MessageFlags {
            seen: true,
            ..MessageFlags::default()
        };
        store
            .upsert_message_envelope(account_id, read_only, 1, &parsed, seen)
            .unwrap();

        let counts = store.unread_counts().unwrap();
        assert_eq!(
            counts,
            vec![(folder_id, 3)],
            "a fully-read folder should be absent, not zero"
        );
    }

    #[test]
    fn unread_only_filters_and_still_pages() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        for uid in 1..=25u32 {
            let parsed = sample_message(&format!("msg {uid}"), "a@example.com", uid as i64);
            let flags = MessageFlags {
                seen: uid % 2 == 0,
                ..MessageFlags::default()
            };
            store
                .upsert_message_envelope(account_id, folder_id, uid, &parsed, flags)
                .unwrap();
        }

        let all = store
            .list_messages_page(
                &[folder_id],
                None,
                50,
                MessageFilter {
                    unread: false,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(all.len(), 25);

        let unread = store
            .list_messages_page(
                &[folder_id],
                None,
                50,
                MessageFilter {
                    unread: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(unread.len(), 13);
        assert!(unread.iter().all(|m| !m.flags.seen));

        let first = store
            .list_messages_page(
                &[folder_id],
                None,
                10,
                MessageFilter {
                    unread: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first.len(), 10);
        let last = first.last().unwrap();
        let next = store
            .list_messages_page(
                &[folder_id],
                Some(PageCursor {
                    date: last.date.unwrap(),
                    id: last.id,
                }),
                10,
                MessageFilter {
                    unread: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            next.len(),
            3,
            "the cursor must not skip or repeat filtered rows"
        );
        assert!(next.iter().all(|m| !m.flags.seen));
    }

    #[test]
    fn search_matches_subject_and_ranks_by_relevance() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        let parsed = sample_message("Quarterly report", "boss@example.com", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();
        let parsed2 = sample_message("Lunch plans", "friend@example.com", 2);
        store
            .upsert_message_envelope(account_id, folder_id, 2, &parsed2, MessageFlags::default())
            .unwrap();

        let results = store
            .search("quarterly", MessageFilter::default(), 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].subject.as_deref(), Some("Quarterly report"));
    }

    #[test]
    fn search_deduplicates_the_same_message_across_folders() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let inbox_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let all_mail_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "[Google Mail]/All Mail",
                imap_path: "[Google Mail]/All Mail",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        let mut parsed = sample_message("Quarterly report", "boss@example.com", 1);
        parsed.message_id = Some("<abc123@example.com>".to_string());

        store
            .upsert_message_envelope(account_id, inbox_id, 1, &parsed, MessageFlags::default())
            .unwrap();
        store
            .upsert_message_envelope(
                account_id,
                all_mail_id,
                55,
                &parsed,
                MessageFlags::default(),
            )
            .unwrap();

        let results = store
            .search("quarterly", MessageFilter::default(), 10)
            .unwrap();
        assert_eq!(
            results.len(),
            1,
            "same Message-ID in two folders should collapse to one search result"
        );
    }

    #[test]
    fn search_keeps_separate_messages_with_no_message_id_header() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        let a = sample_message("Quarterly numbers one", "boss@example.com", 1);
        let b = sample_message("Quarterly numbers two", "boss@example.com", 2);
        assert!(a.message_id.is_none());
        store
            .upsert_message_envelope(account_id, folder_id, 1, &a, MessageFlags::default())
            .unwrap();
        store
            .upsert_message_envelope(account_id, folder_id, 2, &b, MessageFlags::default())
            .unwrap();

        let results = store
            .search("quarterly", MessageFilter::default(), 10)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn filters_compose_rather_than_replace_each_other() {
        let (store, _dir) = test_store();
        let mut store = store;
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();

        for (uid, seen, attachment) in [
            (1u32, false, false),
            (2, false, true),
            (3, true, false),
            (4, true, true),
        ] {
            let mut parsed = sample_message(&format!("m{uid}"), "a@example.com", uid as i64);
            let flags = MessageFlags {
                seen,
                ..MessageFlags::default()
            };
            let id = store
                .upsert_message_envelope(account_id, folder_id, uid, &parsed, flags)
                .unwrap();
            if attachment {
                parsed.attachments.push(ParsedAttachment {
                    filename: Some("note.txt".into()),
                    content_type: Some("text/plain".into()),
                    content_id: None,
                    is_inline: false,
                    contents: b"x".to_vec(),
                });
                store.store_message_body(id, &parsed).unwrap();
            }
        }

        let count = |filter| {
            store
                .list_messages_page(&[folder_id], None, 10, filter)
                .unwrap()
                .len()
        };
        assert_eq!(
            count(MessageFilter::default()),
            4,
            "no filter means everything"
        );
        assert_eq!(
            count(MessageFilter {
                unread: true,
                attachments: false
            }),
            2
        );
        assert_eq!(
            count(MessageFilter {
                unread: false,
                attachments: true
            }),
            2
        );
        assert_eq!(
            count(MessageFilter {
                unread: true,
                attachments: true
            }),
            1
        );
    }

    #[test]
    fn search_matches_a_prefix_as_you_type() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let parsed = sample_message("Postbode weekly digest", "team@postbode.example", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();

        for typed in [
            "p",
            "postbo",
            "postbode",
            "Postbode",
            "postbode weekly",
            "weekly post",
        ] {
            assert_eq!(
                store
                    .search(typed, MessageFilter::default(), 10)
                    .unwrap()
                    .len(),
                1,
                "typing {typed:?} found nothing"
            );
        }
        assert!(
            store
                .search("postgres", MessageFilter::default(), 10)
                .unwrap()
                .is_empty(),
            "a prefix is not a fuzzy match"
        );
    }

    #[test]
    fn punctuation_in_a_search_is_text_not_syntax() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let parsed = sample_message("Your invoice", "billing@example.com", 1);
        store
            .upsert_message_envelope(account_id, folder_id, 1, &parsed, MessageFlags::default())
            .unwrap();

        for typed in [
            "it's",
            "\"unclosed",
            "a OR b",
            "-invoice",
            "subject:invoice",
            "*",
            "()",
        ] {
            assert!(
                store.search(typed, MessageFilter::default(), 10).is_ok(),
                "typing {typed:?} was an error"
            );
        }
        assert_eq!(
            store
                .search("invoice!", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search("  ...  ", MessageFilter::default(), 10)
            .unwrap()
            .is_empty());
    }

    /// The shape a document scanner produces: a reference number, a separator
    /// with spaces around it, then a timestamp. Seen in a real mailbox.
    #[test]
    fn a_separator_in_a_real_filename_cannot_reach_the_filesystem() {
        assert_eq!(
            safe_attachment_name("9876543210 / 20240117 121339.PDF").as_deref(),
            Some("20240117 121339.PDF")
        );
        assert_eq!(
            safe_attachment_name("../../.ssh/authorized_keys").as_deref(),
            Some("authorized_keys")
        );
        assert_eq!(
            safe_attachment_name(r"C:\\Users\\me\\evil.exe").as_deref(),
            Some("evil.exe")
        );
    }

    #[test]
    fn a_bidi_override_cannot_disguise_an_extension() {
        let disguised = "invoice\u{202e}fdp.exe";
        let safe = safe_attachment_name(disguised).unwrap();
        assert!(!safe.contains('\u{202e}'), "{safe}");
        assert!(
            safe.ends_with(".exe"),
            "the real extension stays visible: {safe}"
        );
    }

    #[test]
    fn names_that_are_not_names_fall_back_to_the_caller() {
        assert_eq!(safe_attachment_name(""), None);
        assert_eq!(safe_attachment_name("."), None);
        assert_eq!(safe_attachment_name(".."), None);
        assert_eq!(safe_attachment_name("/"), None);
        assert_eq!(safe_attachment_name("   "), None);
    }

    #[test]
    fn an_ordinary_filename_is_left_exactly_as_it_is() {
        for name in [
            // An ampersand and spaces; a `..` in the middle, from the `B.V.`
            // abbreviation running into the extension; a bare numeric name.
            "Voorjaar & Zomer Catalogus 2026 NL.pdf",
            "Herinnering_1234_Voorbeeldstraat_1_A_Example_Holding_B.V..pdf",
            "1000000001.jpg",
        ] {
            assert_eq!(safe_attachment_name(name).as_deref(), Some(name));
        }
    }

    #[test]
    fn an_over_long_name_is_cut_on_a_character_boundary() {
        let long = format!("{}.pdf", "é".repeat(300));
        let safe = safe_attachment_name(&long).unwrap();
        assert!(safe.len() <= 200, "{} bytes", safe.len());
        assert!(safe.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_sweep_keeps_what_is_in_use_and_drops_what_is_not() {
        use std::time::{Duration, SystemTime};
        let (store, dir) = test_store();
        let root = dir.path();

        let fresh = root.join("attachment-files").join("1");
        let stale = root.join("attachment-files").join("2");
        for d in [&fresh, &stale] {
            fs::create_dir_all(d).unwrap();
            fs::write(d.join("invoice.pdf"), b"pdf").unwrap();
        }
        let long_ago = SystemTime::now() - Duration::from_secs(14 * 24 * 60 * 60);
        let stale_c = std::ffi::CString::new(stale.as_os_str().as_encoded_bytes()).unwrap();
        let secs = long_ago
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        unsafe { libc::utimensat(libc::AT_FDCWD, stale_c.as_ptr(), times.as_ptr(), 0) };

        let shard = root.join("attachments").join("ab");
        fs::create_dir_all(&shard).unwrap();
        let orphan = shard.join("abdead");
        let kept = shard.join("abkeep");
        fs::write(&orphan, b"gone").unwrap();
        fs::write(&kept, b"here").unwrap();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        let message = store
            .upsert_message_envelope(
                account_id,
                folder_id,
                1,
                &sample_message("hi", "a@example.com", 1),
                MessageFlags::default(),
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO attachments (message_id, filename, content_type, content_id, is_inline, size, cached_path)
                 VALUES (?1, 'k.pdf', 'application/pdf', NULL, 0, 4, ?2)",
                params![message.0, kept.to_string_lossy()],
            )
            .unwrap();

        let report = store.sweep_attachment_cache().unwrap();
        assert_eq!(report.stale_copies, 1);
        assert_eq!(report.orphaned_blobs, 1);
        assert!(fresh.exists(), "a copy used recently stays");
        assert!(!stale.exists(), "one past its TTL goes");
        assert!(kept.exists(), "a referenced blob stays however old");
        assert!(!orphan.exists(), "one nothing references goes");
    }

    #[test]
    fn search_narrows_by_the_same_filter_the_list_uses() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder_id = store
            .upsert_folder(&NewFolder {
                account_id,
                name: "INBOX",
                imap_path: "INBOX",
                delimiter: None,
                subscribed: true,
                special_use: None,
            })
            .unwrap();
        for (uid, seen) in [(1u32, true), (2, false)] {
            let parsed = sample_message("quarterly report", "boss@example.com", uid as i64);
            let flags = MessageFlags {
                seen,
                ..MessageFlags::default()
            };
            store
                .upsert_message_envelope(account_id, folder_id, uid, &parsed, flags)
                .unwrap();
        }

        assert_eq!(
            store
                .search("quarterly", MessageFilter::default(), 10)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .search(
                    "quarterly",
                    MessageFilter {
                        unread: true,
                        ..Default::default()
                    },
                    10
                )
                .unwrap()
                .len(),
            1,
            "the unread filter has to reach search too"
        );
        assert!(
            store
                .search(
                    "quarterly",
                    MessageFilter {
                        attachments: true,
                        ..Default::default()
                    },
                    10
                )
                .unwrap()
                .is_empty(),
            "none of these carry one"
        );
    }

    fn insert_test_account(store: &Store) -> AccountId {
        store
            .insert_account(&NewAccount {
                display_name: "Test",
                email: "test@example.com",
                imap_host: "imap.example.com",
                imap_port: 993,
                imap_security: Security::Tls,
                smtp_host: "smtp.example.com",
                smtp_port: 587,
                smtp_security: Security::StartTls,
                username: "test@example.com",
                keyring_ref: "account:1",
            })
            .unwrap()
    }

    #[test]
    fn queued_mail_is_durable_and_lifecycle_complete() {
        let (store, _dir) = test_store();
        let account = insert_test_account(&store);

        let entry = store
            .queue_outgoing(account, r#"{"subject":"hi"}"#)
            .unwrap();
        assert_eq!(entry.state, OutboxState::Queued);

        let due = store.due_outgoing(i64::MAX).unwrap();
        assert_eq!(due.len(), 1, "a fresh row is immediately due");
        assert_eq!(due[0].id, entry.id);

        assert!(store.mark_outgoing_sending(entry.id).unwrap());
        assert!(
            store.due_outgoing(i64::MAX).unwrap().is_empty(),
            "claimed rows are not re-claimed"
        );

        store
            .mark_outgoing_failed(&entry, "smtp down", 999)
            .unwrap();
        let failed = store.list_outbox().unwrap();
        assert_eq!(failed[0].state, OutboxState::Failed);
        assert_eq!(failed[0].attempts, 1);
        assert_eq!(failed[0].last_error.as_deref(), Some("smtp down"));
        assert!(
            store.due_outgoing(998).unwrap().is_empty(),
            "not before its retry time"
        );
        assert!(
            store.due_outgoing(999).unwrap().len() == 1,
            "due at the retry time"
        );

        assert!(store.retry_outgoing(entry.id).unwrap());
        let retried = store.due_outgoing(i64::MAX).unwrap();
        assert_eq!(retried.len(), 1, "retry makes it immediately due");
        assert_eq!(retried[0].attempts, 0);
        assert_eq!(retried[0].last_error, None);

        store.mark_outgoing_sent(entry.id).unwrap();
        let sent = store.list_outbox().unwrap();
        assert_eq!(sent[0].state, OutboxState::Sent);
        assert!(sent[0].sent_at.is_some());
        assert!(
            store.due_outgoing(i64::MAX).unwrap().is_empty(),
            "sent mail is never redelivered"
        );
    }

    #[test]
    fn a_row_claimed_by_a_dead_worker_is_released_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mail.db");
        let account;
        {
            let store = Store::open(&db, dir.path()).unwrap();
            account = insert_test_account(&store);
            let entry = store.queue_outgoing(account, "{}").unwrap();
            assert!(store.mark_outgoing_sending(entry.id).unwrap());
        }
        let reopened = Store::open(&db, dir.path()).unwrap();
        let due = reopened.due_outgoing(i64::MAX).unwrap();
        assert_eq!(due.len(), 1, "the claim died with the process");
        assert_eq!(due[0].account_id, account);
    }

    #[test]
    fn cancel_removes_a_waiting_row_but_not_one_in_flight() {
        let (store, _dir) = test_store();
        let account = insert_test_account(&store);
        let queued = store.queue_outgoing(account, "{}").unwrap();
        assert!(store.cancel_outgoing(queued.id).unwrap());
        assert!(!store.mark_outgoing_sending(queued.id).unwrap());
        assert!(store.list_outbox().unwrap().is_empty());

        let sending = store.queue_outgoing(account, "{}").unwrap();
        assert!(store.mark_outgoing_sending(sending.id).unwrap());
        assert!(
            !store.cancel_outgoing(sending.id).unwrap(),
            "cannot cancel mid-flight"
        );
        assert_eq!(store.list_outbox().unwrap().len(), 1);
    }

    #[test]
    fn a_database_from_before_the_framework_upgrades_in_place() {
        // The shape `execute_batch(SCHEMA)` + hand-rolled ALTERs produced --
        // minus nothing: this is exactly what an old build left behind, at
        // user_version 0 with every historical column already in place.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mail.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE accounts (
                id INTEGER PRIMARY KEY, display_name TEXT NOT NULL, email TEXT NOT NULL,
                imap_host TEXT NOT NULL, imap_port INTEGER NOT NULL, imap_security TEXT NOT NULL,
                smtp_host TEXT NOT NULL, smtp_port INTEGER NOT NULL, smtp_security TEXT NOT NULL,
                username TEXT NOT NULL, keyring_ref TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);
             CREATE TABLE folders (
                id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, name TEXT NOT NULL,
                imap_path TEXT NOT NULL, delimiter TEXT, uid_validity INTEGER, uid_next INTEGER,
                last_synced_at INTEGER, subscribed INTEGER NOT NULL DEFAULT 1,
                UNIQUE(account_id, imap_path));
             CREATE TABLE messages (
                id INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, folder_id INTEGER NOT NULL,
                uid INTEGER NOT NULL, from_name TEXT, reply_to_addrs TEXT, bcc_addrs TEXT,
                preview TEXT, body_fetched INTEGER NOT NULL DEFAULT 0,
                has_attachments INTEGER NOT NULL DEFAULT 0, UNIQUE(folder_id, uid));
             CREATE TABLE attachments (
                id INTEGER PRIMARY KEY, message_id INTEGER NOT NULL, filename TEXT,
                content_type TEXT, content_id TEXT, is_inline INTEGER NOT NULL DEFAULT 0,
                size INTEGER NOT NULL, cached_path TEXT NOT NULL);
             CREATE VIRTUAL TABLE messages_fts USING fts5(subject, from_addr, from_name, snippet);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO accounts VALUES (1, 'Old', 'old@example.com', 'h', 993, 'tls', 'h', 465, 'tls', 'u', 'r', 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = Store::open(&db, dir.path()).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(
            version >= Store::BASELINE_VERSION,
            "stamped past the baseline, got {version}"
        );

        // The pre-framework rows survive, and new-schema features exist.
        assert_eq!(store.list_accounts().unwrap()[0].email, "old@example.com");
        store.queue_outgoing(AccountId(1), "{}").unwrap();
        assert_eq!(store.list_outbox().unwrap().len(), 1);
    }

    #[test]
    fn a_body_is_found_through_a_sibling_copy() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder = |name: &str| {
            store
                .upsert_folder(&NewFolder {
                    account_id,
                    name,
                    imap_path: name,
                    delimiter: None,
                    subscribed: true,
                    special_use: None,
                })
                .unwrap()
        };
        let inbox = folder("INBOX");
        let all_mail = folder("[Google Mail]/All Mail");

        let mut parsed = sample_message("Digest", "support@example.com", 1);
        parsed.message_id = Some("<shared@example.com>".to_string());
        let in_inbox = store
            .upsert_message_envelope(account_id, inbox, 1, &parsed, MessageFlags::default())
            .unwrap();
        let in_all_mail = store
            .upsert_message_envelope(account_id, all_mail, 55, &parsed, MessageFlags::default())
            .unwrap();

        let mut fetched = sample_message("Digest", "support@example.com", 1);
        fetched.message_id = Some("<shared@example.com>".to_string());
        fetched.text_body = Some("plain".to_string());
        fetched.html_body = Some("<p>rich</p>".to_string());
        let mut store = store;
        store.store_message_body(in_all_mail, &fetched).unwrap();

        let (text, html) = store
            .get_message_body(in_inbox)
            .unwrap()
            .expect("sibling body");
        assert_eq!(text.as_deref(), Some("plain"));
        assert_eq!(html.as_deref(), Some("<p>rich</p>"));
    }

    #[test]
    fn a_message_without_a_message_id_is_never_matched_to_a_sibling() {
        let (store, _dir) = test_store();
        let account_id = insert_test_account(&store);
        let folder = |name: &str| {
            store
                .upsert_folder(&NewFolder {
                    account_id,
                    name,
                    imap_path: name,
                    delimiter: None,
                    subscribed: true,
                    special_use: None,
                })
                .unwrap()
        };
        let inbox = folder("INBOX");
        let other = folder("Other");

        let orphan = store
            .upsert_message_envelope(
                account_id,
                inbox,
                1,
                &sample_message("A", "a@example.com", 1),
                MessageFlags::default(),
            )
            .unwrap();
        let unrelated = store
            .upsert_message_envelope(
                account_id,
                other,
                2,
                &sample_message("B", "b@example.com", 2),
                MessageFlags::default(),
            )
            .unwrap();
        let mut fetched = sample_message("B", "b@example.com", 2);
        fetched.text_body = Some("not yours".to_string());
        let mut store = store;
        store.store_message_body(unrelated, &fetched).unwrap();

        assert!(store.get_message_body(orphan).unwrap().is_none());
    }
}
