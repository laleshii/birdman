---
id: message-schema-and-lazy-bodies
title: Envelopes, lazy bodies, and on-disk attachments
altitude: 2
topics:
- storage
relations:
- type: part_of
  target: local-message-store
summary: Why messages and message_bodies are separate tables, how body_fetched drives the lazy fetch path, and why attachment bytes live on disk content-addressed.
---

# Envelopes, lazy bodies, and on-disk attachments

The central storage decision in `crates/birdman-store/src/lib.rs`.

## Two tables, two costs

`messages` holds envelope data — subject, from, to, cc, date, flags,
`message_id_header`, `refs_header`, `has_attachments` — all of it derivable from
a cheap `BODY.PEEK[HEADER]` fetch. `message_bodies` holds `text_body` and
`html_body`, which require the expensive full `BODY.PEEK[]`.

Keeping them apart means **syncing a folder's envelope list never implies
downloading every message body**. On an account with tens of thousands of
messages that's the difference between a usable first sync and an unusable one.

`messages.body_fetched` is the flag that ties the two together:

- `upsert_message_envelope` writes the envelope and does *not* touch the body.
- `store_message_body` writes the body, its attachments, and sets
  `body_fetched = 1`.
- Everything downstream branches on it. `MessageSummary::body_fetched` reaches
  the UI, and `AppState::select_message` uses it to decide whether to show the
  cached body immediately or kick off an on-demand fetch.

## Attachment bytes are files, not BLOBs

`store_message_body` writes each attachment's contents to
`<data_dir>/attachments/`, content-addressed by SHA-256 (`sha2`), and stores only
metadata plus `cached_path` in the `attachments` row. The SQLite file stays
small.

Content-addressing has one consequence worth knowing: `delete_message` removes
the message row and, via `ON DELETE CASCADE`, its body and attachment
*metadata* — but **intentionally does not delete the blob on disk**, since other
messages may share the same content-addressed file. There is currently no
garbage collection for orphaned attachment blobs.

## Inline vs. regular attachments

`attachments.is_inline` and `content_id` distinguish `cid:`-referenced images
embedded in an HTML body from real file attachments. `get_inline_attachments`
returns only the inline ones — everything an `<img src="cid:...">` might point
at. Regular attachments are listed separately by the reading pane rather than
embedded. See [[remote-and-inline-images]] for what the UI does with them.

## Keyset pagination

`list_messages_page(folder_id, after: Option<PageCursor>, limit)` orders by
`date DESC, id DESC` and pages with `WHERE (date, id) < (?, ?)` against
`idx_messages_folder_date`. Not `OFFSET` — scrolling deep into a large mailbox
stays cheap either way with keyset, and degrades linearly with `OFFSET`.

The API is fully built; the UI doesn't use it yet. `AppState::refresh_messages`
fetches a single flat page because there's no infinite-scroll UI. Wiring that up
is a UI change, not a store change.
