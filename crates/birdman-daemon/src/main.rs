use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use birdman_config::logging::Timed;
use birdman_config::{Config, SaveToSent};
use birdman_proto::{Frame, Request, RequestKind, WireResult};
use birdman_service::{AccountBackends, Service};
use birdman_store::{SpecialUse, Store};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut daemon = birdman_config::load_daemon();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--no-auto-stop") {
        daemon.auto_stop = false;
    }
    if let Some(secs) = args
        .iter()
        .position(|a| a == "--idle-timeout")
        .and_then(|at| args.get(at + 1))
        .and_then(|v| v.parse().ok())
    {
        daemon.idle_timeout = std::time::Duration::from_secs(secs);
    }

    let data_dir = birdman_config::data_dir();
    std::fs::create_dir_all(&data_dir).expect("failed to create the data directory");
    // The database, the attachment cache and usually the socket all live here,
    // so this one `0700` is what enforces owner-only access.
    if let Err(err) = birdman_config::restrict_to_owner(&data_dir) {
        eprintln!("birdmand: could not restrict {}: {err}", data_dir.display());
        std::process::exit(1);
    }
    birdman_config::logging::init(&data_dir);

    let socket = birdman_proto::socket_path(&data_dir);
    let listener = match bind(&socket).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("birdmand: {err}");
            std::process::exit(1);
        }
    };
    log::info!("listening on {}", socket.display());

    // So a client can signal a daemon whose protocol it cannot speak -- which
    // is usually the whole reason to stop one.
    let pid_file = socket.with_extension("pid");
    let _ = std::fs::write(&pid_file, std::process::id().to_string());
    let _ = birdman_config::restrict_to_owner(&pid_file);

    let service = match build_service(&data_dir).await {
        Ok(service) => service,
        Err(err) => {
            log::error!("{err}");
            eprintln!("birdmand: {err}");
            std::process::exit(1);
        }
    };

    let clients = Arc::new(AtomicUsize::new(0));
    if daemon.auto_stop {
        spawn_idle_watchdog(
            clients.clone(),
            service.clone(),
            daemon.idle_timeout,
            socket.clone(),
        );
    } else {
        log::info!("auto-stop disabled; staying up until killed");
    }

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                match peer_check(&stream) {
                    PeerCheck::Ours => {}
                    PeerCheck::NotOurs(uid) => {
                        log::warn!("refused connection from uid {uid}");
                        continue;
                    }
                    PeerCheck::Gone(err) => {
                        log::debug!("peer hung up before it could be identified: {err}");
                        continue;
                    }
                }
                let service = service.clone();
                let clients = clients.clone();
                clients.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    if let Err(err) = serve(stream, service).await {
                        log::debug!("connection ended: {err}");
                    }
                    clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(err) => log::warn!("accept failed: {err}"),
        }
    }
}

enum PeerCheck {
    Ours,
    NotOurs(u32),
    /// Ordinary, not suspicious: liveness probes connect and drop immediately,
    /// so `getpeereid` returns `ENOTCONN` several times a second at startup.
    Gone(std::io::Error),
}

/// The second control after the socket's permissions. Filesystem modes are set
/// once and can be changed afterwards, or bypassed by a process that inherited
/// the descriptor; a uid comparison holds regardless. Root is refused too.
fn peer_check(stream: &UnixStream) -> PeerCheck {
    let peer = match stream.peer_cred() {
        Ok(peer) => peer,
        Err(err) => return PeerCheck::Gone(err),
    };
    // SAFETY: `getuid` is always successful and takes no arguments.
    let ours = unsafe { libc::getuid() };
    if peer.uid() == ours {
        PeerCheck::Ours
    } else {
        PeerCheck::NotOurs(peer.uid())
    }
}

