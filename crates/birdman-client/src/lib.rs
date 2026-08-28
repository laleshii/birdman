use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use birdman_backend::{Command, Outcome, OutgoingMessage};
use birdman_config::logging::Timed;
use birdman_proto::{
    Event, Frame, InlineAttachment, MessageBody, Query, Request, RequestKind, Response, WireResult,
};
use birdman_store::{AccountId, Folder, FolderId, MessageId, MessageSummary, PageCursor};

mod spawn;

pub use spawn::ensure_daemon;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not reach birdmand: {0}")]
    Transport(String),
    #[error("{0}")]
    Server(String),
    #[error("birdmand answered {asked} with something else")]
    Mismatch { asked: &'static str },
    #[error("birdmand speaks protocol {daemon}, this build speaks {client} -- run `birdman daemon restart`")]
    VersionMismatch { daemon: u32, client: u32 },
}

type Result<T> = std::result::Result<T, ClientError>;

pub type ClientFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send>>;

pub struct Client {
    socket: PathBuf,
    /// A reply belongs to the request written immediately before it, so one
    /// connection carries one request at a time. A second is opened only when
    /// every existing one is busy -- the daemon serves each connection strictly
    /// in order and says so itself: "A client wanting concurrency opens a
    /// second connection". Sharing one made every query wait for the slowest,
    /// which during a folder sync was 6s of queueing for a reply that took 1ms.
    conns: Mutex<Vec<Arc<Mutex<Connection>>>>,
    next_id: AtomicU64,
}

struct Connection {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Connection {
    fn open(socket: &Path) -> Result<Self> {
        let write = UnixStream::connect(socket)
            .map_err(|err| ClientError::Transport(format!("{}: {err}", socket.display())))?;
        let read = BufReader::new(
            write
                .try_clone()
                .map_err(|err| ClientError::Transport(err.to_string()))?,
        );
        let mut connection = Self { write, read };
        connection.hello()?;
        Ok(connection)
    }

    /// Every connection is independently versioned. Background requests and
    /// subscriptions use their own sockets, so checking only `Client::connect`
    /// would let those paths bypass skew detection after a daemon restart.
    fn hello(&mut self) -> Result<()> {
        let request = Request {
            id: 0,
            kind: RequestKind::Hello {
                version: birdman_proto::PROTOCOL_VERSION,
            },
        };
        let line = serde_json::to_string(&request)
            .map_err(|err| ClientError::Transport(format!("could not encode hello: {err}")))?;
        writeln!(self.write, "{line}")
            .and_then(|_| self.write.flush())
            .map_err(|err| ClientError::Transport(err.to_string()))?;

        let mut reply = String::new();
        if self
            .read
            .read_line(&mut reply)
            .map_err(|err| ClientError::Transport(err.to_string()))?
            == 0
        {
            return Err(ClientError::Transport(
                "birdmand closed the connection during the handshake".into(),
            ));
        }
        match serde_json::from_str::<Frame>(&reply) {
            Ok(Frame::Reply {
                id: 0,
                result: WireResult::Done,
            }) => Ok(()),
            Ok(Frame::Reply {
                id: 0,
                result: WireResult::VersionMismatch { daemon, client },
            }) => Err(ClientError::VersionMismatch { daemon, client }),
            Ok(Frame::Reply {
                id: 0,
                result: WireResult::Error(message),
            }) => Err(ClientError::Server(message)),
            Ok(_) => Err(ClientError::Mismatch { asked: "hello" }),
            Err(err) => Err(ClientError::Transport(format!(
                "unreadable handshake reply: {err}"
            ))),
        }
    }
}

impl Client {
    pub fn connect() -> Result<Self> {
        let socket = birdman_proto::socket_path(&birdman_config::data_dir());
        spawn::ensure_daemon(&socket)?;
        Ok(Self {
            conns: Mutex::new(vec![Arc::new(Mutex::new(Connection::open(&socket)?))]),
            socket,
            next_id: AtomicU64::new(1),
        })
    }

    pub fn shutdown(&self) -> Result<()> {
        match self.call(RequestKind::Shutdown)? {
            WireResult::Done => Ok(()),
            WireResult::Error(message) => Err(ClientError::Server(message)),
            _ => Err(ClientError::Mismatch { asked: "shutdown" }),
        }
    }

    pub fn is_running() -> bool {
        let socket = birdman_proto::socket_path(&birdman_config::data_dir());
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }

