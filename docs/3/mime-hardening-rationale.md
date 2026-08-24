---
id: mime-hardening-rationale
title: Why birdman-mime refuses to recurse into nested messages
altitude: 3
topics:
- mime
- security
relations:
- type: part_of
  target: mime-parsing
summary: The specific CVE class that motivated the size cap, the part cap, and the deliberate absence of MessagePart::message() calls.
---

# Why birdman-mime refuses to recurse into nested messages

`mail-parser` has no documented size or depth limits of its own. A mail client
parses bytes from arbitrary IMAP servers and arbitrary senders, so
`crates/birdman-mime/src/lib.rs` adds its own bounds. All three are load-bearing;
none is incidental.

## 1. Reject before parsing

`MAX_RAW_MESSAGE_BYTES` = 32 MiB. Checked against `raw.len()` and returning
`ParseError::TooLarge` **before** the bytes are handed to the parser at all — not
after, not during.

## 2. Never descend into `message/rfc822`

The code never calls `mail_parser::MessagePart::message()`. That is the exact
vector of the class of bug in CVE-2026-26312: cyclical references from malformed
nested `message/rfc822` parts causing unbounded CPU and memory consumption when
walked.

Birdman has no legitimate need for it. Only the *outer* message's subject, body,
and attachments are exposed, and that's all the reading pane, search, and reply
builders consume. A forwarded message shows up as its quoted text or as an
attachment, which is what a user sees in most clients anyway.

**If a future change wants "proper forwarded-message support", this is a
decision to re-open deliberately** — with a depth limit and cycle detection — not
a missing feature to fill in.

## 3. Cap part enumeration independently

`MAX_PARTS` = 500, applied via `.take(MAX_PARTS)` on `text_bodies()`,
`html_bodies()`, and `attachments()` — regardless of what the parser reports. A
message claiming tens of thousands of parts costs a bounded amount of work.

## How callers handle failure

`mail-parser` is liberal about malformed input by design and doesn't panic;
`ParseError::NoHeaders` is its only structural failure. `sync.rs` uses
`let Ok(parsed) = birdman_mime::parse_message(header) else { continue };` — one
unparseable message is skipped, and the rest of the folder syncs fine. Preserve
that shape; a `?` there would let a single bad message break a whole folder's
sync.