/// Daily as well as at start: with the desktop open the daemon never goes
/// idle, so a start-only sweep would never run where it matters most.
fn spawn_attachment_sweep(service: Arc<Service>) {
    tokio::spawn(async move {
        loop {
            match service.sweep_attachments() {
                Ok(report) if report.stale_copies + report.orphaned_blobs > 0 => log::info!(
                    "attachment sweep: {} stale copies, {} orphaned blobs, {} MB reclaimed",
                    report.stale_copies,
                    report.orphaned_blobs,
                    report.bytes_reclaimed / 1024 / 1024
                ),
                Ok(_) => log::debug!("attachment sweep: nothing to remove"),
                Err(err) => log::warn!("attachment sweep failed: {err}"),
            }
            let week_ago = unix_now() - 7 * 24 * 60 * 60;
            if let Err(err) = service.sweep_sent_outbox(week_ago) {
                log::warn!("outbox sweep failed: {err}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

/// What an account needs for the "copy of what went out" step after a send:
/// the decision of whether to file one, and where to file it.
struct SentPolicy {
    account: birdman_store::AccountId,
    append: bool,
    imap: birdman_imap::AccountConfig,
    auth: Arc<dyn birdman_auth::AuthAdapter>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Gmail archives submissions to its own Sent folder; an explicit copy would
/// be the second of every send. Everything else does not, unless told.
fn server_archives_sent(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "imap.gmail.com"
        || host == "imap.googlemail.com"
        || host.ends_with(".gmail.com")
        || host.ends_with(".googlemail.com")
}

const OUTBOX_POLL_INTERVAL: Duration = Duration::from_secs(5);
const OUTBOX_MAX_ATTEMPTS: u32 = 8;
const OUTBOX_DELIVERY_TIMEOUT: Duration = Duration::from_secs(60);

/// 5s, 10s, 20s, ... capped at ten minutes. A flat retry would hammer a
/// server that is down; an unbounded wait would feel like the daemon forgot.
fn outbox_backoff(attempts: u32) -> Duration {
    Duration::from_secs(5u64.saturating_mul(2u64.saturating_pow(attempts.saturating_sub(1))))
        .min(Duration::from_secs(10 * 60))
}

/// Drains the outbox: delivers due rows through the account's sender, with
/// retry, and files a copy in the Sent folder when the account wants one.
///
/// The worker exists so that sending is a daemon job, not a client's: the
/// client that composed the mail can already be gone, and delivery with
/// retries is what survives a network blip or a restart.
fn spawn_outbox_worker(service: Arc<Service>, sent_policies: Vec<SentPolicy>) {
    let wake = service.outbox_wake();
    tokio::spawn(async move {
        loop {
            match service.due_outgoing(unix_now()) {
                Ok(entries) => {
                    for entry in entries {
                        deliver_one(&service, &sent_policies, entry).await;
                    }
                }
                Err(err) => log::warn!("outbox: {err}"),
            }
            tokio::select! {
                _ = wake.notified() => {}
                _ = tokio::time::sleep(OUTBOX_POLL_INTERVAL) => {}
            }
        }
    });
}

async fn deliver_one(
    service: &Arc<Service>,
    sent_policies: &[SentPolicy],
    entry: birdman_store::OutboxEntry,
) {
    let message: birdman_backend::OutgoingMessage = match serde_json::from_str(&entry.payload) {
        Ok(message) => message,
        Err(err) => {
            log::error!(
                "outbox {}: payload is unreadable ({err}); keeping it for inspection",
                entry.id.0
            );
            let _ = service
                .store()
                .lock()
                .map(|store| store.mark_outgoing_failed(&entry, "payload is unreadable", i64::MAX));
            service.publish(birdman_proto::Event::OutboxChanged {
                account: entry.account_id,
            });
            return;
        }
    };

    let claimed = service
        .store()
        .lock()
        .map_err(|_| "store is poisoned".to_string())
        .and_then(|store| {
            store
                .mark_outgoing_sending(entry.id)
                .map_err(|err| err.to_string())
        });
    match claimed {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            log::warn!("outbox {}: could not claim the row -- {err}", entry.id.0);
            return;
        }
    }
    service.publish(birdman_proto::Event::OutboxChanged {
        account: entry.account_id,
    });

    let account = entry.account_id;
    let delivery = match tokio::time::timeout(
        OUTBOX_DELIVERY_TIMEOUT,
        service.deliver(account, message.clone()),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(birdman_proto::ProtoError::Backend(
            "delivery timed out".into(),
        )),
    };

    match delivery {
        Ok(()) => {
            match service.store().lock() {
                Ok(store) => {
                    if let Err(err) = store.mark_outgoing_sent(entry.id) {
                        log::error!(
                            "outbox {}: delivered but could not record it -- {err}",
                            entry.id.0
                        );
                    }
                }
                Err(_) => log::error!("outbox {}: delivered but the store is poisoned", entry.id.0),
            }
            log::info!("outbox {}: delivered", entry.id.0);
            service.publish(birdman_proto::Event::OutboxChanged { account });
            if let Some(policy) = sent_policies
                .iter()
                .find(|p| p.account == account && p.append)
            {
                archive_sent_copy(service, policy, &message).await;
            }
        }
        Err(err) => {
            let attempts = entry.attempts + 1;
            // Past the budget the row stops becoming due on its own; it stays
            // failed and visible until `birdman outbox retry` claims it back.
            let retry_at = if attempts >= OUTBOX_MAX_ATTEMPTS {
                i64::MAX
            } else {
                unix_now() + outbox_backoff(attempts).as_secs() as i64
            };
            if let Ok(store) = service.store().lock() {
                let _ = store.mark_outgoing_failed(&entry, &err.to_string(), retry_at);
            }
            log::warn!("outbox {}: attempt {attempts} failed -- {err}", entry.id.0);
            service.publish(birdman_proto::Event::OutboxChanged { account });
        }
    }
}

/// Best-effort by design: delivery already succeeded, and losing the copy
/// must not turn the send itself into a failure.
async fn archive_sent_copy(
    service: &Arc<Service>,
    policy: &SentPolicy,
    message: &birdman_backend::OutgoingMessage,
) {
    let sent_path = match service.store().lock() {
        Ok(store) => match store.list_folders(policy.account) {
            Ok(folders) => folders
                .iter()
                .find(|f| f.special_use == Some(SpecialUse::Sent))
                .map(|f| f.imap_path.clone()),
            Err(err) => {
                log::warn!("could not resolve the Sent folder: {err}");
                return;
            }
        },
        Err(_) => return,
    };
    let Some(sent_path) = sent_path else { return };

    let raw = match birdman_smtp::render(message) {
        Ok(raw) => raw,
        Err(err) => {
            log::warn!("could not render the Sent copy: {err}");
            return;
        }
    };
    if let Err(err) =
        birdman_imap::append_message(&policy.imap, &policy.auth, &sent_path, &raw).await
    {
        log::warn!("delivered, but the Sent copy failed: {err}");
    }
}

/// Polls rather than waking on the last disconnect: the condition is "*still*
/// idle a moment later", so a client reconnecting within the window keeps it.
fn spawn_idle_watchdog(
    clients: Arc<AtomicUsize>,
    service: Arc<Service>,
    idle: std::time::Duration,
    socket: std::path::PathBuf,
) {
    let tick = (idle / 10).max(std::time::Duration::from_secs(1));
    tokio::spawn(async move {
        let mut idle_since: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(tick).await;
            if clients.load(Ordering::SeqCst) > 0 || service.outbox_has_automatic_work() {
                idle_since = None;
                continue;
            }
            let since = *idle_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= idle {
                log::info!("no clients for {}s, shutting down", idle.as_secs());
                // So the next client binds cleanly rather than having to prove
                // this one is stale.
                let _ = std::fs::remove_file(&socket);
                let _ = std::fs::remove_file(socket.with_extension("pid"));
                std::process::exit(0);
            }
        }
    });
}

/// Staleness is decided by *connecting*, not by whether the file exists.
async fn bind(path: &std::path::Path) -> Result<UnixListener, String> {
    // A socket is only as private as the directory holding it, and
    // `BIRDMAN_SOCKET` can point anywhere -- at `/tmp` this would hand the
    // mailbox to every process on the machine. Refused rather than repaired:
    // quietly making somebody else's directory owner-only is not ours to do.
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    match birdman_config::is_reachable_by_others(dir) {
        Ok(true) => {
            return Err(format!(
                "refusing to bind {} -- {} is reachable by other users (needs mode 0700)",
                path.display(),
                dir.display()
            ))
        }
        Ok(false) => {}
        Err(err) => return Err(format!("could not check {}: {err}", dir.display())),
    }

    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(format!(
                "another birdmand is already listening on {}",
                path.display()
            ));
        }
        log::info!("removing stale socket {}", path.display());
        let _ = std::fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)
        .map_err(|err| format!("could not bind {}: {err}", path.display()))?;
    // So a directory later loosened by hand does not immediately expose this.
    if let Err(err) = birdman_config::restrict_to_owner(path) {
        return Err(format!("could not restrict {}: {err}", path.display()));
    }
    Ok(listener)
}

async fn build_service(data_dir: &std::path::Path) -> Result<Arc<Service>, String> {
    let accounts = match birdman_config::load() {
        Config::Accounts(accounts) => accounts,
        Config::Unconfigured { path, error } => {
            let mut message = format!("no usable account in {}", path.display());
            if let Some(error) = error {
                message.push_str(&format!(" -- {error}"));
            }
            return Err(message);
        }
    };

    let store = Store::open(&data_dir.join("mail.db"), data_dir)
        .map_err(|err| format!("could not open the mailbox: {err}"))?;
    let ids: Vec<_> = accounts.iter().map(|a| ensure_account(&store, a)).collect();
    let store = Arc::new(Mutex::new(store));

    let adapters: Vec<_> = accounts.iter().map(|a| a.auth.adapter()).collect();
    let receiver_configs: Vec<_> = accounts
        .iter()
        .zip(&ids)
        .map(|(account, id)| birdman_imap::AccountConfig {
            account_id: *id,
            imap_host: account.receiver.host.clone(),
            imap_port: account.receiver.port,
            username: account.auth.username.clone(),
            keyring_ref: account.auth.username.clone(),
            danger_accept_invalid_certs: account.danger_accept_invalid_certs,
        })
        .collect();

    let engine = birdman_imap::spawn(
        receiver_configs
            .iter()
            .cloned()
            .zip(adapters.iter().cloned())
            .collect(),
        store.clone(),
    );
    let sessions = Arc::new(birdman_imap::SessionCache::new());

    let backends: Vec<AccountBackends> = accounts
        .iter()
        .zip(&ids)
        .zip(&receiver_configs)
        .zip(&adapters)
        .map(|(((account, id), receiver_config), auth)| {
            let receiver: Arc<dyn birdman_backend::MailReceiver> = match account.receiver.kind {
                birdman_config::ReceiverKind::Imap => Arc::new(birdman_imap::ImapBackend::new(
                    vec![receiver_config.clone()],
                    auth.clone(),
                    sessions.clone(),
                    store.clone(),
                    engine.runtime.clone(),
                )),
            };
            let sender: Arc<dyn birdman_backend::MailSender> = match account.sender.kind {
                birdman_config::SenderKind::Smtp => Arc::new(birdman_smtp::SmtpSender::new(
                    birdman_smtp::SmtpConfig {
                        host: account.sender.host.clone(),
                        port: account.sender.port,
                        implicit_tls: account.sender.implicit_tls,
                        username: account.auth.username.clone(),
                        danger_accept_invalid_certs: account.danger_accept_invalid_certs,
                    },
                    auth.clone(),
                    account.id.clone(),
                )),
            };
            log::info!("account {:?} ({}) ready", account.id, account.email);
            AccountBackends {
                id: *id,
                receiver,
                sender,
            }
        })
        .collect();

    // Reads only. `Store::open` is idempotent, and opening the same file twice
    // is what WAL is for.
    let reader = Store::open(&data_dir.join("mail.db"), data_dir)
        .map_err(|err| format!("could not open the message store for reading: {err}"))?;
    let service = Arc::new(Service::new(store, reader, backends));
    spawn_attachment_sweep(service.clone());

    let sent_policies: Vec<SentPolicy> = accounts
        .iter()
        .zip(&ids)
        .zip(&receiver_configs)
        .zip(&adapters)
        .map(|(((account, id), imap_config), auth)| SentPolicy {
            account: *id,
            append: match account.save_to_sent {
                SaveToSent::Yes => true,
                SaveToSent::No => false,
                SaveToSent::Auto => !server_archives_sent(&account.receiver.host),
            },
            imap: imap_config.clone(),
            auth: auth.clone(),
        })
        .collect();
    spawn_outbox_worker(service.clone(), sent_policies);

    let events = engine.events.clone();
    let publishing = service.clone();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            let event = translate(event);
            // Debug: a running commentary, not a record of what happened.
            log::debug!("event {event:?}");
            publishing.publish(event);
        }
    });

    Ok(service)
}

