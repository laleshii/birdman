---
id: imap-connection-pool
title: 'IMAP connections: one per account, more on demand'
altitude: 2
topics:
- sync
- performance
relations:
- type: part_of
  target: imap-sync-engine
- type: supersedes
  target: on-demand-imap-session-cache
- type: references
  target: sync-supervisor-loop
---

# IMAP connections: one per account, more on demand

`crates/connectors/birdman-imap/src/session_cache.rs`. Replaces the single
cached connection described in [[on-demand-imap-session-cache]], which was
correct until a background sync and a click had to share it.

## Why one connection was not enough

The cache held exactly one connection per account, so every user-triggered
operation queued behind whatever was already using it. A background folder sync
holds it for twelve-odd seconds, which made opening a message take sixteen, and
frequently hit `ON_DEMAND_TIMEOUT` (20s) instead of finishing at all. Measured
on a real mailbox, a launch produced `execute open message ... took 20002ms`
alongside `execute delete message ... took 20002ms` — both the timeout, not real
work.

This is the "warm-up" a user perceives: slow for the first clicks after launch,
fast once the startup folder sweep drains.

## The shape

One connection per account until every one is busy, then another, capped at
three (`MAX_SESSIONS_PER_ACCOUNT`). Lanes are tried in order: one already
`SELECT`ed on the wanted mailbox first (no round trip at all), then any free one
(one `SELECT`), then growth, then waiting.

At the cap a caller waits on **whichever lane frees first**, via
`futures_util::future::select_all`, rather than a fixed lane — a long sync on the
lane you happened to pick would otherwise hold you up while another sat idle.

## Growth is serialised, deliberately

A `tokio::sync::Mutex` is held across the connect, so only one new connection
opens at a time however many callers arrive together. Gmail stalls connections
opened in quick succession and a stalled one never returns — the same behaviour
`ON_DEMAND_TIMEOUT` exists to survive. A burst of clicks must not turn into a
burst of logins.

The cap is politeness, not a protocol limit: Gmail allows 15 simultaneous IMAP
connections per account, and the supervisor already holds one outside this cache.

## Switching mailbox is a SELECT, not a reconnect

The superseded implementation dropped a live, authenticated session whenever the
wanted mailbox differed from the cached one, and built a new one — full TCP, TLS
and LOGIN. A sweep over ten folders was therefore ten handshakes and ten logins
in quick succession, which is precisely the pattern Gmail stalls on. It was the
larger half of the problem: after the fix a measured warm-up made **four**
connects in total rather than one per folder switch.

`prepare` assigns `cached.selected` only *after* the `SELECT` await returns, so a
failed switch leaves the old value rather than claiming a mailbox the session is
not on.

## Still true from the superseded doc

The supervisor's connection is separate and sits in an IDLE loop (see
[[sync-supervisor-loop]]); it is not part of this pool. `SessionGuard` derefs to
`ImapSession`, holds its lane for its whole lifetime — so drop it promptly — and
`invalidate()` clears the lane after a failed operation, because the connection
may be dead rather than the operation invalid.

The client side has the same problem one layer up, solved the same way: see
[[daemon-connection-pool]].
