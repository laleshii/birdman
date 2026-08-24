use std::sync::{Arc, Mutex};
use std::time::Duration;

use birdman_store::Store;

use crate::connect::connect_for_account;
use crate::idle::{idle_once_for, server_supports_idle, IdleOutcome};
use crate::sync::sync_folder;
use crate::{AccountConfig, CoreError, SyncEvent};
use birdman_auth::AuthAdapter;

const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// IMAP allows one selected -- hence idled -- mailbox per connection, so
/// INBOX gets the push and every other folder gets this: a `STATUS` sweep
/// whose only cost when nothing changed is one round trip per folder.
const FOLDER_POLL_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Shorter than [`crate::IDLE_REFRESH_INTERVAL`]: the supervisor can only
/// notice a dead connection when the wait returns, and the folder poll can
/// only run between waits. Still far under RFC 2177's 29-minute ceiling.
const IDLE_WAIT: Duration = Duration::from_secs(FOLDER_POLL_INTERVAL.as_secs());
const BODY_BATCH: u32 = 20;

const BODY_BACKFILL_MONTHS: i64 = 6;

pub const BODY_BUDGET_PER_SYNC: usize = 200;

const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// A session that survives this long was not refused -- something dropped it --
/// so the next attempt starts from a fresh backoff. Sized above the initial
/// sync, so reaching the idle loop at all counts as healthy.
const HEALTHY_SESSION: Duration = Duration::from_secs(120);

pub async fn run_account(
    config: AccountConfig,
    credentials: Arc<dyn AuthAdapter>,
    store: Arc<Mutex<Store>>,
    events: async_channel::Sender<SyncEvent>,
) {
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = std::time::Instant::now();
        // Keyed on how long the session lasted, not on an `Ok` return:
        // `run_account_once` never returns one, so a success-keyed reset can
        // never run and the backoff would climb to its ceiling and stay there.
        let Err(err) = run_account_once(&config, &credentials, &store, &events).await;
        let lasted = started.elapsed();

        let _ = events
            .send(SyncEvent::SyncError {
                account_id: config.account_id,
                message: err.to_string(),
            })
            .await;

        let wait;
        (wait, backoff) = advance_backoff(backoff, lasted);
        log::warn!(
            "{} lost its connection after {}s: {err} -- reconnecting in ~{}s",
            config.username,
            lasted.as_secs(),
            wait.as_secs()
        );
        tokio::time::sleep(with_jitter(wait)).await;
    }
}

fn advance_backoff(backoff: Duration, lasted: Duration) -> (Duration, Duration) {
    let wait = if lasted >= HEALTHY_SESSION {
        MIN_BACKOFF
    } else {
        backoff
    };
    (wait, (wait * 2).min(MAX_BACKOFF))
}

/// Two accounts on the same host go down together, and Gmail answers a burst
/// of simultaneous logins with `[AUTHENTICATIONFAILED]` whatever the credential
/// was. Derived from the clock to keep the crate free of an RNG dependency.
fn with_jitter(backoff: Duration) -> Duration {
    let spread = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos() as u64 % 1_000)
        .unwrap_or(500);
    backoff / 2 + (backoff / 2).mul_f64(spread as f64 / 1_000.0)
}