fn translate(event: birdman_imap::SyncEvent) -> birdman_proto::Event {
    use birdman_imap::SyncEvent;
    match event {
        SyncEvent::FoldersListed { account_id } => birdman_proto::Event::FoldersChanged {
            account: account_id,
        },
        SyncEvent::FolderSyncing {
            account_id,
            folder_name,
        } => birdman_proto::Event::SyncProgress {
            account: account_id,
            folder: Some(folder_name),
        },
        SyncEvent::NewMessages { folder_id, .. } => {
            birdman_proto::Event::MessagesChanged { folder: folder_id }
        }
        SyncEvent::SyncComplete { account_id } => birdman_proto::Event::SyncIdle {
            account: account_id,
        },
        SyncEvent::SyncError {
            account_id,
            message,
        } => birdman_proto::Event::SyncFailed {
            account: account_id,
            message,
        },
    }
}

fn ensure_account(
    store: &Store,
    account: &birdman_config::ConfiguredAccount,
) -> birdman_store::AccountId {
    if let Ok(accounts) = store.list_accounts() {
        if let Some(existing) = accounts.iter().find(|a| a.email == account.email) {
            return existing.id;
        }
    }
    store
        .insert_account(&birdman_store::NewAccount {
            display_name: &account.display_name,
            email: &account.email,
            imap_host: &account.receiver.host,
            imap_port: account.receiver.port,
            imap_security: birdman_store::Security::Tls,
            smtp_host: &account.sender.host,
            smtp_port: account.sender.port,
            smtp_security: if account.sender.implicit_tls {
                birdman_store::Security::Tls
            } else {
                birdman_store::Security::StartTls
            },
            username: &account.auth.username,
            keyring_ref: &account.auth.username,
        })
        .expect("failed to insert account")
}

