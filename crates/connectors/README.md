# Connectors

A connector implements one of the two traits in
[`birdman-backend`](../birdman-backend/src/lib.rs). They are the only parts of Birdman
that know a wire protocol.

```
crates/birdman-backend             the contract: Command, MailReceiver, MailSender
crates/birdman-auth                credential resolution: AuthAdapter, Credentials
crates/connectors/birdman-imap     receiving, over IMAP
crates/connectors/birdman-smtp     sending, over SMTP
crates/connectors/<yours>       either role, over anything else
```

`birdman-ui` depends on the contract and never names a protocol. Which connector
serves an account is declared in its config, not compiled in.

## Two roles, not one

| Trait | Serves | Method |
|---|---|---|
| `MailReceiver` | folders, sync, bodies, flags, moves, deletes | `execute(Command) -> BackendFuture` |
| `MailSender` | outgoing mail | `send(OutgoingMessage) -> SendFuture` |

They are separate because for IMAP accounts they are separate *servers*, with
different hosts, ports and often credentials. A protocol that does both — JMAP,
the Gmail API, Microsoft Graph — implements both traits on one type. Nothing
forces them apart at runtime.

This used to be one trait with a `Send` command. Every receiver then had to
carry a `Send` arm it could only answer with `Unsupported`, and the pairing
"IMAP here, SMTP there" could not be expressed at all.

## The contract in one paragraph

The UI hands you a `Command` (or an `OutgoingMessage`). You do the work, **write
results into `birdman-store`**, and resolve. The return value carries no mail: the
UI re-reads the store. That is what lets a sync touching thousands of messages
report progress as it goes instead of materialising everything at the end, and
it is why the store — not your crate — defines what a message is.

## Rules

- Depend on `birdman-store`, `birdman-backend`, and `birdman-mime` if you parse MIME.
- **Never depend on `birdman-ui`.** If you want to, what you want probably belongs
  in `Outcome` or in the store.
- `MessageId` / `FolderId` are the shared vocabulary. Translating one to a UID,
  a blob id or a path is your job.
- Bring your own async runtime. `birdman-backend` has no runtime dependency, which
  is why `execute` returns a boxed future rather than being `async fn` — that is
  also what keeps the traits object-safe.
- `OutgoingMessage` and `Recipient` live in `birdman-backend`, not in the SMTP
  crate. They describe what the UI wants sent, not how SMTP sends it.

## Writing a receiver

```rust
use birdman_backend::{boxed, BackendError, BackendFuture, Command, MailReceiver, Outcome};

pub struct JmapReceiver { /* session, store, http client */ }

impl MailReceiver for JmapReceiver {
    fn name(&self) -> &'static str { "jmap" }

    fn execute(&self, command: Command) -> BackendFuture {
        let store = self.store.clone();
        boxed(async move {
            match command {
                Command::SyncFolder { folder } => { /* ...write to store... */ Ok(Outcome::default()) }
                Command::MoveMessage { .. } => Err(BackendError::Unsupported("move message")),
                _ => Err(BackendError::Unsupported("that operation")),
            }
        })
    }
}
```

**Resolve ids before you connect.** Look store ids up on the caller's thread, so
a message deleted between the UI issuing the command and you running it costs
`BackendError::NotFound` and no connection. `ImapBackend::message` is the worked
example.

### What each command must leave true

| Command | Done when |
|---|---|
| `ListFolders` | every folder is in the store via `upsert_folder` |
| `SyncFolder` | the folder's envelopes are current |
| `BackfillBodies` | up to `budget` recent bodies are stored; **bounded** — the caller repeats it |
| `FetchBody` | `store_message_body` has been called |
| `OpenMessage` | body fetched if `fetch_body`, marked read if `mark_read`; both conditional |
| `SetFlags` | flags applied remotely *and* via `set_flags` locally |
| `MoveMessage` | the message is in `to_folder`, server and store agreeing |
| `DeleteMessage` | gone, by whatever "deleted" means for your protocol |

## Writing a sender

```rust
use birdman_backend::{boxed_send, BackendError, MailSender, OutgoingMessage, SendFuture};

impl MailSender for JmapSubmission {
    fn name(&self) -> &'static str { "jmap" }

    fn send(&self, message: OutgoingMessage) -> SendFuture {
        let session = self.session.clone();
        boxed_send(async move {
            session.submit(message).await.map_err(|e| BackendError::Failed(e.to_string()))
        })
    }
}
```

`birdman-smtp` is ~40 lines of trait impl over an existing `send` function; a
sender is a much smaller job than a receiver.

## Registering it

A connector is reachable from config by its declared type. Add a variant and an
arm:

1. `crates/birdman-ui/src/config.rs` — add to `ReceiverKind` or `SenderKind` and
   its `parse`.
2. `crates/birdman-ui/src/main.rs` — add the arm that constructs it.

```toml
[accounts.fastmail]
email = "me@fastmail.com"
receiver = { type = "jmap", host = "api.fastmail.com" }
sender   = { type = "jmap", host = "api.fastmail.com" }
auth     = { type = "keyring", username = "me@fastmail.com" }
```

The type is declared rather than implied by a key name (`imap_host`) precisely
so that adding a protocol does not mean inventing a key prefix.

## Credentials

Never take a password. Take an `Arc<dyn birdman_auth::AuthAdapter>` and ask it,
**per connection attempt**:

```rust
let ctx = AuthContext { account_id: ..., username: ... };
match auth.credentials(&ctx).await? {
    Credentials::Password(password) => /* LOGIN, AUTH PLAIN */,
    Credentials::OAuth2 { username, access_token } => /* SASL XOAUTH2 */,
}
```

Two rules that are not obvious:

- **Match on the variant, do not unwrap a string.** OAuth2 changes the
  *mechanism*, not just the secret: a password goes over `LOGIN`, a token over
  SASL `XOAUTH2` with a payload of its own
  (`birdman_auth::Credentials::xoauth2_payload` builds it — IMAP and SMTP use the
  identical bytes). A connector that treats a token as a password will fail to
  authenticate, not fall back.
- **Resolve per connection, never at construction.** That is what lets an OAuth
  adapter refresh an expired token, and it means a changed password takes effect
  on the next reconnect rather than at the next restart. Caching belongs inside
  the adapter, which is the only place an expiry is known.

A connector that cannot do a given mechanism should return
`BackendError::Unsupported`, not fail obscurely.

## Errors

| Variant | Use for |
|---|---|
| `Unsupported(&'static str)` | your protocol can never do this. The UI shows a *limitation*, not a failure. |
| `NotFound(String)` | the message or folder is gone |
| `Failed(String)` | everything else: network, auth, protocol |

`Failed` is a `String` on purpose. The UI must never match on a
protocol-specific error type, so your crate keeps its own — `birdman-imap` keeps
`CoreError` entirely to itself.

**Partial implementations are legitimate.** A read-only archive reader that
supports only `ListFolders`, `SyncFolder` and `FetchBody` is a working
connector, not a broken one.

## Testing without a server

`execute` takes a value and returns a future, so a connector is testable with no
network at all. `birdman-backend`'s own tests include a `RecordingBackend` doing
exactly this.

## What is not behind the boundary yet

- **Background sync.** `birdman_imap::spawn` runs IDLE and emits `SyncEvent`. There
  is no generic equivalent, so a new receiver must supply its own and `main.rs`
  must be edited to use it.
- **`AccountConfig`.** IMAP-shaped: host, port, keyring ref.

Commands are fully abstracted; the *lifecycle* around them is not.
