---
id: oauth2-flow
title: 'OAuth2 for Gmail: the consent flow, the refresh loop, and the traps'
altitude: 2
topics:
- config
- security
relations:
- type: part_of
  target: auth-adapter-design
- type: references
  target: keyring-credentials
summary: How birdman authorize obtains a refresh token over a loopback redirect, how the adapter exchanges it per connection, and the Google-specific gotchas that look like bugs.
---

# OAuth2 for Gmail: the consent flow, the refresh loop, and the traps

`crates/birdman-auth/src/oauth2.rs`. Two halves that run at very different times.

## Authorization: once, from a terminal

`birdman authorize <account>` — a subcommand handled in `main()` before gpui
starts. No window, no store, no sync engine.

It is a terminal command on purpose. Consent is a browser round trip that must
finish before the app can authenticate at all, and driving it from a terminal
makes the failure modes — a refused grant, a mistyped client id, an unpublished
consent screen — visible instead of buried behind a spinner.

The flow:

1. Bind a `TcpListener` on `127.0.0.1:0`; the OS picks the port.
2. Build the authorize URL with PKCE `code_challenge`, a random `state`,
   `access_type=offline` and `prompt=consent`.
3. Caller opens the browser. **`begin_authorization` does not** — it returns the
   URL and the caller decides how to present it. A library that writes to
   stdout is a library that cannot be reused.
4. Wait for the redirect, verify `state`, exchange the code for tokens.
5. Store the **refresh token** in the keyring as `oauth2:<username>` — prefixed
   so it can never collide with a password entry for the same address.

### Why loopback

Google shut off the out-of-band redirect (`urn:ietf:wg:oauth:2.0:oob`) in 2022.
A desktop app has exactly one supported option left. Google special-cases
`http://127.0.0.1` and **ignores the port when matching**, which is what lets
the port be whatever the OS hands out rather than something registered ahead of
time.

### Why PKCE despite a client secret

A "client secret" issued for a Desktop-app client is embedded in software the
user can read, and Google documents it as not confidential. It cannot be the
thing protecting the exchange; PKCE is. Birdman always sends it.

`pkce_challenge` is pinned by the RFC 7636 appendix B test vector. If that
drifts, every authorization fails with an opaque `invalid_grant`.

## Refresh: on every connection

`OAuth2Adapter::credentials` returns a cached access token if one is live,
otherwise reads the refresh token from the keyring and POSTs
`grant_type=refresh_token`.

`EXPIRY_SKEW` is 120s: a token expiring inside that window is treated as already
gone, so one cannot lapse mid-connection. There is a unit test for exactly that
boundary.

**The access token never touches disk.** It lives in the adapter's `Mutex` and
dies with the process. Only the refresh token is persisted.

## The traps

### Testing mode expires refresh tokens after 7 days

The one that will look like a bug. While a Google Cloud project's consent screen
is in **Testing**, refresh tokens are revoked after seven days, so Birdman stops
authenticating every week.

Fix: set publishing status to **In production**. That does *not* require
submitting for verification — it only means users see a "Google hasn't verified
this app" interstitial, which *Advanced → Go to … (unsafe)* passes.

### The scope must be `https://mail.google.com/`

The narrower `gmail.readonly` / `gmail.send` scopes are for the REST API. They
authenticate fine over XOAUTH2 and then fail every IMAP command, which reads as
a broken mailbox rather than a scope problem.

### `access_type=offline` *and* `prompt=consent`

Without both, Google returns no refresh token on a repeat authorization — only
an access token — and the flow silently produces nothing durable. The adapter
turns that into an explicit error naming
`myaccount.google.com/permissions`, because the fix is to revoke the existing
grant and retry.

### XOAUTH2 failure needs an empty second response

Gmail does not simply reject a bad token. It sends a base64 JSON error challenge
and expects an **empty** client response before it will return the tagged `NO`.
Re-sending the payload hangs the exchange.

`XOAuth2` in `birdman-imap/src/connect.rs` is therefore `Option<String>` and
`process` does `take()`: the payload once, empty strings afterwards.

## Birdman ships no client credentials

There is no registered Birdman OAuth client, so each user creates their own
Desktop-app client in the Google Cloud console and puts the id and secret in
their account config. The README has the steps.

This is the honest position for a project with no organisation behind it. A
shipped client id would put every user's quota and consent screen under one
unverified project.