fn describe(kind: &RequestKind) -> String {
    match kind {
        RequestKind::Query(query) => format!("query {query:?}"),
        RequestKind::Execute { account, command } => {
            format!("execute {} for account {}", command.describe(), account.0)
        }
        RequestKind::Send { account, .. } => format!("send for account {}", account.0),
        RequestKind::OutboxRetry { id } => format!("outbox retry {}", id.0),
        RequestKind::OutboxCancel { id } => format!("outbox cancel {}", id.0),
        RequestKind::Subscribe => "subscribe".to_string(),
        RequestKind::Hello { version } => format!("hello v{version}"),
        RequestKind::Shutdown => "shutdown".to_string(),
    }
}

/// Sequential per connection, so replies arrive in the order requests were
/// sent. A client wanting concurrency opens a second connection.
async fn serve(stream: UnixStream, service: Arc<Service>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut events: Option<async_channel::Receiver<birdman_proto::Event>> = None;
    let mut handshaken = false;

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                if line.trim().is_empty() {
                    continue;
                }
                let mut requested_shutdown = false;
                let frame = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => {
                        let id = request.id;
                        requested_shutdown = matches!(request.kind, RequestKind::Shutdown);
                        let gate = handshake_gate(&mut handshaken, &request.kind);
                        if gate.is_none()
                            && matches!(request.kind, RequestKind::Subscribe)
                            && events.is_none()
                        {
                            events = Some(service.subscribe());
                        }
                        let budget = match &request.kind {
                            RequestKind::Execute { .. } => Timed::NETWORK,
                            _ => Timed::ROUND_TRIP,
                        };
                        let _timed = Timed::new(describe(&request.kind), budget);
                        let result = match gate {
                            Some(result) => result,
                            None => handle(&service, request.kind).await,
                        };
                        Frame::Reply { id, result }
                    }
                    Err(err) => Frame::Reply {
                        // An unparseable line has no id, so 0 is reserved for
                        // "this reply belongs to nothing".
                        id: 0,
                        result: WireResult::Error(format!("malformed request: {err}")),
                    },
                };
                let shutting_down = matches!(
                    &frame,
                    Frame::Reply { result: WireResult::Done, .. }
                ) && requested_shutdown;
                send(&mut write, &frame).await?;
                if shutting_down {
                    // Leaving it behind makes the next client prove staleness.
                    let socket = birdman_proto::socket_path(&birdman_config::data_dir());
                    let _ = std::fs::remove_file(&socket);
                    let _ = std::fs::remove_file(socket.with_extension("pid"));
                    std::process::exit(0);
                }
            }
            event = async {
                match &events {
                    Some(rx) => rx.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                let Some(event) = event else { continue };
                send(&mut write, &Frame::Event(event)).await?;
            }
        }
    }
}