    pub fn socket_path() -> PathBuf {
        birdman_proto::socket_path(&birdman_config::data_dir())
    }

    /// Reconnects once on a transport failure: the daemon stops itself when
    /// idle, so an unused connection can legitimately be found closed.
    fn call(&self, kind: RequestKind) -> Result<WireResult> {
        match self.call_once(&kind) {
            Err(ClientError::Transport(_)) => {
                spawn::ensure_daemon(&self.socket)?;
                // The whole pool, not just this one: a transport error means the
                // daemon went away, which makes every connection to it stale. A
                // caller still holding one keeps it alive through its `Arc`.
                self.conns.lock().map_err(|_| poisoned())?.clear();
                self.call_once(&kind)
            }
            other => other,
        }
    }

    /// One connection is enough until it isn't. Opening a second costs a
    /// handshake, so it happens only when every existing one is in use.
    ///
    /// The cap bounds how many the daemon has to serve concurrently; past it a
    /// caller waits, which is what every caller did before.
    const MAX_CONNECTIONS: usize = 4;

    fn checkout(&self) -> Result<Arc<Mutex<Connection>>> {
        let existing = { self.conns.lock().map_err(|_| poisoned())?.clone() };
        for slot in &existing {
            // Dropped immediately: this only asks whether the connection is
            // free, and the caller re-locks it to use it. Losing the race means
            // waiting on a connection that was free a moment ago, not an error.
            if slot.try_lock().is_ok() {
                return Ok(slot.clone());
            }
        }
        if existing.len() < Self::MAX_CONNECTIONS {
            let slot = Arc::new(Mutex::new(Connection::open(&self.socket)?));
            let mut conns = self.conns.lock().map_err(|_| poisoned())?;
            // Re-checked: another thread may have grown the pool meanwhile, and
            // going over the cap is worse than dropping this one unused.
            if conns.len() < Self::MAX_CONNECTIONS {
                conns.push(slot.clone());
                return Ok(slot);
            }
        }
        existing.first().cloned().ok_or_else(|| {
            ClientError::Transport("the connection pool was emptied concurrently".into())
        })
    }

    fn call_once(&self, kind: &RequestKind) -> Result<WireResult> {
        let _timed = Timed::new(describe(kind), Timed::ROUND_TRIP);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = Request {
            id,
            kind: kind.clone(),
        };
        let line = serde_json::to_string(&request)
            .map_err(|err| ClientError::Transport(format!("could not encode request: {err}")))?;

        // Timed apart from the round trip above it. Every query shares this one
        // connection for the whole of its request and reply, so a slow one
        // delays the rest -- and until these were separated a queued query and
        // a slow one were indistinguishable in the log.
        let slot = self.checkout()?;
        let mut conn = {
            let _queued = Timed::new(format!("{} queued", describe(kind)), Timed::ROUND_TRIP);
            slot.lock().map_err(|_| poisoned())?
        };
        writeln!(conn.write, "{line}").map_err(|err| ClientError::Transport(err.to_string()))?;
        conn.write
            .flush()
            .map_err(|err| ClientError::Transport(err.to_string()))?;

        loop {
            let mut reply = String::new();
            let read = conn
                .read
                .read_line(&mut reply)
                .map_err(|err| ClientError::Transport(err.to_string()))?;
            if read == 0 {
                return Err(ClientError::Transport(
                    "birdmand closed the connection".into(),
                ));
            }
            match serde_json::from_str::<Frame>(&reply) {
                // Events can arrive here if something subscribed on this
                // connection; they are not what we are waiting for.
                Ok(Frame::Event(_)) => continue,
                Ok(Frame::Reply {
                    id: replied,
                    result,
                }) if replied == id || replied == 0 => return Ok(result),
                Ok(Frame::Reply { .. }) => continue,
                Err(err) => return Err(ClientError::Transport(format!("unreadable reply: {err}"))),
            }
        }
    }

    pub fn query(&self, query: Query) -> Result<Response> {
        match self.call(RequestKind::Query(query))? {
            WireResult::Response(response) => Ok(response),
            WireResult::Error(message) => Err(ClientError::Server(message)),
            _ => Err(ClientError::Mismatch { asked: "a query" }),
        }
    }

    pub fn accounts(&self) -> Result<Vec<birdman_store::Account>> {
        match self.query(Query::Accounts)? {
            Response::Accounts(accounts) => Ok(accounts),
            _ => Err(ClientError::Mismatch { asked: "accounts" }),
        }
    }

