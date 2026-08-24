---
id: mime-parsing
title: 'birdman-mime: parsing raw messages, defensively'
altitude: 1
topics:
- mime
relations:
- type: refines
  target: birdman-overview
summary: A thin hardened wrapper over mail-parser producing a flat owned ParsedMessage, with explicit size and part-count caps and no recursion into nested message/rfc822.
---

# birdman-mime: parsing raw messages, defensively

`crates/birdman-mime` wraps `mail-parser` and exposes exactly one entry point:
`parse_message(raw: &[u8]) -> Result<ParsedMessage, ParseError>`. No I/O, no
async, no dependencies on anything else in the workspace.

## What it produces

`ParsedMessage` is intentionally **flat and owned**: subject, `from`/`to`/`cc`
as `Vec<Mailbox>`, a `date` as a Unix timestamp, `message_id`, `in_reply_to`,
`references`, `text_body`, `html_body`, and `attachments: Vec<Attachment>`. No
borrowed lifetimes leak out of the parser, so callers can store it, send it
across threads, and hand it to `birdman-store` without ceremony.

Only the *first* text body and *first* HTML body are kept — this is a mail
client's reading pane, not a MIME inspector.

## The hardening is the point of this crate

This isn't a passthrough. Two guards are load-bearing and documented as
deliberate in the module header:

1. `MAX_RAW_MESSAGE_BYTES` (32 MiB) rejects oversized input **before** the
   parser sees it, returning `ParseError::TooLarge`.
2. The code **never** calls `mail_parser::MessagePart::message()` to descend
   into nested `message/rfc822` parts. That is the exact vector of the class of
   bug in CVE-2026-26312 — cyclical references from malformed nested parts
   causing unbounded CPU/memory when walked. Birdman has no need to recurse into
   forwarded messages: only the outer message's subject, body, and attachments
   are exposed.

Body and attachment iteration is additionally capped at `MAX_PARTS` (500),
independent of what the parser claims to have found.

If you extend this crate, do not add recursion into nested messages to "support
forwarded mail properly" without deliberately re-litigating this decision —
see [[mime-hardening-rationale]].

## Who calls it

- `birdman-imap`'s `sync_folder` on the `BODY.PEEK[HEADER]` bytes (envelope only)
- `birdman-imap`'s `fetch_message_body` on the full `BODY.PEEK[]`
- `birdman-store::upsert_message_envelope` / `store_message_body` take a
  `&ParsedMessage` as their input shape
- `mail-send`'s draft builders read a `ParsedMessage` to construct replies

`mail-parser` is liberal about malformed input by design and doesn't panic;
`ParseError::NoHeaders` is the only structural failure it reports. Callers in
`sync.rs` use `let Ok(parsed) = ... else { continue }` — a single unparseable
message skips rather than failing the whole folder sync.
