---
id: reply-forward-draft-semantics
title: 'Reply and forward drafts: threading headers and a lossy round-trip'
altitude: 2
topics:
- sending
relations:
- type: part_of
  target: outgoing-mail
- type: depends_on
  target: mime-parsing
summary: 'How Re:/Fwd: subjects, In-Reply-To/References, and reply-all recipient filtering are built — and why the UI''s reconstructed ParsedMessage loses display names and CC/To structure.'
---

# Reply and forward drafts: threading headers and a lossy round-trip

`crates/connectors/birdman-smtp/src/compose.rs` builds a `ComposeDraft` from a
`ParsedMessage`. Pure functions, no I/O, unit-tested without a server.

## Subject prefixing

`prefixed_subject` compares **case-insensitively** and prepends only if the
subject doesn't already start with the prefix — so replying to "Re: Lunch?"
gives "Re: Lunch?", not "Re: Re: Lunch?". A missing subject becomes just
`"Re: "`. There's a test for the no-double-prefix case.

Note it's a simple `starts_with`, so `"RE:"` and `"Re :"`-style variants from
other clients aren't normalized, and localized prefixes ("AW:", "SV:") aren't
recognized.

## Threading headers

`reply_draft` sets:

- `in_reply_to` = the original's `Message-ID`
- `references` = the original's `References`, with its `Message-ID` appended if
  not already present (the dedup check matters — some servers already include it)

`forward_draft` sets **neither**, deliberately: a forward starts a new thread for
the new recipient.

`mail-send::send` only writes these headers when present (`if let Some(...)`,
`if !is_empty()`), so a fresh compose emits no threading headers at all.

## Reply-all recipient rules

Plain reply → `to` is the original's `From` only, `cc` empty.

Reply-all → `to` is `From` plus the original's `To`, and `cc` is the original's
`Cc`; both filtered against `self_address` with `eq_ignore_ascii_case`. The
filter is by address only — no alias awareness, so replying-all from an alias
will include your own canonical address.

## Body quoting

`quote_body` produces `"On {date}, {from} wrote:"` followed by the plaintext body
with `"> "` prefixed to each line. `forward_draft` instead inlines the original's
`From`/`Date`/`Subject`/`To` under a `---------- Forwarded message ----------`
banner, unquoted.

Both read `text_body` only. **An HTML-only message quotes as empty** — there's no
HTML-to-text conversion anywhere in the codebase.

Dates are formatted by hand via `time::OffsetDateTime` as
`YYYY-MM-DD HH:MM` in **UTC**, with an unparseable timestamp yielding an empty
string rather than an error.

## The lossy round-trip through the UI

The UI doesn't keep a `ParsedMessage` around; `AppState::to_parsed_message`
rebuilds one from a stored `MessageSummary` plus `selected_body`. Three things
are lost, and they're visible in a reply:

1. **`To`/`Cc` display names.** `MessageSummary::to_addrs`/`cc_addrs` are
   comma-joined strings, documented as "good enough for reply-all and display;
   not split into structured mailboxes". `split_addrs` splits on `,` and sets
   every `Mailbox::name` to `None`. Only the sender's name survives, via
   `from_name`.
2. **A comma inside a quoted display name** would split into bogus addresses —
   the join/split pair isn't RFC-aware.
3. **`in_reply_to`** is left at `Default` (empty) because it's `..Default::default()`
   in the constructed struct. Only `references` is carried, reconstructed by the
   store's row mapper splitting `refs_header` on whitespace. This doesn't affect
   the reply's own threading headers, since `reply_draft` derives `in_reply_to`
   from the original's `Message-ID`, which *is* carried.

Also note the body: `to_parsed_message` uses `self.selected_body`, so a draft
built while the body is still loading quotes nothing.

## Sending from the compose window

`ComposeView::send` parses the To/Cc text fields back into `Recipient`s
(`parse_recipients` — plain comma splitting, no display-name parsing) and calls
`birdman_smtp::send` on birdman-imap's Tokio runtime. Outgoing bodies are plaintext
only. Sent mail is **not** appended to the Sent folder — no IMAP `APPEND` exists
anywhere in the codebase, so a sent message only appears locally after the
server files it and the next sync picks it up.

## A reply goes to `Reply-To`, not to `From`

RFC 5322 gives the header exactly that meaning, and ignoring it sends a reply
to a mailing list's posting address back to one subscriber, or to a `no-reply@`
box nobody reads.

`reply_draft` prefers it when the sender set one. Reply-all additionally drops
anyone `Reply-To` already named, or a list that also appears in `To` would be
addressed twice.

Getting it working took three layers, and the one that bit was the last:
`birdman-mime` parses it, `birdman-store` keeps it in `reply_to_addrs`, and
`AppState::to_parsed_message` rebuilds a `ParsedMessage` from the stored row.
Miss that last hand-off and the header is parsed, stored, and dropped one layer
before it is used -- which is what happened, silently, because nothing fails
when a struct field is left at its default. The same applies to `bcc`.

Existing mail carries `NULL` for both until its envelope is re-fetched.

## Addressing a new message

`Store::contacts` aggregates every address in the mailbox -- `from`, `to`, `cc`,
`bcc` -- deduped case-insensitively, ranked by how often you have corresponded
and then by recency.

One scan rather than a `contacts` table: a table would need maintaining on
every envelope write, backfilling for mail already synced, and would be a
second copy of what the messages already say. The scan is ~190ms over 11,500
messages, and the compose window asks **once** when it opens, then filters in
memory. A round trip per keystroke could not keep up with typing.

Names come from `from_name` alone, the only place the store keeps one -- `to`,
`cc` and `bcc` are stored as bare addresses. That is the right bias: the name
you know somebody by is the one they send under.

Two exclusions, both learned from the first run: recipients already on the
draft, and the address the draft is being sent *from*. The latter is the single
most frequent address in the mailbox by a wide margin -- 74,857 appearances
against the next one's 4,993 -- so it would head every suggestion list, in the
one place it is never the answer.