    pub fn folders(&self, account: Option<AccountId>) -> Result<Vec<Folder>> {
        match self.query(Query::Folders { account })? {
            Response::Folders(folders) => Ok(folders),
            _ => Err(ClientError::Mismatch { asked: "folders" }),
        }
    }

    pub fn unread_counts(&self) -> Result<Vec<(FolderId, u32)>> {
        match self.query(Query::UnreadCounts)? {
            Response::UnreadCounts(counts) => Ok(counts),
            _ => Err(ClientError::Mismatch {
                asked: "unread counts",
            }),
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
            _ => Err(ClientError::Mismatch { asked: "messages" }),
        }
    }

    pub fn message_counts(&self, folders: Vec<FolderId>) -> Result<(u32, u32)> {
        match self.query(Query::MessageCounts { folders })? {
            Response::MessageCounts { total, unread } => Ok((total, unread)),
            _ => Err(ClientError::Mismatch {
                asked: "message counts",
            }),
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
            _ => Err(ClientError::Mismatch { asked: "search" }),
        }
    }

    pub fn message(&self, message: MessageId) -> Result<Option<MessageSummary>> {
        match self.query(Query::Message { message })? {
            Response::Message(found) => Ok(found),
            _ => Err(ClientError::Mismatch { asked: "message" }),
        }
    }

    pub fn body(&self, message: MessageId) -> Result<Option<MessageBody>> {
        match self.query(Query::Body { message })? {
            Response::Body(body) => Ok(body),
            _ => Err(ClientError::Mismatch {
                asked: "message body",
            }),
        }
    }

    pub fn attachments(&self, message: MessageId) -> Result<Vec<birdman_store::Attachment>> {
        match self.query(Query::Attachments { message })? {
            Response::Attachments(attachments) => Ok(attachments),
            _ => Err(ClientError::Mismatch {
                asked: "attachments",
            }),
        }
    }

    /// Blocking form, on the shared connection. Fine for a caller with nothing
    /// else in flight; a UI wants the async form below instead.
    pub fn materialise_attachments_blocking(
        &self,
        message: MessageId,
    ) -> Result<Vec<birdman_store::Attachment>> {
        match self.query(Query::MaterialiseAttachments { message })? {
            Response::Attachments(attachments) => Ok(attachments),
            _ => Err(ClientError::Mismatch {
                asked: "attachments",
            }),
        }
    }

    /// On its own connection, not `query`: it copies files, and on the shared
    /// connection it would hold up the body read of this very message.
    pub fn materialise_attachments(
        &self,
        message: MessageId,
    ) -> ClientFuture<Vec<birdman_store::Attachment>> {
        self.off_thread(
            RequestKind::Query(Query::MaterialiseAttachments { message }),
            "attachments",
            |result| match result {
                WireResult::Response(Response::Attachments(attachments)) => Some(attachments),
                _ => None,
            },
        )
    }

    pub fn contacts(&self, limit: u32) -> Result<Vec<birdman_store::Contact>> {
        match self.query(Query::Contacts { limit })? {
            Response::Contacts(contacts) => Ok(contacts),
            _ => Err(ClientError::Mismatch { asked: "contacts" }),
        }
    }

    pub fn inline_attachments(&self, message: MessageId) -> Result<Vec<InlineAttachment>> {
        match self.query(Query::InlineAttachments { message })? {
            Response::InlineAttachments(attachments) => Ok(attachments),
            _ => Err(ClientError::Mismatch {
                asked: "inline attachments",
            }),
        }
    }

    pub fn sync_status(&self) -> Result<Vec<(AccountId, birdman_proto::SyncState)>> {
        match self.query(Query::SyncStatus)? {
            Response::SyncStatus(state) => Ok(state),
            _ => Err(ClientError::Mismatch {
                asked: "sync status",
            }),
        }
    }

    pub fn execute_blocking(&self, account: AccountId, command: Command) -> Result<Outcome> {
        match self.call(RequestKind::Execute { account, command })? {
            WireResult::Outcome { bodies_fetched } => Ok(Outcome { bodies_fetched }),
            WireResult::Error(message) => Err(ClientError::Server(message)),
            _ => Err(ClientError::Mismatch { asked: "a command" }),
        }
    }

