---
id: service-boundary
title: 'The service boundary: one server, several clients'
altitude: 0
topics:
- architecture
relations:
- type: refines
  target: birdman-overview
- type: references
  target: connector-boundary
summary: Why Birdman is being split into a service and clients, the Query/Command/Event contract, what has landed, and what a daemon still needs.
---

# The service boundary: one server, several clients

Birdman is being reshaped from "a desktop app that owns a mailbox" into "a
service that owns a mailbox, with clients in front of it". The desktop app is
the first client; a CLI is the second, and is meant to come *first* for every
feature after this one.

**Status: built.** `birdmand` owns the mailbox and both clients talk to it over
a Unix socket. See [[daemon-and-clients]] for the transport and lifecycle, and
[[cli-client]] for the second client that proved the contract.

## Why

- Sync should continue when no window is open.
- One writer avoids SQLite's multi-writer problems, and one connection pool
  avoids re-authenticating per client.
- `birdman ls` should answer from the store instantly rather than opening IMAP.
- A feature that cannot be expressed in the contract is a feature only the
  desktop can have. Making the contract the product surface is what keeps a
  CLI from perpetually lagging.

## The contract

`crates/birdman-proto`. Three kinds of traffic, and they are different on
purpose:

| | direction | shape |
|---|---|---|
| [`Query`] | client asks | request/response, read-only |
| `birdman_backend::Command` | client asks | request/response, changes state |
| [`Event`] | server tells | stream, unsolicited |

Eight queries cover everything the desktop reads: `Accounts`, `Folders`,
`UnreadCounts`, `Messages`, `MessageCounts`, `Search`, `Body`,
`InlineAttachments`. That the whole read surface is eight values was the
surprise of the exercise -- the connector work had already concentrated the
store access into five call sites.

Commands were **already** modelled; see [[connector-boundary]]. That work turns
out to have been most of this one.

### Queries are values, not methods

Same reasoning as `Command`: a value can be logged, queued, replayed and sent
over a socket. A trait method can only be called. Ergonomics are recovered on
the client side with typed helpers over `query()`, not by weakening the
contract.

A helper that receives the wrong `Response` variant returns
`ProtoError::Mismatch` rather than panicking. A client cannot tell a server bug
from a version skew, and neither should take the app down.

### Events say what changed, never what it changed to

`MessagesChanged { folder }`, not the messages. Clients re-query.

This is the same rule the sync engine already followed
([[ui-sync-store-data-flow]]), and it is what stops the event stream becoming a
second, unreliable copy of the store. It also means a client that falls behind
catches up in one query instead of replaying a backlog.

`SyncEvent` -- the IMAP supervisor's own vocabulary -- is **not** the protocol
event type. It is translated at the wiring, so a JMAP connector with entirely
different internals reports the same handful of facts.

## What the service owns

`crates/birdman-service`. Reads, writes and the event stream:

```rust
service.query(Query) -> Response          // reads
service.execute(account, Command)         // writes
service.send(account, OutgoingMessage)    // writes
service.subscribe() -> Receiver<Event>    // changes
```

It holds the store and the per-account connectors (`AccountBackends`).

**A client holds none of those.** That is the load-bearing property: the
desktop's `AppState` has no store handle, no `Arc<dyn MailReceiver>`, no
`Arc<dyn MailSender>`. It names an account and a command. The compiler
confirmed the split rather than a comment asserting it -- once the reads moved,
the store field and both connector imports became unused.

### Folder ordering is a server promise

`Query::Folders` returns folders in sidebar order, and `is_default_folder`
lives in the service. It was a UI function. Left there, a CLI would have
re-implemented it and drifted, and two clients would disagree about which
folders are "the defaults" -- a difference the user would see as a bug in
whichever one they trusted less.

The general rule: **anything two clients must agree on belongs behind the
contract**, even when it is not obviously data.

## What the split actually cost

Very little, which is the point of having done the boundary work first. The
connectors, the store and the auth adapters moved to the daemon unchanged. What
had to be built was transport, lifecycle, and two things the contract turned
out to be missing:

- **A query for current state.** Events are deltas and are never replayed, so a
  client that connects late has no way to learn what it missed.
  `Query::SyncStatus` answers it.
- **Events for client commands.** Only the sync engine published, which was
  invisible while the only client was the one issuing the command.

Both are in [[daemon-and-clients]]. Neither was visible with one in-process
client, and both would have been much more expensive to discover after a third
client existed.

## Known transport limitation

Bodies still go through NDJSON as one JSON string. It works, but it is the
transport's weak point; a separate byte endpoint is the likely answer.

Every connection now starts with a version handshake, including background
requests and event subscriptions. `birdman daemon status | stop | restart`
covers daemon lifecycle operations; `stop` deliberately remains usable across
a protocol-version mismatch.

## Keyboard-first falls out of this

Every operation has a name and typed arguments, so every operation is
addressable from a palette as well as a key. The filterable picker built for
"move to folder" ([[gpui-ui-conventions]]) is the same widget a command palette
needs -- pointed at the operation list instead of at folders.
