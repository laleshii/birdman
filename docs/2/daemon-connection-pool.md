---
id: daemon-connection-pool
title: 'Client-to-daemon connections: one, then more on demand'
altitude: 2
topics:
- architecture
- performance
relations:
- type: references
  target: imap-connection-pool
---

# Client-to-daemon connections: one, then more on demand

`crates/birdman-client/src/lib.rs`.

## The daemon serves each connection strictly in order

That is a deliberate protocol property, and `serve()` in the daemon says so:
*"Sequential per connection, so replies arrive in the order requests were sent.
A client wanting concurrency opens a second connection."*

Every query went through `Client::call`, which locks one shared `Connection` for
the whole of its request **and** its reply. So the ordering guarantee, which is
per-connection by design, became a global one: a slow query delayed every other.
Measured at 6142ms of queueing for a reply that itself took 1ms.

## Same shape as the IMAP pool

`checkout()` returns a free connection, opens another when every existing one is
busy (capped at `MAX_CONNECTIONS`, 4), and otherwise hands back the first to wait
on. See [[imap-connection-pool]] — the two caps are independent and answer to
different limits: this one is how many the daemon must serve concurrently, that
one is how many logins a mail provider tolerates.

`try_lock` is used only to *ask* whether a connection is free; the guard is
dropped and the caller re-locks. Losing that race means waiting on a connection
that was free a moment ago, which is not an error.

## A transport error clears the whole pool

`call` retries once on `ClientError::Transport`, and that means the daemon went
away — so every connection to it is stale, not just the one that noticed. The
pool is cleared rather than one entry replaced. A caller still holding a
connection keeps it alive through its `Arc` and fails on its own next use.

## Queue time is timed apart from round-trip time

`call_once` wraps the lock acquisition in its own `Timed` (`"<request> queued"`),
separate from the timer around the whole call. Without that split a request
waiting for the connection and a request the daemon was slow to answer look
identical in the log, which is exactly how the warm-up stall got misattributed
twice while it was being diagnosed. Zero `queued` time with a large total means
look at the daemon; the reverse means look here.
