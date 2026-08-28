---
id: imap-fetch-streams-must-be-drained
title: An abandoned FETCH stream desynchronises the IMAP session
altitude: 3
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
---

# An abandoned FETCH stream desynchronises the IMAP session

`crates/connectors/birdman-imap/src/sync.rs`, `fetch_message_body`.

## The rule

An IMAP FETCH response ends at its tagged completion line. A response stream must
be consumed to the end before the session is used again — dropping it early
leaves the remainder in the connection buffer, and the **next** command reads
that as its own reply. The session is then permanently one response behind.

`fetch_message_body` took the first item and dropped the stream, so every body
backfill after the first came back with the previous message's body. The two bulk
fetches in the same file always consumed theirs with `while let` and were never
affected — the single-item path is the one that looks like it doesn't need to.

## How it presents

Nothing is corrupted: the `Message-ID` guard in the same function catches every
mismatch and refuses to store the body. What you see instead is that backfilled
bodies never arrive, plus a stream of `refusing body for message …` errors.

The signature that identifies it, as opposed to any other cause of a mismatch:
**each refusal's `expected` Message-ID is the next refusal's `server returned`
value.** That chain held for 18 of 18 consecutive pairs. A genuinely wrong
mailbox produces unrelated ids, not a chain.

The guard's message used to assert "the wrong mailbox was selected", which is one
possible cause but was not this one, and the misdirection cost real time. It now
states only what it refused to do.

## Why it survived the integration test

`tests/greenmail.rs` fetched exactly one body. The first fetch on a session is
always correct — the desync only shows from the second onwards. The test now
fetches a second body on the same session and asserts whose body came back.

Note the two seeded messages differ: message 2 is multipart, so its text part
keeps the newline before the MIME boundary (`"Second message body.\r\n"`), while
message 1 is single-part and has none (`"First message body."`). Assuming
symmetry there is a trap.
