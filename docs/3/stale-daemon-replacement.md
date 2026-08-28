---
id: stale-daemon-replacement
title: An upgraded client replaces an older daemon, never a newer one
altitude: 3
topics:
- architecture
relations:
- type: references
  target: daemon-connection-pool
---

# An upgraded client replaces an older daemon, never a newer one

`crates/birdman-client/src/lib.rs` (`open_replacing_stale`) and
`crates/birdman-client/src/spawn.rs`.

## Why an upgrade needed handling at all

`ensure_daemon` only checks that *something* is listening on the socket. The
daemon outlives the client that started it and stops only when idle, so after an
upgrade the previous binary keeps serving. The handshake catches the skew, but
leaving it as an error makes the remedy `birdman daemon restart` — a CLI command
someone who only ever launches the desktop app has no reason to know exists.

## The rule that matters

Replace an **older** daemon; never a newer one.

A newer daemon belongs to a newer build, which is presumably also running. If
both sides replaced what they disagreed with, the two clients would restart the
daemon under each other for as long as both stayed open. Getting this backwards
produces a livelock rather than an error, which is why the direction has its own
test (`a_newer_daemon_is_reported_rather_than_replaced`) rather than being left
to the type system.

The remaining error case is therefore only "the mailbox is newer than this
build", and the message says so instead of suggesting a restart.

## Why stopping it bypasses `Client`

`Connection::open` performs the handshake and refuses on a mismatch — and a
version-mismatched daemon is exactly the one that has to be stopped. So
`stop_without_handshake` speaks the wire format directly: a raw `shutdown` frame
first, then `SIGTERM` via the pid file (never `SIGKILL`, the daemon has a store
to close). It lives in the client rather than the CLI because both need it.

## It helps from the *next* upgrade onward

The client that has to notice the skew is the one already running, which on any
given upgrade is still the old build. So the release that introduces this does
not fix its own installation; every upgrade after it self-heals. Worth
remembering before concluding the mechanism is broken.
