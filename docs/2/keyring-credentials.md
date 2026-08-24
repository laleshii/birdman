---
id: keyring-credentials
title: 'Passwords: the keyring adapter and the first-run prompt'
altitude: 2
topics:
- config
- security
relations:
- type: part_of
  target: account-configuration
- type: part_of
  target: imap-sync-engine
summary: The keyring is one AuthAdapter among several; where the entry lives, why the blocking call is now handled inside the adapter, and how the first-run prompt launches straight into the app.
---

# Passwords: the keyring adapter and the first-run prompt

Account passwords never touch `birdman-store`, SQLite, or the config file. With
`auth = { type = "keyring" }` they live only in the OS keyring (Keychain on
macOS, Secret Service on Linux).

The keyring is now **one adapter among several** rather than the only path —
see [[auth-adapter-design]] for the trait, and for the `command` and `env`
adapters that need no keyring at all.

## Where the entry lives

`birdman_auth::KeyringAdapter::new(service)` reads
`keyring::Entry::new(service, ctx.username)`. The service name is
`KEYRING_SERVICE = "birdman"` in `crates/birdman-ui/src/main.rs`; the username comes
from the account's `auth.username`, which defaults to its `email`.

That coordinate is what makes a config migration safe: as long as
`auth.username` is unchanged, the same keyring entry is found and the user is
not re-prompted — which is exactly what happened when the single-`[account]`
shape was replaced by `[accounts.<id>]`.

## The blocking-call rule is now structural

Keyring calls block — Secret Service is a D-Bus round trip, macOS Keychain can
show an authorization dialog — and can take hundreds of milliseconds, longer if
the keyring is locked.

This used to be a rule in a doc comment: "callers must run it via
`spawn_blocking`". `connect.rs` obeyed it; every new caller had to know. The
`spawn_blocking` now lives **inside `KeyringAdapter`**, once, and the trait is
async, so there is no rule left to break.

## macOS re-prompts after a rebuild

A rebuilt binary has a different code signature, so macOS asks again whether
this application may read the keychain item — `SecurityAgent` appears and the
app blocks until it is answered. This looks exactly like a hang: no window, an
empty log, a live process.

Worth recognising during development. It is not a bug, and "Always Allow"
persists until the next rebuild changes the signature again.

## Storing one: `birdman login`

`birdman_auth::store_password(service, username, password)` — the same
coordinates the adapter reads back.

Storing is deliberately **not** on the `AuthAdapter` trait. Nothing in the sync
or send path ever writes a credential, so putting it there would make every
adapter implement a method only account setup calls.

### Why it is a CLI command and not a screen

The desktop used to own this: a password prompt window shown before the main
one, which then launched the app directly so saving a password connected
immediately.

That stopped making sense once `birdmand` owned the mailbox. The daemon resolves
credentials per connection, so a missing password is a *sync failure*, not a
startup failure — and the app pre-checking the keyring on the daemon's behalf
was a leftover that also meant the **app** had to be running, and be the first
thing you ran, before mail could sync.

Now: `birdman login <account>` writes it, and a credential failure reaches the
desktop as a status line reading *"No password saved — run: birdman login
<account>"*. See [[cli-client]].

Only keyring accounts have anything to store. `command`, `env` and `oauth2`
accounts get their credential elsewhere, so `login` refuses them and points an
OAuth account at `birdman authorize` instead.
