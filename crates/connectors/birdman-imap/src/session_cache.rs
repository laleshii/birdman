use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

use birdman_auth::AuthAdapter;

use crate::{connect_for_account, AccountConfig, CoreError, ImapSession};

struct Cached {
    session: ImapSession,
    selected: String,
}

type Slot = Arc<Mutex<Option<Cached>>>;

/// One connection is enough until it isn't. A second is opened only when every
/// existing one is busy, which is the case this exists for: a background folder
/// sync holds the connection for ten-odd seconds, and a click behind it used to
/// wait the whole time and often hit [`crate::ON_DEMAND_TIMEOUT`].
///
/// The cap is low on purpose. Gmail allows 15 simultaneous IMAP connections per
/// account and the supervisor already holds one outside this cache, but it also
/// stalls connections opened in quick succession -- so the ceiling that matters
/// is politeness, not the documented limit.
const MAX_SESSIONS_PER_ACCOUNT: usize = 3;

#[derive(Default)]
struct Lanes {
    /// Never held across an await.
    slots: std::sync::Mutex<Vec<Slot>>,
    /// Held across the connect, so only one new connection is opened at a time
    /// however many callers arrive together. Gmail stalls connections opened in
    /// quick succession, and a stalled one never returns.
    growth: Mutex<()>,
}

#[derive(Default)]
pub struct SessionCache {
    /// Registry only. Never held across an await -- the awaited locks are
    /// per-account, so accounts do not serialise behind each other.
    accounts: std::sync::Mutex<HashMap<birdman_store::AccountId, Arc<Lanes>>>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The guard holds one of this account's connections for its whole
    /// lifetime, so drop it promptly rather than across unrelated work.
    pub async fn selected(
        &self,
        config: &AccountConfig,
        auth: &Arc<dyn AuthAdapter>,
        imap_path: &str,
    ) -> Result<SessionGuard, CoreError> {
        let lanes: Arc<Lanes> = {
            let mut accounts = self.accounts.lock().expect("session registry poisoned");
            accounts.entry(config.account_id).or_default().clone()
        };
        let existing = lanes.slots.lock().expect("session slots poisoned").clone();

        // Already sitting on this mailbox: no SELECT, no connect.
        for slot in &existing {
            if let Ok(guard) = slot.clone().try_lock_owned() {
                if matches!(&*guard, Some(cached) if cached.selected == imap_path) {
                    return Ok(SessionGuard { guard });
                }
            }
        }
        // Free, but on another mailbox: one SELECT, still no connect.
        for slot in &existing {
            if let Ok(guard) = slot.clone().try_lock_owned() {
                return prepare(guard, config, auth, imap_path).await;
            }
        }

        if existing.len() < MAX_SESSIONS_PER_ACCOUNT {
            let _growing = lanes.growth.lock().await;
            // Re-read: another caller may have grown while this one waited, and
            // that connection is worth having rather than adding a third.
            let fresh = {
                let mut slots = lanes.slots.lock().expect("session slots poisoned");
                if slots.len() < MAX_SESSIONS_PER_ACCOUNT {
                    let slot: Slot = Slot::default();
                    slots.push(slot.clone());
                    Some(slot)
                } else {
                    None
                }
            };
            if let Some(slot) = fresh {
                log::debug!(
                    "account {}: every connection busy, opening another",
                    config.account_id.0
                );
                // Uncontended: nothing else has seen this slot yet. `_growing`
                // outlives the connect inside `prepare`, which is the point.
                let guard = slot.lock_owned().await;
                return prepare(guard, config, auth, imap_path).await;
            }
        }

        // At the cap with all of them busy. Take whichever frees first rather
        // than a fixed one, or a long sync on the lane we picked would hold up
        // a caller that another lane could already have served.
        let waiters = existing
            .iter()
            .map(|slot| Box::pin(slot.clone().lock_owned()))
            .collect::<Vec<_>>();
        let (guard, _, _) = futures_util::future::select_all(waiters).await;
        prepare(guard, config, auth, imap_path).await
    }
}

/// Reuses the connection where it can. Switching mailbox is a `SELECT`, not a
/// reconnect: dropping a live authenticated session to change folder made a
/// sweep of ten folders ten TLS handshakes and ten logins, in quick succession,
/// which is precisely what Gmail stalls.
async fn prepare(
    mut guard: OwnedMutexGuard<Option<Cached>>,
    config: &AccountConfig,
    auth: &Arc<dyn AuthAdapter>,
    imap_path: &str,
) -> Result<SessionGuard, CoreError> {
    match guard.as_mut() {
        Some(cached) if cached.selected == imap_path => {}
        Some(cached) => {
            cached.session.select(imap_path).await?;
            // After the await, so a failed SELECT leaves the old value rather
            // than claiming a mailbox this session is not on.
            cached.selected = imap_path.to_string();
        }
        None => {
            let mut session = connect_for_account(config, auth).await?;
            session.select(imap_path).await?;
            *guard = Some(Cached {
                session,
                selected: imap_path.to_string(),
            });
        }
    }
    Ok(SessionGuard { guard })
}

pub struct SessionGuard {
    guard: OwnedMutexGuard<Option<Cached>>,
}

impl std::ops::Deref for SessionGuard {
    type Target = ImapSession;
    fn deref(&self) -> &ImapSession {
        &self
            .guard
            .as_ref()
            .expect("SessionCache::selected always fills the slot before returning")
            .session
    }
}

impl std::ops::DerefMut for SessionGuard {
    fn deref_mut(&mut self) -> &mut ImapSession {
        &mut self
            .guard
            .as_mut()
            .expect("SessionCache::selected always fills the slot before returning")
            .session
    }
}

impl SessionGuard {
    /// Call after any failed operation: the connection may be dead rather than
    /// the operation invalid, so the next `selected` must reconnect fresh.
    pub fn invalidate(mut self) {
        *self.guard = None;
    }
}
