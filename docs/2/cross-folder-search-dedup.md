---
id: cross-folder-search-dedup
title: Full-text search and cross-folder deduplication
altitude: 2
topics:
- storage/search
relations:
- type: part_of
  target: local-message-store
summary: Why search must dedupe by Message-ID across Gmail label-folders, the single-statement window-function query, ordering by recency rather than BM25, and the three FTS5 write sites.
---

# Full-text search and cross-folder deduplication

`Store::search(query, limit)` in `crates/birdman-store/src/lib.rs`, backed by the
FTS5 virtual table `messages_fts(subject, from_addr, snippet)`.

## Why dedup is mandatory, not a nicety

IMAP has no cross-folder message identity. `(folder_id, uid)` is unique only
*within* a folder — that's what `messages`' `UNIQUE(folder_id, uid)` encodes.
And Gmail models labels as folders, so one physical email routinely has separate
rows in `INBOX`, `[Google Mail]/All Mail`, `[Google Mail]/Important`, and every
label it carries.

Without deduplication, searching would show the same email once per folder it
happens to be filed under. The only usable identity is the RFC 5322 `Message-ID`
header, stored as `messages.message_id_header`.

## How the query does it

One statement, no post-processing in Rust. A `ranked` CTE joins `messages_fts` to
`messages` and assigns:

```sql
ROW_NUMBER() OVER (
    PARTITION BY COALESCE(m.message_id_header, 'row-' || m.id)
    ORDER BY f.rank
) AS dedup_rank
```

then the outer query keeps `dedup_rank = 1` and orders by **`date DESC, id
DESC`** — recency, not relevance. BM25 `rank` now only decides *which duplicate
survives* the dedup, not the order results appear in. For mail, "most recent
first" is what a reader expects; relevance ranking put a 2019 thread above this
morning's message. The
`COALESCE(..., 'row-' || m.id)` is the malformed-mail case: a message with **no**
`Message-ID` partitions by its own row id, so it stays a distinct result instead
of collapsing together with every other ID-less message. Two unit tests pin both
halves: `search_deduplicates_the_same_message_across_folders` and
`search_keeps_separate_messages_with_no_message_id_header`.

Where duplicates exist, the copy kept is the highest-ranking one, not a
particular folder's.

## The indexed columns, and migrating them

`fts5(subject, from_addr, from_name, snippet)`.

`from_name` was added later, and doing so is not a normal migration: **FTS5 has
no `ALTER TABLE ... ADD COLUMN`.** The table has to be dropped and rebuilt from
`messages`, which the migration does. Budget for that if another column is ever
needed.

Only `snippet` carries body text, and it is only written when a body is
downloaded — so full-text coverage of message *bodies* tracks how much of the
mailbox has been backfilled, not how much has been synced. Envelope fields are
always searchable.

## The rowid contract

`messages_fts.rowid` **is** `messages.id`. Every write to the FTS table passes
`message_id.0` as `rowid` explicitly, and `search` joins on `m.id = f.rowid`.
Nothing enforces this at the schema level — FTS5 virtual tables have no foreign
keys — so it has to be maintained by hand at every write site.

## Three write sites, and an FTS5 quirk

1. `upsert_message_envelope` writes `subject`, `from_addr` and `from_name`.
   FTS5 virtual
   tables **don't support `ON CONFLICT ... DO UPDATE`** (UPSERT), so this uses
   `INSERT OR REPLACE`. The subtlety, called out in a comment: its `VALUES` are
   evaluated *before* the delete, so
   `COALESCE((SELECT snippet FROM messages_fts WHERE rowid = ?1), '')` still sees
   the old row and an existing snippet survives re-syncing the envelope.
2. `store_message_body` sets `snippet` to the first 200 chars of the plaintext
   body, in the same transaction that writes the body and sets `body_fetched`.
3. `delete_message` issues `DELETE FROM messages_fts WHERE rowid = ?1` —
   `ON DELETE CASCADE` doesn't reach a virtual table.

Bodies are never indexed in full; only subject, from address, and that 200-char
snippet are searchable. Adding a searchable field means a column in the virtual
table plus a write at all three sites.

## Scope in the UI

Search spans **every folder of every account** — it is not scoped to the selected
folder. `AppState::search_results` being `Some` is what makes the message list
show results instead of the selected folder's messages (the header reads
"Search Results"); `clear_search` sets it back to `None`.

## Any copy can serve the body

Dedup picks one row per `Message-ID`, but which one is arbitrary -- and only one
copy of a Gmail message usually has its body fetched. Search would hand you a
copy with nothing cached, the pane showed "(no plaintext body)", and the
on-demand fetch then failed on that copy's stale UID. Meanwhile the All Mail
copy of the same message had the body all along.

`Store::get_message_body` now falls back to a sibling row matched on
`Message-ID` when its own row has nothing. The extra query runs only on a miss,
and a row without a `Message-ID` is never matched to anything.

`copy_body_to_siblings` already shared bodies at fetch time, but only with the
copies that existed *then* -- a label applied later, or a row synced after the
fetch, still had nothing. Reading through to a sibling covers those without
needing to guess when to re-run the copy.

`AppState::select_message` correspondingly asks the store before trusting its
own row's `body_fetched` flag, or the fallback would never be reached.

## What reaches `MATCH` is built, never typed

`fts_query` turns the reader's text into an FTS5 query. Raw text could not do
two things.

**Prefixes.** FTS5 matches whole tokens, so "postho" found nothing while
the full word matched -- which reads as the search being broken, because every other
search box matches as you type. Each token gets a `*`.

**Survive punctuation.** `MATCH` takes a query *language*: a quote, a colon, a
leading `-`, the bare word `OR` all mean something in it, and the ones that do
not parse come back as an **error** rather than as no results. Typing `it's`
should not be a syntax error.

Splitting on non-alphanumerics and re-quoting each token solves both, and means
nothing typed is ever read as syntax. Because the tokens are alphanumeric by
construction there is no quote left inside one to escape. Tokens are
space-joined, which FTS5 reads as AND.

Nothing searchable at all returns an empty result rather than an error.
