---
id: connector-boundary
title: 'The connector boundary: how the UI stays protocol-agnostic'
altitude: 1
topics:
- architecture
- connectors
relations:
- type: refines
  target: birdman-overview
- type: depends_on
  target: local-message-store
summary: birdman-backend's MailReceiver/MailSender contract, why commands are an enum but sending is not, how commands route per account, and what is still not behind the boundary.
---

# The connector boundary: how the UI stays protocol-agnostic

`crates/birdman-backend` defines the contract between the UI and whatever talks to
a mail server. `crates/connectors/birdman-imap` implements it over IMAP. Adding
JMAP, the Gmail API, Microsoft Graph or a Maildir reader is a new crate under
`crates/connectors/` plus one line in `main.rs` — no UI change.

## The contract

```rust
pub trait MailReceiver: Send + Sync + 'static {
    fn execute(&self, command: Command) -> BackendFuture;
    fn name(&self) -> &'static str;
}
```

`Command` is an enum: `ListFolders`, `SyncFolder`, `BackfillBodies`,
`FetchBody`, `OpenMessage`, `SetFlags`, `MoveMessage`, `DeleteMessage`, `Send`.
Every variant names *what should be true afterwards*, not how to get there —
`MoveMessage`, not "UID MOVE to All Mail" — so a backend whose archive is a
label change or a file move can honour it its own way.

## Why a command enum rather than one trait method per operation

A wide trait grows a method every time the UI learns a new trick, and every
backend then has to implement it, including ones that cannot. A single
`execute(Command)` lets a backend match on what it supports and return
`BackendError::Unsupported` for the rest. It also makes operations *values*, so
they can be logged, queued, retried or replayed uniformly — which is what
`AppState::dispatch` exploits.

## Results arrive twice, deliberately

`execute` resolves when the work finishes; that is what the UI awaits in order
to report failure. **The data arrives separately**, by the backend writing into
`birdman-store` and the UI re-reading it. `Outcome` carries only
`bodies_fetched`, for progress reporting.

Keeping bulk data out of the return value is what lets a sync touching thousands
of messages report progress incrementally instead of materialising everything at
the end. It is the same rule as [[ui-sync-store-data-flow]], applied to the
command direction.

## Errors are deliberately flattened

| Variant | Meaning |
|---|---|
| `Unsupported(&'static str)` | this backend can never do this. The UI presents a *limitation*, not a failure. |
| `NotFound(String)` | the message or folder is gone |
| `Failed(String)` | everything else: network, auth, protocol |

`Failed` holds a `String`, not a source error. The UI must never match on a
protocol-specific error type, so each connector is free to have its own —
`birdman-imap` keeps `CoreError` entirely to itself.

Because `Unsupported` is a first-class outcome, **a partial implementation is a
legitimate connector**, not a broken one: a read-only archive reader that
supports only `ListFolders`, `SyncFolder` and `FetchBody` works.

## Commands are routed per account

With several accounts configured, a command must reach the right connector.
`AppState` holds one `AccountRuntime` per account — store id, display name,
email, receiver, sender — and routes by `birdman_store::AccountId`.

The account is resolved from `self.folders`, which is already in memory, so
routing costs no store lookup:

```rust
pub fn account_of_folder(&self, folder_id: FolderId) -> Option<AccountId>
```

Anything that picks a *target* folder must be account-scoped too.
`special_folder` takes an `AccountId` for exactly this reason: with two accounts
`self.folders` holds two Trash folders, and archiving a work message into a
personal Trash would be real data loss rather than a cosmetic bug.

## One dispatch path in the UI

`AppState::dispatch(command, cx, on_done)` in `crates/birdman-ui/src/state.rs` is
the only place the UI invokes a backend. It replaced six near-identical
copies of "connect, select mailbox, apply timeout, invalidate session on error",
each of which was free to get the error handling subtly wrong — and did.

```rust
fn dispatch(&self, command: Command, cx: &mut Context<Self>,
            on_done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static)
```

Failures land on the status line using `Command::describe()`, which is phrased
in the UI's voice ("open message failed"), not the protocol's.

## Object safety drives two API choices

The UI holds `Arc<dyn MailReceiver>`, so `execute` cannot be an `async fn`. It
returns `BackendFuture` (a `Pin<Box<dyn Future + Send>>`); `birdman_backend::boxed`
is the helper connectors use. `birdman-backend` also has **no runtime dependency**
— a connector brings its own. `ImapBackend` holds a
`tokio::runtime::Handle` and does the `runtime.spawn` internally so callers
never have to.

## Resolve ids before connecting

Commands address messages by `birdman_store::MessageId`. `ImapBackend::execute`
resolves the id to `(account, mailbox, uid, flags)` **up front on the caller's
thread**, before spawning, so a message deleted between the UI issuing the
command and the backend running it costs `NotFound` and no connection at all.
`ImapBackend::message` is the worked example.

## OpenMessage is fused on purpose

`OpenMessage { message, fetch_body, mark_read }` exists rather than composing
`FetchBody` + `SetFlags` because a backend can do both against a single
connection, and splitting them doubles the round trips on the most
latency-sensitive action in the app. Both halves are conditional, so re-opening
an already-read message with a cached body issues no work.

## Testability is the other half of the point

`execute` takes a value and returns a future, so `AppState` can be driven with
no mail server. `birdman-backend`'s own tests include a `RecordingBackend` that
does exactly this.

## What is NOT behind the boundary yet

Know this before assuming a connector is only a `MailReceiver` impl. `main.rs`
still names `birdman_imap` directly for:

- **Background sync.** `birdman_imap::spawn` runs IDLE and emits `SyncEvent`
  ([[sync-supervisor-loop]]). There is no generic equivalent.
- **`AccountConfig`.** IMAP-shaped: host, port, keyring ref.
- **The Tokio handle.** `AppState` takes one from the IMAP engine.

Commands are fully abstracted; the *lifecycle* around them is not. That is the
next piece of work for anyone extending this.

## Sending is a second role, not a command

`MailSender` is a separate trait:

```rust
pub trait MailSender: Send + Sync + 'static {
    fn send(&self, message: OutgoingMessage) -> SendFuture;
    fn name(&self) -> &'static str;
}
```

`Command::Send` used to exist in the enum and was never dispatched — the UI
called SMTP directly. The fix was not to wire that arm up but to split the
trait, because the enum's payoff (a backend declining arms it cannot serve)
buys nothing for an operation with exactly one variant, and a single trait could
not express "IMAP here, SMTP there" at all.

For IMAP accounts the two roles are genuinely different servers, with different
hosts, ports and often credentials, which is why config declares them
independently. A protocol that does both — JMAP, the Gmail API, Graph —
implements both traits on one type. Nothing forces them apart at runtime.

`SendFuture` is distinct from `BackendFuture` because a send has no `Outcome` to
report: it either went out or it did not.

`OutgoingMessage`, `Recipient` and the draft builders live in `birdman-backend`
rather than in the SMTP crate. Once sending went behind a trait, the contract
needed the message type in its own signature, and a contract cannot import from
one of its implementations.

See [[outgoing-mail]].