/// Only shutdown is allowed without a successful hello: an older daemon is
/// precisely the process a newer CLI must still be able to stop.
fn handshake_gate(handshaken: &mut bool, kind: &RequestKind) -> Option<WireResult> {
    match kind {
        RequestKind::Hello { version } if *version == birdman_proto::PROTOCOL_VERSION => {
            *handshaken = true;
            None
        }
        RequestKind::Hello { .. } | RequestKind::Shutdown => None,
        _ if *handshaken => None,
        _ => Some(WireResult::Error(
            "protocol handshake required before this request".into(),
        )),
    }
}

async fn send(write: &mut tokio::net::unix::OwnedWriteHalf, frame: &Frame) -> std::io::Result<()> {
    let mut line = serde_json::to_string(frame).unwrap_or_else(|err| {
        serde_json::to_string(&Frame::Reply {
            id: 0,
            result: WireResult::Error(format!("could not encode reply: {err}")),
        })
        .expect("the error frame always encodes")
    });
    line.push('\n');
    write.write_all(line.as_bytes()).await
}

async fn handle(service: &Arc<Service>, kind: RequestKind) -> WireResult {
    match kind {
        RequestKind::Query(query) => match service.query(query) {
            Ok(response) => WireResult::Response(response),
            Err(err) => WireResult::Error(err.to_string()),
        },
        RequestKind::Execute { account, command } => {
            match service.execute(account, command).await {
                Ok(outcome) => WireResult::Outcome {
                    bodies_fetched: outcome.bodies_fetched,
                },
                Err(err) => WireResult::Error(err.to_string()),
            }
        }
        RequestKind::Send { account, message } => match service.queue_send(account, *message) {
            Ok(id) => WireResult::Queued { id: id.0 },
            Err(err) => WireResult::Error(err.to_string()),
        },
        RequestKind::OutboxRetry { id } => match service.outbox_retry(id) {
            Ok(changed) => WireResult::Outbox { changed },
            Err(err) => WireResult::Error(err.to_string()),
        },
        RequestKind::OutboxCancel { id } => match service.outbox_cancel(id) {
            Ok(changed) => WireResult::Outbox { changed },
            Err(err) => WireResult::Error(err.to_string()),
        },
        RequestKind::Subscribe => WireResult::Done,
        RequestKind::Hello { version } => {
            if version == birdman_proto::PROTOCOL_VERSION {
                WireResult::Done
            } else {
                WireResult::VersionMismatch {
                    daemon: birdman_proto::PROTOCOL_VERSION,
                    client: version,
                }
            }
        }
        RequestKind::Shutdown => {
            log::info!("shutdown requested");
            // Answered before exiting, so the client sees confirmation rather
            // than a closed socket.
            WireResult::Done
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn binding_refuses_a_directory_others_can_reach() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = bind(&dir.path().join("birdman.sock")).await.unwrap_err();
        assert!(err.contains("reachable by other users"), "{err}");
    }

    #[tokio::test]
    async fn a_bound_socket_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("birdman.sock");

        let _listener = bind(&path).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn our_own_connections_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("birdman.sock");
        let listener = bind(&path).await.unwrap();

        let _client = UnixStream::connect(&path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        // A test running as one user cannot produce a different uid, so only
        // the accepting half is asserted here.
        assert!(matches!(peer_check(&server), PeerCheck::Ours));
    }

    #[test]
    fn sent_copy_auto_detection_only_skips_google_mail() {
        assert!(server_archives_sent("imap.gmail.com"));
        assert!(server_archives_sent("imap.googlemail.com"));
        assert!(!server_archives_sent("mail.example.com"));
    }

    #[test]
    fn outbox_retry_backoff_grows_and_caps() {
        assert_eq!(outbox_backoff(1), Duration::from_secs(5));
        assert_eq!(outbox_backoff(2), Duration::from_secs(10));
        assert_eq!(outbox_backoff(20), Duration::from_secs(10 * 60));
    }

    #[test]
    fn requests_are_refused_until_a_matching_hello() {
        let mut handshaken = false;
        assert!(matches!(
            handshake_gate(
                &mut handshaken,
                &RequestKind::Query(birdman_proto::Query::Accounts)
            ),
            Some(WireResult::Error(_))
        ));
        assert!(handshake_gate(
            &mut handshaken,
            &RequestKind::Hello {
                version: birdman_proto::PROTOCOL_VERSION
            }
        )
        .is_none());
        assert!(handshaken);
        assert!(handshake_gate(
            &mut handshaken,
            &RequestKind::Query(birdman_proto::Query::Accounts)
        )
        .is_none());
    }

    #[test]
    fn shutdown_can_cross_a_version_boundary() {
        let mut handshaken = false;
        assert!(handshake_gate(&mut handshaken, &RequestKind::Shutdown).is_none());
        assert!(!handshaken);
    }
}
