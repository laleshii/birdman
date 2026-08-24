---
id: birdman-overview
title: 'Birdman: what it is and how the crates fit together'
altitude: 0
topics:
- architecture
- cli
summary: 'A scriptable mail daemon with a CLI as its primary client: the crate graph, the store-is-truth rule, the client protocol, and the connector boundary that makes the wire protocol swappable.'
---

# Birdman: what it is and how the crates fit together

Birdman is a **mail daemon with clients**, written in Rust. `birdmand` owns the
mailbox — the SQLite cache, the IMAP/SMTP connectors, the sync engine and the
credentials — and clients reach it over a Unix socket speaking
newline-delimited JSON.

**The CLI is the primary client, not a companion to the app.** `birdman` reaches
everything the mailbox can do, and the parity is deliberate: it is what keeps
the protocol honest, and it is checked by feature rather than by tooling. See
[[cli-client]]. `birdman-desktop` is a GPUI application shipped alongside — a
real client, but one of several rather than the product. Anything it can do,
the CLI can do.

Developed and tested on macOS and Linux against Gmail. Early software, actively
developed.

## The rules that shape everything

**1. The store is the single source of truth.** Clients read only through
`birdman-service` and never talk to a mail server -- or to SQLite. A connector
syncs *into* the store; a client re-reads it. This is why the app stays
responsive on a mailbox of ~80,000 messages. See [[ui-sync-store-data-flow]].

**2. No client names a protocol.** They issue `birdman_backend::Command` values
against a `dyn MailReceiver`, and send through a `dyn MailSender`. Which
connector serves each role is declared per account in config, not compiled in.

**3. Nothing the desktop can do is desktop-only.** A capability that reaches the
mailbox belongs in `birdman-proto` and therefore in every client. Where the two
would otherwise derive the same answer separately -- reply-all membership,
`Reply-To` handling -- the derivation is pushed down into `birdman-backend`
(`parsed_from_summary`) so they cannot disagree.
See [[connector-boundary]] and [[account-configuration]].

Both rules are what make **multiple accounts** work: every folder, message and
command is keyed by `birdman_store::AccountId`, and each account carries its own
pair of connectors.

## The crates

| Crate | Role |
|---|---|
| `crates/birdman-store` | SQLite persistence, FTS5 search, keyset paging |
| `crates/birdman-mime` | RFC 822/MIME parsing over `mail-parser` |
| `crates/birdman-proto` | The **client/server contract**: `Query`, `Response`, `Event`. See [[service-boundary]]. |
| `crates/birdman-service` | Answers it. Owns the store and the connectors. |
| `crates/birdman-backend` | The connector *contract*: `Command`, `MailReceiver`, `MailSender`, and the outgoing-message types. Names no protocol. |
| `crates/birdman-auth` | Credential resolution behind a pluggable `AuthAdapter`. Depends on nothing else here. |
| `crates/connectors/birdman-imap` | Receiving, over IMAP |
| `crates/connectors/birdman-smtp` | Sending, over SMTP |
| `crates/birdman-ui` | The GPUI application (binary `birdman-desktop`) |

```
birdman-ui ──► birdman-backend ──► birdman-store ──► birdman-mime
   ├──► connectors/birdman-imap ──► birdman-backend, birdman-auth, birdman-store, birdman-mime
   ├──► connectors/birdman-smtp ──► birdman-backend, birdman-auth, birdman-mime
   └──► birdman-auth, birdman-store, birdman-mime
```

`birdman-ui` depends on the two connectors only to *construct* them in `main.rs`,
`AppState` holds an `Arc<birdman_client::Client>` and names no protocol -- not
even a store handle. Everything goes through `birdmand`; see
[[daemon-and-clients]]. `birdman-smtp`'s apparent dependency on `birdman-imap` is dev-only — a test
asserting a sent message arrives over IMAP.

## Directory layout is deliberate

`crates/connectors/` exists to make "this is one implementation among possible
others" visible without reading any code. The crate was called `mail-core` until
the boundary landed, which hid that it is *entirely* IMAP — `connect`, `idle`,
`session_cache` and `sync` have no meaning for a JMAP or Maildir backend.
`crates/connectors/README.md` is the authoring guide for a new one.

## Building and running

```sh
cargo build && ./target/debug/birdman
```

On macOS the first build needs the Metal toolchain
(`xcodebuild -downloadComponent MetalToolchain`) or shader compilation fails.

Runtime files live in the platform data dir
(`~/Library/Application Support/birdman/` on macOS): `config.toml` for the
account, `mail.db` for the store, `birdman.log` for the log. The password is in
the system keyring, never in the config file. See [[account-configuration]].

## Testing

`cargo test --workspace` runs everything that needs no server. The GreenMail
integration tests under `crates/connectors/birdman-imap/tests/` and
`crates/connectors/birdman-smtp/tests/` are `#[ignore]`d and need a running container. See
[[testing-approach]].

Read [[gpui-ui-conventions]] before touching the UI — several of its rules exist
because breaking them produces silent, hard-to-diagnose misbehaviour rather than
a compile error.