    pub fn send_blocking(
        &self,
        account: AccountId,
        message: OutgoingMessage,
    ) -> Result<birdman_store::OutboxId> {
        match self.call(RequestKind::Send {
            account,
            message: Box::new(message),
        })? {
            WireResult::Queued { id } => Ok(birdman_store::OutboxId(id)),
            WireResult::Error(text) => Err(ClientError::Server(text)),
            _ => Err(ClientError::Mismatch { asked: "a send" }),
        }
    }

    pub fn outbox(&self) -> Result<Vec<birdman_store::OutboxEntry>> {
        match self.query(Query::Outbox)? {
            Response::Outbox(entries) => Ok(entries),
            _ => Err(ClientError::Mismatch { asked: "outbox" }),
        }
    }

    pub fn outbox_retry(&self, id: birdman_store::OutboxId) -> Result<bool> {
        match self.call(RequestKind::OutboxRetry { id })? {
            WireResult::Outbox { changed } => Ok(changed),
            WireResult::Error(text) => Err(ClientError::Server(text)),
            _ => Err(ClientError::Mismatch {
                asked: "an outbox retry",
            }),
        }
    }

    pub fn outbox_cancel(&self, id: birdman_store::OutboxId) -> Result<bool> {
        match self.call(RequestKind::OutboxCancel { id })? {
            WireResult::Outbox { changed } => Ok(changed),
            WireResult::Error(text) => Err(ClientError::Server(text)),
            _ => Err(ClientError::Mismatch {
                asked: "an outbox cancel",
            }),
        }
    }

    /// Own connection and own thread, so awaiting never occupies the executor
    /// and never queues behind another in-flight request.
    pub fn execute(&self, account: AccountId, command: Command) -> ClientFuture<Outcome> {
        self.off_thread(
            RequestKind::Execute { account, command },
            "a command",
            |result| match result {
                WireResult::Outcome { bodies_fetched } => Some(Outcome { bodies_fetched }),
                _ => None,
            },
        )
    }

    pub fn send(
        &self,
        account: AccountId,
        message: OutgoingMessage,
    ) -> ClientFuture<birdman_store::OutboxId> {
        self.off_thread(
            RequestKind::Send {
                account,
                message: Box::new(message),
            },
            "a send",
            |result| match result {
                WireResult::Queued { id } => Some(birdman_store::OutboxId(id)),
                _ => None,
            },
        )
    }

    fn off_thread<T: Send + 'static>(
        &self,
        kind: RequestKind,
        asked: &'static str,
        unwrap: fn(WireResult) -> Option<T>,
    ) -> ClientFuture<T> {
        let socket = self.socket.clone();
        let (tx, rx) = async_channel::bounded(1);
        let label = describe(&kind);
        std::thread::spawn(move || {
            let _timed = Timed::new(label, Timed::NETWORK);
            let outcome = (|| {
                ensure_daemon(&socket)?;
                let mut conn = Connection::open(&socket)?;
                let request = Request { id: 1, kind };
                let line = serde_json::to_string(&request)
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                writeln!(conn.write, "{line}")
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                conn.write
                    .flush()
                    .map_err(|err| ClientError::Transport(err.to_string()))?;
                loop {
                    let mut reply = String::new();
                    if conn
                        .read
                        .read_line(&mut reply)
                        .map_err(|err| ClientError::Transport(err.to_string()))?
                        == 0
                    {
                        return Err(ClientError::Transport(
                            "birdmand closed the connection".into(),
                        ));
                    }
                    match serde_json::from_str::<Frame>(&reply) {
                        Ok(Frame::Event(_)) => continue,
                        Ok(Frame::Reply {
                            result: WireResult::Error(message),
                            ..
                        }) => return Err(ClientError::Server(message)),
                        Ok(Frame::Reply { result, .. }) => {
                            return unwrap(result).ok_or(ClientError::Mismatch { asked })
                        }
                        Err(err) => {
                            return Err(ClientError::Transport(format!("unreadable reply: {err}")))
                        }
                    }
                }
            })();
            let _ = tx.send_blocking(outcome);
        });
        Box::pin(async move {
            rx.recv()
                .await
                .unwrap_or_else(|_| Err(ClientError::Transport("the request thread died".into())))
        })
    }

