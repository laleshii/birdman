---
id: ui-sync-store-data-flow
title: 'Data flow: the service owns state, clients re-read it'
altitude: 1
topics:
- architecture
relations:
- type: refines
  target: birdman-overview
summary: 'The core rule: nothing hands mail data to a client. The service writes to the store and announces what changed; clients re-query.'
---

# Data flow: the service owns state, clients re-read it

The most load-bearing rule in Birdman: **nothing hands mail data to a client**.
It is written into `birdman-store`, and the client is told only that something
changed.

It now applies at two boundaries that used to be one.

## Inside the service: connectors write, they do not return

A connector's `execute` resolves when the work is done; the *data* arrives by
the connector writing to `birdman-store`. `Outcome` carries only
`bodies_fetched`, for progress. See [[connector-boundary]].

This is what lets a sync touching thousands of messages report progress as it
goes instead of materialising everything at the end.

## Across the boundary: events name what changed

`birdman_proto::Event` is the entire vocabulary between the service and a
client:

- `FoldersChanged { account }`
- `MessagesChanged { folder }`
- `SyncProgress { account, folder }`
- `SyncFailed { account, message }`
- `SyncIdle { account }`

No bodies, no envelope structs, no bulk payloads. A client reacts by issuing a
`Query`. See [[service-boundary]].

The IMAP supervisor's own `SyncEvent` is a *different type*, translated at the
wiring. It describes what the supervisor did; `Event` describes what a client
should re-read. A connector for another protocol emits different internals and
lands on the same handful of facts.

## Why it is built this way

There is exactly one source of truth, so there is no class of bug where an
in-memory list and the database disagree. A client that misses events, starts
late, or falls behind converges by re-querying -- it never has to replay a
backlog to become correct.

The one deliberate exception is `SyncProgress`'s folder name, which is a
progress string with no persistent home rather than mail content.

## Practical consequence when adding a feature

1. Persist it in `birdman-store` (schema + a query method).
2. Have the connector write it during sync.
3. Expose it as a `Query`, if a client needs to read it.
4. Emit (or reuse) an `Event` that says "this changed".
5. Have the client re-read it.

Do **not** widen `Event` to carry the data itself. The temptation appears every
time step 3 feels like too much ceremony for one field; the cost of giving in
is a second copy of the store that is wrong in ways nothing detects.
