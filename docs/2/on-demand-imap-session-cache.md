---
id: on-demand-imap-session-cache
title: 'SessionCache: reused connections for user-triggered IMAP operations'
altitude: 2
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
summary: A per-account cached, SELECTed IMAP connection for UI-triggered fetch/flag/delete, separate from the supervisor's IDLE-holding one, with explicit invalidation on failure.
---

# SessionCache: reused connections for user-triggered IMAP operations

`crates/connectors/birdman-imap/src/session_cache.rs`. Solves a specific, observed problem.

## The problem it fixed

UI-triggered operations — fetch a body, set a flag, delete — used to open a
brand-new connection (full TCP + TLS + LOGIN handshake) on *every single call*.
That's slow, and against Gmail specifically it's prone to stalling and
throttling when several happen in quick succession, which is exactly what a user
clicking through messages produces. Reusing one connection per account fixes
both.

## Why it isn't the supervisor's connection

The supervisor's connection sits inside an IDLE loop (see
[[sync-supervisor-loop]]) and isn't free to be interrupted for a one-off
command. So there are deliberately two connections per account: one idling, one
for on-demand work.

## The API

`SessionCache::selected(config, credentials, imap_path) -> SessionGuard`
returns a session already `SELECT`ed on `imap_path`. It connects fresh only when
there's no cached session for the account yet, **or** the cached one is selected
on a different folder.

`SessionGuard` derefs to `ImapSession` and holds the cache's `tokio::sync::Mutex`
guard for its whole lifetime. That means **only one on-demand operation runs at a
time**, which matches IMAP's own one-command-at-a-time-per-connection protocol.
The practical rule: drop the guard promptly, never hold it across unrelated work.

## `invalidate()` on failure

This is the part that's easy to get wrong. If an operation using the guard
fails, call `guard.invalidate()` — it drops the cached session instead of keeping
it, because the failure might mean the connection itself is dead (server closed
it, network drop) rather than the operation being invalid. The next `selected`
call then reconnects fresh.

It deliberately does **not** retry the failed operation. The user surfaces the
error and tries again (a click), which is simple and avoids an unbounded retry
loop.

The established call shape, used in both `AppState::select_message` and
`AppState::delete_selected` in `crates/birdman-ui/src/state.rs`:

```rust
let mut session = session_cache.selected(&account, &credentials, &folder.imap_path).await?;
let result: Result<(), CoreError> = async { /* ... operations ... */ }.await;
if result.is_err() {
    session.invalidate();
}
result
```

The whole thing is wrapped in `birdman_imap::with_timeout` (`ON_DEMAND_TIMEOUT`,
20s). Before that timeout existed, a stalled Gmail connection left the reading
pane on "Loading message…" forever with no recovery short of restarting.

## One lock per account, not one for the cache

The guard returned by `selected()` used to hold a `MutexGuard` over the whole
`HashMap`, which serialised every account's on-demand work behind one lock.

With two accounts that was not merely slow. A UI-triggered `SyncFolder` runs a
body backfill of up to 200 messages while holding the guard, so a second
account's "open this message" waited behind it and hit `ON_DEMAND_TIMEOUT` --
reporting *"timed out talking to the server"* when the server had never been
asked anything. Opening mail on one account failed because a **different**
account was syncing.

The map is now a registry of per-account slots: its own lock is a
`std::sync::Mutex` held only long enough to clone an `Arc`, never across an
await, and the awaited lock is the per-account one (`OwnedMutexGuard`).

That keeps the property IMAP actually requires -- one command at a time per
*connection* -- and drops the one it does not.

Within a single account this is unchanged: a UI-triggered sync still holds that
account's connection for the length of its backfill. The supervisor has its own
connection, so background sync is unaffected.
