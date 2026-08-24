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

#[derive(Default)]
pub struct SessionCache {
    /// Registry only. Never held across an await -- the awaited lock is the
    /// per-account one, so accounts do not serialise behind each other.
    slots: std::sync::Mutex<HashMap<birdman_store::AccountId, Slot>>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The guard holds this account's lock for its whole lifetime, so drop it
    /// promptly rather than across unrelated work.
    pub async fn selected(
        &self,
        config: &AccountConfig,
        auth: &Arc<dyn AuthAdapter>,
        imap_path: &str,
    ) -> Result<SessionGuard, CoreError> {
        let slot: Slot = {
            let mut slots = self.slots.lock().expect("session slot registry poisoned");
            slots.entry(config.account_id).or_default().clone()
        };

        let mut guard = slot.lock_owned().await;
        let usable = matches!(&*guard, Some(cached) if cached.selected == imap_path);
        if !usable {
            let mut session = connect_for_account(config, auth).await?;
            session.select(imap_path).await?;
            *guard = Some(Cached {
                session,
                selected: imap_path.to_string(),
            });
        }
        Ok(SessionGuard { guard })
    }
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