    /// Best-effort about the stop: a daemon too wedged to answer `Shutdown` is
    /// exactly the one worth replacing.
    ///
    /// Any [`subscribe`](Self::subscribe) stream taken before this ends with
    /// the old daemon, so callers that live on events must resubscribe.
    pub fn restart_daemon(&self) -> Result<()> {
        let _ = self.call_once(&RequestKind::Shutdown);
        if !spawn::wait_for_stop(&self.socket, std::time::Duration::from_secs(5)) {
            return Err(ClientError::Transport(
                "birdmand did not stop within 5s".into(),
            ));
        }
        spawn::ensure_daemon(&self.socket)?;
        // Every one of them was talking to the daemon that just stopped.
        self.conns.lock().map_err(|_| poisoned())?.clear();
        Ok(())
    }

    pub fn subscribe(&self) -> Result<async_channel::Receiver<Event>> {
        let mut conn = Connection::open(&self.socket)?;
        let request = Request {
            id: 0,
            kind: RequestKind::Subscribe,
        };
        let line = serde_json::to_string(&request).expect("subscribe always encodes");
        writeln!(conn.write, "{line}").map_err(|err| ClientError::Transport(err.to_string()))?;
        conn.write
            .flush()
            .map_err(|err| ClientError::Transport(err.to_string()))?;

        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match conn.read.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Ok(Frame::Event(event)) = serde_json::from_str::<Frame>(&line) {
                    if tx.send_blocking(event).is_err() {
                        break;
                    }
                }
            }
            log::debug!("event stream ended");
        });
        Ok(rx)
    }
}

fn describe(kind: &RequestKind) -> String {
    match kind {
        RequestKind::Query(query) => format!("client query {}", query.describe()),
        RequestKind::Execute { account, command } => {
            format!("client {} (account {})", command.describe(), account.0)
        }
        RequestKind::Send { account, .. } => format!("client send (account {})", account.0),
        RequestKind::OutboxRetry { id } => format!("client outbox retry ({})", id.0),
        RequestKind::OutboxCancel { id } => format!("client outbox cancel ({})", id.0),
        RequestKind::Subscribe => "client subscribe".to_string(),
        RequestKind::Hello { .. } => "client hello".to_string(),
        RequestKind::Shutdown => "client shutdown".to_string(),
    }
}

fn poisoned() -> ClientError {
    ClientError::Transport("the client connection is poisoned".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn socket() -> (tempfile::TempDir, PathBuf, UnixListener) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("birdman.sock");
        let listener = UnixListener::bind(&path).unwrap();
        (dir, path, listener)
    }

    fn read_request(reader: &mut BufReader<UnixStream>) -> Request {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn reply(stream: &mut UnixStream, id: u64, result: WireResult) {
        let frame = Frame::Reply { id, result };
        writeln!(stream, "{}", serde_json::to_string(&frame).unwrap()).unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn opening_any_connection_performs_the_version_handshake() {
        let (_dir, path, listener) = socket();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let request = read_request(&mut reader);
            assert_eq!(request.id, 0);
            assert!(matches!(
                request.kind,
                RequestKind::Hello {
                    version: birdman_proto::PROTOCOL_VERSION
                }
            ));
            reply(&mut stream, 0, WireResult::Done);
        });

        Connection::open(&path).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn a_version_mismatch_is_reported_before_any_request() {
        let (_dir, path, listener) = socket();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let request = read_request(&mut reader);
            assert!(matches!(request.kind, RequestKind::Hello { .. }));
            reply(
                &mut stream,
                0,
                WireResult::VersionMismatch {
                    daemon: 6,
                    client: birdman_proto::PROTOCOL_VERSION,
                },
            );
        });

        assert!(matches!(
            Connection::open(&path),
            Err(ClientError::VersionMismatch {
                daemon: 6,
                client: birdman_proto::PROTOCOL_VERSION
            })
        ));
        server.join().unwrap();
    }

    #[test]
    fn a_query_round_trips_after_the_handshake() {
        let (_dir, path, listener) = socket();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = read_request(&mut reader);
            reply(&mut stream, hello.id, WireResult::Done);

            let query = read_request(&mut reader);
            assert!(matches!(query.kind, RequestKind::Query(Query::Accounts)));
            reply(
                &mut stream,
                query.id,
                WireResult::Response(Response::Accounts(Vec::new())),
            );
        });

        let client = Client {
            conns: Mutex::new(vec![Arc::new(Mutex::new(Connection::open(&path).unwrap()))]),
            socket: path,
            next_id: AtomicU64::new(1),
        };
        assert!(client.accounts().unwrap().is_empty());
        server.join().unwrap();
    }
}