/// [`Infallible`](std::convert::Infallible) in the success position is load
/// bearing: it stops a caller writing a "reset on success" branch that can
/// never run. One did, for a while.
async fn run_account_once(
    config: &AccountConfig,
    auth: &Arc<dyn AuthAdapter>,
    store: &Arc<Mutex<Store>>,
    events: &async_channel::Sender<SyncEvent>,
) -> Result<std::convert::Infallible, CoreError> {
    let mut session = connect_for_account(config, auth).await?;

    // The folder list only, plus INBOX below. Syncing every folder's envelopes
    // here made launch take minutes; the rest sync when opened.
    let folders = crate::sync::sync_folder_list(&mut session, store, config.account_id).await?;
    let _ = events
        .send(SyncEvent::FoldersListed {
            account_id: config.account_id,
        })
        .await;

    // INBOX only: IMAP allows one selected -- hence idled -- mailbox per
    // connection, so anything else would need a fan-out of connections.
    let inbox = folders
        .iter()
        .find(|f| f.imap_path.eq_ignore_ascii_case("INBOX"))
        .ok_or(CoreError::NoInbox)?;

    let _ = events
        .send(SyncEvent::FolderSyncing {
            account_id: config.account_id,
            folder_name: inbox.name.clone(),
        })
        .await;
    let initial = sync_folder(
        &mut session,
        store,
        config.account_id,
        inbox.id,
        &inbox.imap_path,
    )
    .await?;
    backfill_folder_bodies(&mut session, store, inbox.id, BODY_BUDGET_PER_SYNC).await;
    emit_new_messages(events, config.account_id, inbox.id, initial.new_uids).await;
    let _ = events
        .send(SyncEvent::SyncComplete {
            account_id: config.account_id,
        })
        .await;

    session.select(&inbox.imap_path).await?;
    let supports_idle = server_supports_idle(&mut session).await.unwrap_or(false);

    // The INBOX sync above just ran; the other folders get their first look
    // one interval in rather than doubling the cost of connecting.
    let mut last_poll = std::time::Instant::now();

    loop {
        let inbox_moved;
        if supports_idle {
            let (outcome, returned_session) = idle_once_for(session, IDLE_WAIT).await?;
            session = returned_session;
            inbox_moved = matches!(outcome, IdleOutcome::Activity);
        } else {
            tokio::time::sleep(POLL_INTERVAL).await;
            inbox_moved = true;
        }

        // Between waits is the only chance the folder sweep gets, so a quiet
        // INBOX is when it takes its turn -- not a reason to skip everything.
        if last_poll.elapsed() >= FOLDER_POLL_INTERVAL {
            poll_other_folders(&mut session, store, config.account_id, events).await?;
            if supports_idle {
                session.select(&inbox.imap_path).await?;
            }
            last_poll = std::time::Instant::now();
        }

        if !inbox_moved {
            continue;
        }
        let result = sync_folder(
            &mut session,
            store,
            config.account_id,
            inbox.id,
            &inbox.imap_path,
        )
        .await?;
        backfill_folder_bodies(&mut session, store, inbox.id, BODY_BUDGET_PER_SYNC).await;
        emit_new_messages(events, config.account_id, inbox.id, result.new_uids).await;
    }
}

/// One `STATUS` per already-known folder, and a real sync only where the
/// counts moved against what the store holds.
///
/// Folders that were never synced stay lazy: `uid_validity` is set by the
/// first real sync, so a 40k-message archive is downloaded when the user
/// opens it, not because a timer walked past it.
///
/// A failed `STATUS` is treated as a suspect connection and returned, which
/// sends this account through the reconnect path -- guessing which errors
/// are benign is how a wedged session looks like a slow folder.
async fn poll_other_folders(
    session: &mut crate::ImapSession,
    store: &Arc<Mutex<Store>>,
    account_id: birdman_store::AccountId,
    events: &async_channel::Sender<SyncEvent>,
) -> Result<(), CoreError> {
    let candidates: Vec<birdman_store::Folder> = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.list_folders(account_id)?
    };

    for folder in candidates {
        if folder.imap_path.eq_ignore_ascii_case("INBOX") || folder.uid_validity.is_none() {
            continue;
        }
        let (total, unread): (u32, u32) = {
            let store = store.lock().expect("birdman-store mutex poisoned");
            store.count_messages(&[folder.id])?
        };
        let status = session
            .status(&folder.imap_path, "(MESSAGES UNSEEN UIDVALIDITY UIDNEXT)")
            .await?;
        // An absent UNSEEN means the server would not say, not "nothing":
        // only compare what was actually reported.
        let unseen_moved = status.unseen.is_some_and(|server| server != unread);
        let validity_moved = status
            .uid_validity
            .is_some_and(|server| Some(server) != folder.uid_validity);
        let next_moved = status
            .uid_next
            .is_some_and(|server| Some(server) != folder.uid_next);
        if status.exists == total && !unseen_moved && !validity_moved && !next_moved {
            continue;
        }
        log::info!(
            "{} changed on the server ({total} local / {} remote) -- syncing",
            folder.imap_path,
            status.exists
        );
        let _ = events
            .send(SyncEvent::FolderSyncing {
                account_id,
                folder_name: folder.name.clone(),
            })
            .await;
        let result = sync_folder(session, store, account_id, folder.id, &folder.imap_path).await?;
        backfill_folder_bodies(session, store, folder.id, BODY_BUDGET_PER_SYNC).await;
        emit_new_messages(events, account_id, folder.id, result.new_uids).await;
        let _ = events.send(SyncEvent::SyncComplete { account_id }).await;
    }
    Ok(())
}

