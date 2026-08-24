---
id: testing-approach
title: How this repo is tested
altitude: 2
topics:
- engineering/practices
relations:
- type: refines
  target: birdman-overview
summary: 'Inline unit tests for pure logic, an in-memory Store for persistence tests, and #[ignore]d GreenMail integration tests — plus the tests that exist to catch copy-paste errors that still compile.'
---

# How this repo is tested

CI runs the complete unit suite, formatting, Clippy with warnings denied, and
supply-chain checks on Linux and macOS. A separate Linux job starts GreenMail
and runs the ignored connector integration tests against it.

## Three tiers

**1. Inline unit tests** — `#[cfg(test)] mod tests` at the bottom of the file
under test, never a separate `tests/` file for unit-level work. Present in
`birdman-store/src/lib.rs`, `birdman-mime/src/lib.rs`, `birdman-backend/src/lib.rs`,
`connectors/birdman-imap/src/sync.rs`, `birdman-backend/src/compose.rs`, and across
`birdman-ui`: `assets.rs`, `compose.rs`, `config.rs`, `cursor.rs`, `logging.rs`,
`root.rs`, `text_input.rs`, `theme.rs`, `webview.rs`. They target pure logic and
invariants that are easy to break silently:

- pagination returning newest-first with no gaps or dupes, filtered and unfiltered
- bodies and attachments actually being lazy
- search deduplication *and* the ID-less-message exception
- sanitize stripping scripts and event handlers while keeping `style`
- `line_and_col` / `offset_for` round-tripping
- every theme token mapping to its own palette field
- the icon table and its call sites agreeing in both directions

The last two are a pattern worth copying: they catch a **copy-paste error that
compiles**. A duplicated match arm or a mistyped asset path produces no error,
just a wrong colour or an invisible button.

**2. In-memory store tests** — `Store::open_in_memory(dir.path())` with a
`tempfile::tempdir()` backing attachment blobs (they're never in SQLite, so a
real directory is still needed). This is the standard fixture:

```rust
fn test_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory(dir.path()).unwrap();
    (store, dir)
}
```

Keep the `TempDir` alive by returning it — dropping it deletes the directory.

**3. Integration tests against a real IMAP/SMTP server** —
`crates/connectors/birdman-imap/tests/greenmail.rs` and
`crates/connectors/birdman-smtp/tests/greenmail_send.rs`. Deliberately a real server
(GreenMail in Docker), not a mock: protocol quirks — what a real FETCH response
looks like, real UIDVALIDITY/UIDNEXT behavior, real IDLE semantics — are exactly
what a hand-rolled mock papers over. The send test proves `mail-builder` output
is well-formed enough for a real server to accept *and deliver*, by fetching the
message back over IMAP.

## Running the integration tests

They're `#[ignore]`d so a normal `cargo test` doesn't require Docker. The
container, per the module docs:

```sh
docker run -d --name birdman-test-greenmail \
  -p 3993:3993 -p 3143:3143 -p 3025:3025 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.users=testuser:testpass@localhost -Dgreenmail.auth.disabled=false' \
  greenmail/standalone:2.1.12
```

Seed the two messages described in `birdman-imap/tests/greenmail.rs`, then run
the full-sync test before the IDLE test; CI contains the executable fixture.
For ad-hoc runs, use `cargo test -p birdman-imap -- --ignored
--test-threads=1` and then `cargo test -p birdman-smtp -- --ignored`. GreenMail uses a self-signed
certificate, which is what `danger_accept_invalid_certs` /
`insecure_tls` exist for — never for a real account.

## Two things to remember

- `cargo test --workspace` works as one command. It did not used to: the Blitz
  renderer pulled `stylo` into the graph and forced a build split. Removing that
  path removed the split too.
- Credential tests use `birdman_auth::StaticAdapter` rather than a real keyring, so
  they don't need a Secret Service provider in a sandbox or CI.
