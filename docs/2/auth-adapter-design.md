---
id: auth-adapter-design
title: 'Authentication adapters: pluggable credential resolution'
altitude: 2
topics:
- config
- architecture
relations:
- type: references
  target: connector-boundary
- type: references
  target: keyring-credentials
summary: The AuthAdapter trait, why Credentials is an enum rather than a password string, why it resolves per connection, and the three adapters that ship.
---

# Authentication adapters: pluggable credential resolution

`crates/birdman-auth`. A connector asks for the credential belonging to an account
and gets back a `Credentials` value; where it came from — the OS keyring, a
shell command, an environment variable — is the adapter's business.

**Status: built.** Four adapters ship — `keyring`, `command`, `env` and
`oauth2` — and both connectors consume the trait. See [[oauth2-flow]] for the
OAuth2 half.

## Why the trait cannot return a String

The obvious design is "give me the secret for this account, as a string". It
does not work, because **OAuth2 changes the authentication mechanism, not just
the secret.**

A password authenticates over IMAP `LOGIN` or SMTP `AUTH PLAIN`. An OAuth2
access token authenticates over SASL `XOAUTH2`, whose payload has its own
format. A connector handed a bare string has no way to know which to use, and
guessing wrong is an auth failure, not a fallback.

```rust
pub enum Credentials {
    Password(String),
    OAuth2 { username: String, access_token: String },
}
```

Each connector matches on it. `connect_and_authenticate` in
`birdman-imap/src/connect.rs` is the reference: `LOGIN` for a password,
`AUTHENTICATE XOAUTH2` for a token. `birdman-smtp` does the same with
`smtp::Credentials::new` vs `new_xoauth2`.

`Credentials::xoauth2_payload` builds the SASL initial response
(`user=<u>\x01auth=Bearer <t>\x01\x01`). It lives in `birdman-auth` rather than in
either connector because IMAP and SMTP send **identical bytes** — only the
command carrying them differs.

`Debug` for `Credentials` is hand-written to redact the secret while keeping the
username. These values reach log and error paths.

## Why it is async, and resolved per connection

```rust
pub trait AuthAdapter: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn credentials<'a>(&'a self, ctx: &'a AuthContext) -> AuthFuture<'a>;
}
```

An OAuth adapter must refresh an expired token, which is a network call. Async
also turns the keyring's blocking-call hazard from a convention into a
structural fact: `KeyringAdapter` does its own `spawn_blocking` internally,
once, so no caller has to remember. That convention previously lived in a doc
comment on `CredentialProvider::password_for`, and every new caller had to know.

Adapters are consulted **per connection attempt**, never at startup. Two
consequences worth stating:

- A rotated password takes effect on the next reconnect, not the next restart.
- **Nothing downstream stores a secret.** `SmtpConfig` used to carry
  `password: String`, handed over at startup and held for the life of the
  process; that field is gone.

Caching, and deciding when a token is stale, belongs inside the adapter — the
only place the expiry is known.

## The adapters

| `auth.type` | Config | Behaviour |
|---|---|---|
| `keyring` | `username` | OS keyring. The default. |
| `command` | `command = [...]` | runs the program, takes stdout as the secret |
| `env` | `var` | reads an environment variable |
| `oauth2` | `provider`, `client_id`, `client_secret` | refresh token from the keyring, access token per connection ([[oauth2-flow]]) |

`command` is the highest value-per-line of the three: it makes `pass`, `gopass`,
1Password's CLI and anything else with a command-line interface work without
Birdman integrating with any of them.

Two details in `CommandAdapter` that are deliberate:

- Trailing newlines are trimmed. Every one of those tools emits one and none of
  them mean it as part of the secret.
- A failure reports **stderr**, never stdout. Stdout is the secret and must not
  reach an error message. There is a test asserting exactly that.

Empty output is `NotFound`, not an empty password — a silently-empty secret
would surface as a confusing auth rejection.

## Config validation happens on load

`build_account` rejects `type = "command"` with no `command` array, and
`type = "env"` with no `var`. A config error should surface when the file is
read, not on the first attempt to send mail hours later.

## First-run prompting is keyring-only

`main()` checks, before opening any window, whether any **keyring** account has
nothing saved yet, and shows the password screen for the first one. Other
adapters are skipped entirely: there is nothing to prompt for, and asking would
be wrong — the secret lives in `pass` or the environment already.

Multi-account first run therefore takes one pass per keyring account. Acceptable
for one-time setup, and much simpler than a queue of prompts.

## Why the crate is standalone

`birdman-auth` depends on neither `birdman-backend` nor `birdman-store`. A connector
should be able to take a credential without taking the whole command contract,
and an adapter has no business knowing what a `Command` is.

This is a deviation from the original sketch, which had `birdman-backend` re-export
the auth types. Nothing in the command contract carries a credential, so that
dependency would have bought nothing.

## Still open

- **Per-role credentials.** `auth` sits on the account, shared by receiver and
  sender. Right for IMAP + SMTP with one app password, and for JMAP where one
  token serves both; wrong for an outbound relay that authenticates separately.
  The schema can absorb an optional `auth` inside `receiver`/`sender` later
  without breaking anything. Nothing should be built until a real account needs
  it.