/// Must run on the sync connection, with the folder already selected. Against
/// the shared session cache it reconnects whenever the selected mailbox
/// changes, forcing a fresh IMAP login every few messages.
///
/// Failures are remembered *for this call*: a message the server won't return
/// leaves `body_fetched` at 0, so without that the next query hands back the
/// same row forever.
pub async fn backfill_folder_bodies(
    session: &mut crate::ImapSession,
    store: &Arc<Mutex<Store>>,
    folder_id: birdman_store::FolderId,
    budget: usize,
) -> usize {
    let since = body_cutoff();
    let mut failed: std::collections::HashSet<birdman_store::MessageId> =
        std::collections::HashSet::new();
    let mut fetched = 0usize;

    while fetched < budget {
        let batch: Vec<_> = {
            let store = store.lock().expect("birdman-store mutex poisoned");
            match store.messages_missing_bodies(folder_id, since, BODY_BATCH) {
                Ok(rows) => rows
                    .into_iter()
                    .filter(|(id, _)| !failed.contains(id))
                    .collect(),
                Err(_) => break,
            }
        };
        if batch.is_empty() {
            break;
        }
        for (message_id, uid) in batch {
            if fetched >= budget {
                break;
            }
            match crate::sync::fetch_message_body(session, store, message_id, uid).await {
                Ok(()) => {
                    fetched += 1;
                    // Gmail mirrors one message into INBOX, All Mail and
                    // Important as labels, so most rows have siblings.
                    if let Ok(store) = store.lock() {
                        let _ = store.copy_body_to_siblings(message_id);
                    }
                }
                Err(err) => {
                    failed.insert(message_id);
                    log::warn!("body backfill skipped a message: {err}");
                }
            }
        }
    }
    fetched
}

/// 30-day months: the boundary is a policy choice, not a date calculation.
fn body_cutoff() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now - BODY_BACKFILL_MONTHS * 30 * 24 * 60 * 60
}

async fn emit_new_messages(
    events: &async_channel::Sender<SyncEvent>,
    account_id: birdman_store::AccountId,
    folder_id: birdman_store::FolderId,
    new_uids: Vec<u32>,
) {
    let _ = events
        .send(SyncEvent::NewMessages {
            account_id,
            folder_id,
            uids: new_uids,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_fast_failures_escalate_to_the_ceiling() {
        let mut backoff = MIN_BACKOFF;
        let instant = Duration::from_secs(1);
        let mut waits = Vec::new();
        for _ in 0..12 {
            let wait;
            (wait, backoff) = advance_backoff(backoff, instant);
            waits.push(wait);
        }
        assert_eq!(waits[0], Duration::from_secs(2));
        assert_eq!(waits[1], Duration::from_secs(4));
        assert_eq!(waits[2], Duration::from_secs(8));
        assert_eq!(*waits.last().unwrap(), MAX_BACKOFF);
    }

    #[test]
    fn a_session_that_lasted_starts_over_from_the_minimum() {
        let mut backoff = MAX_BACKOFF;
        let (wait, next) = advance_backoff(backoff, HEALTHY_SESSION);
        assert_eq!(
            wait, MIN_BACKOFF,
            "a healthy session should not be punished"
        );
        assert_eq!(next, MIN_BACKOFF * 2);

        backoff = Duration::from_secs(16);
        let (wait, _) = advance_backoff(backoff, HEALTHY_SESSION - Duration::from_secs(1));
        assert_eq!(wait, Duration::from_secs(16));
    }

    #[test]
    fn jitter_stays_within_the_window_it_spreads() {
        for secs in [2, 30, 300] {
            let backoff = Duration::from_secs(secs);
            let jittered = with_jitter(backoff);
            assert!(
                jittered >= backoff / 2,
                "{jittered:?} too short for {backoff:?}"
            );
            assert!(jittered <= backoff, "{jittered:?} longer than {backoff:?}");
        }
    }
}
