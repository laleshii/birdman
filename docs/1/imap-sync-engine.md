---
id: imap-sync-engine
title: 'birdman-imap: the IMAP sync engine'
altitude: 1
topics:
- sync
- connectors
relations:
- type: refines
  target: birdman-overview
- type: depends_on
  target: local-message-store
- type: depends_on
  target: mime-parsing
- type: part_of
  target: connector-boundary
summary: birdman-imap runs IMAP sync on its own Tokio runtime on a dedicated OS thread, one supervised task per account, writing into birdman-store and emitting SyncEvents.
---

# birdman-imap: the IMAP sync engine

This crate is **one connector**, not shared infrastructure — it lives under
`crates/connectors/` for that reason and was renamed from `mail-core`, a name
that hid the fact that `connect`, `idle`, `session_cache` and `sync` have no
meaning outside IMAP. The protocol-neutral contract it implements is in
`crates/birdman-backend`; see [[connector-boundary]].

`crates/connectors/birdman-imap` owns everything network-facing on the receive side:
connecting, logging in, listing folders, fetching envelopes and bodies,
IDLE-based push, reconnect, and flag/delete operations. Built on `async-imap`
with `async-native-tls`.

## Its own runtime, on its own thread

`birdman_imap::spawn` (`src/lib.rs`) builds a **multi-thread Tokio runtime** and
`block_on`s it inside `std::thread::spawn`. This is deliberate: the engine is
completely independent of whatever executor the UI uses, because GPUI's
executors are not Tokio. `spawn` returns an `EngineHandle` with two things:

- `events: async_channel::Receiver<SyncEvent>` — the fact stream (see
  [[ui-sync-store-data-flow]])
- `runtime: tokio::runtime::Handle` — a way for outside code (including
  non-Tokio contexts like GPUI) to submit one-off work onto this runtime. See
  [[gpui-tokio-runtime-bridging]].

## One supervised task per account

Each `AccountConfig` gets its own `tokio::spawn(supervisor::run_account(...))`.
The isolation is intentional and stated in the code: one account's auth failure
or malformed server response can't take down another's, or the supervisor
itself. See [[sync-supervisor-loop]].

## Module map

| File | Responsibility |
|---|---|
| `lib.rs` | `spawn`, `SyncEvent`, `AccountConfig`, `CoreError`, `with_timeout` |
| `backend.rs` | `ImapBackend` — the `MailBackend` impl, see [[connector-boundary]] |
| `supervisor.rs` | the forever-loop per account: connect → sync all → IDLE/poll |
| `sync.rs` | folder list, per-folder UID sync, body fetch, flags, delete |
| `connect.rs` | TCP + implicit TLS + LOGIN |
| *(credentials)* | resolved through `birdman_auth::AuthAdapter` -- see [[auth-adapter-design]] |
| `session_cache.rs` | reused per-account connection for UI-triggered ops |
| `idle.rs` | `IDLE` with a refresh interval, and capability detection |

## Shared conventions inside the crate

- The `Store` is always `Arc<Mutex<Store>>`, and locks are taken for the
  shortest possible span — often a single statement in its own block — because
  the mutex is shared with the UI thread. `.expect("birdman-store mutex poisoned")`
  is the established idiom for the lock.
- Every fetch that reads message content uses `BODY.PEEK[...]`, never `BODY[...]`
  — see [[folder-and-uid-sync]] for why this matters.
- `CoreError` is a `thiserror` enum with `#[from]` conversions for every
  underlying error type; new failure modes get a variant rather than a
  stringly-typed error.
- UI-triggered one-off work is wrapped in `with_timeout` (`ON_DEMAND_TIMEOUT`,
  20s) so a stalled server can't hang the reading pane forever.

## v1 limitations that are choices, not omissions

- Implicit TLS only on IMAP; STARTTLS upgrade isn't implemented (`connect.rs`).
- IDLE runs on INBOX only — IMAP allows one selected mailbox per connection, and
  a multi-folder IDLE fan-out was out of scope. Other folders refresh on every
  reconnect pass.
- No CONDSTORE/QRESYNC: flags are re-fetched for the whole mailbox every sync.
  See [[folder-and-uid-sync]].
