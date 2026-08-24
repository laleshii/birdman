---
id: folder-and-uid-sync
title: Folder listing and per-folder UID sync
altitude: 2
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
- type: depends_on
  target: local-message-store
summary: sync_folder_list handles NoSelect and SPECIAL-USE; sync_folder handles UIDVALIDITY invalidation, incremental UID ranges, PEEK-only fetches, and full flag reconciliation.
---

# Folder listing and per-folder UID sync

`crates/connectors/birdman-imap/src/sync.rs`.

## `sync_folder_list`

Issues `LIST "" "*"`, upserts **every** returned name into `birdman-store`, but
returns only the *selectable* ones — entries carrying `NameAttribute::NoSelect`
are real nodes in the mailbox tree that can't be `SELECT`ed or synced, so they're
stored (the sidebar may want them) but excluded from the sync loop.

`special_use_from_attribute` maps RFC 6154 `SPECIAL-USE` attributes (`\Drafts`,
`\Sent`, `\Flagged`, `\Junk`, `\Trash`, `\Archive`, `\All`) to
`birdman_store::SpecialUse`. Confirmed present on a real Gmail account's plain
`LIST` response — no explicit `LIST (SPECIAL-USE)` request needed, Gmail
advertises them by default. This is what lets the sidebar recognize "the Sent
folder" across providers instead of pattern-matching path strings like Gmail's
`[Google Mail]/Sent Mail`. `\Inbox` is deliberately **not** mapped: RFC 6154
doesn't define it, and INBOX is identified by its own reserved,
case-insensitive name.

## `sync_folder`: the four steps

1. **`SELECT` and check UIDVALIDITY.** If the server's `uid_validity` differs
   from what's stored, the server rebuilt the folder and every cached UID is
   meaningless — `clear_folder_messages(folder_id)` wipes them, forcing a full
   resync. Then `set_folder_uid_state` records the new values. Note the
   division of responsibility: `set_folder_uid_state` only *records*; detecting
   and handling the change is the caller's job, done right here.
2. **Incremental fetch.** `start_uid = max_uid + 1` (or 1 if empty), then
   `UID FETCH <start>:* (UID FLAGS BODY.PEEK[HEADER])`. Only envelopes.
3. **Guard against echoed-back UIDs.** Some servers echo UIDs outside the
   requested range when the mailbox is empty or the range is degenerate;
   `if uid < start_uid { continue }` drops them. Don't remove this.
4. **Reconcile flags** over the whole mailbox — see below.

Unparseable headers and missing UIDs are skipped with `let ... else { continue }`
rather than failing the folder.

## `BODY.PEEK`, never `BODY`

Every content fetch in this crate uses `BODY.PEEK[...]`. Plain `BODY[...]` would
set `\Seen` as a side effect, meaning *syncing* would silently mark mail as
read. Marking read is an explicit, separate action taken only when the user
actually opens a message (in `AppState::select_message`). This invariant is
stated in the doc comments of both `sync_folder` and `fetch_message_body` —
preserve it in any new fetch.

## Flag reconciliation is currently O(mailbox)

`reconcile_flags` runs a `UID FETCH 1:* (UID FLAGS)` pass every sync to pick up
changes made elsewhere (read on another client, flagged, etc.). It's cheap per
message but linear in mailbox size. The code names the proper fix explicitly:
CONDSTORE/QRESYNC when the server supports it. This MVP just re-fetches
everything, every sync.

## Mutating operations

- `set_flags_remote` issues `UID STORE ... FLAGS (...)` — a **replace, not a
  merge**. Callers must pass the complete target flag set. It mirrors into
  `birdman-store` only on success.
- `move_message_remote` tries `UID MOVE` (RFC 6851) and falls back to
  copy-then-delete. **Copy-first ordering is load bearing**: deleting first and
  failing the copy loses the message outright.
- `delete_message_remote` **moves to Trash**, it does not expunge. `\Deleted` +
  `EXPUNGE` means "remove from this mailbox", and what happens next is the
  server's choice — Gmail's default IMAP setting *archives*, so the mail stayed
  in All Mail and Trash never saw it. It resolves the account's `\Trash`
  folder by SPECIAL-USE and moves there. Expunging is right in exactly two
  cases: the message is already in Trash (emptying the bin), or the account has
  no Trash folder at all.
- Expunging goes through `expunge_uid`, which issues **`UID EXPUNGE`
  (RFC 4315)**, falling back to a bare `EXPUNGE` only when the server lacks
  UIDPLUS. A bare `EXPUNGE` removes *every* message carrying `\Deleted`, and
  that flag is shared state — another client can have left it on mail nobody
  meant to destroy.
- `fetch_message_body` fetches `BODY.PEEK[]` for one UID and stores the parsed
  result; `CoreError::MessageMissing` if the server returns nothing.

## `FolderSyncResult`

Returns both `new_uids: Vec<u32>` (for the `NewMessages` event) and
`new_messages: Vec<(MessageId, u32)>` (same messages paired with the row they
landed at, so the eager-prefetch pass in [[sync-supervisor-loop]] doesn't need a
second lookup). Ascending UID order.

## A missing UIDVALIDITY is not a changed one

The single most destructive line this codebase has had:

```rust
let server_uid_validity = mailbox.uid_validity.unwrap_or(0);
```

UIDVALIDITY changing means the server reissued its uids and every stored uid is
meaningless, so the folder is cleared and re-downloaded. That is correct. But
`unwrap_or(0)` turned "the `SELECT` response did not carry one" into the *value
zero*, which cannot equal any stored validity -- so a dropped or unparsed
response wiped the folder. An INBOX went from 8,824 messages to 329.

Worse, the zero was then **stored**, so the next sync compared a real validity
against it, found a mismatch, and wiped again. One missing header became a
permanent loop.

Two rules now:

- Clear only when **both** sides have a value and they differ. Silence means
  carry on.
- Persist only what the server actually said. Never write a placeholder that a
  later comparison will read as a real value.

The asymmetry is the whole argument. A mailbox that genuinely reissued its uids
is rare, and the cost of missing it is one delayed resync -- the next real
change catches it. A missing header is a transient nothing, and the cost of
treating it as a reissue is the mailbox. When the two guesses cost that
differently, the safe one is not a matter of taste.

Nothing was lost from the server, and the blobs are content-addressed so bodies
and attachments survived on disk; what went were the envelope rows, which
re-download. Attachments vanishing from a message that had them is the same
event seen from the UI: `attachments` cascades from `messages`, so clearing the
folder took the rows with it and `has_attachments` went false until the
envelope came back.
