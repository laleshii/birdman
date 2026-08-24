---
id: sidebar-folder-ordering
title: Sidebar folder ordering, naming, and grouping
altitude: 3
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: depends_on
  target: folder-and-uid-sync
summary: sidebar_folder_rank orders and classifies folders, sidebar_folder_name relabels them canonically, and everything past the five defaults collapses into More folders — all off RFC 6154 SPECIAL-USE.
---

# Sidebar folder ordering and naming, Apple Mail style

`sidebar_folder_rank` in `crates/birdman-ui/src/state.rs` sorts the folder sidebar:
Inbox (0), Flagged (1), Drafts (2), Sent (3), Trash (4), everything else
(`OTHER_FOLDER_RANK`, 5).

## Why it's driven by SPECIAL-USE

The ranks come from `folder.special_use`, the RFC 6154 attribute captured during
folder listing (see [[folder-and-uid-sync]]). The alternative — matching
`imap_path` string patterns — only ever works for one provider's naming, e.g.
Gmail's `[Google Mail]/Sent Mail`. SPECIAL-USE works across providers that
advertise it, and Gmail does so on a plain `LIST`.

## INBOX is the exception

`INBOX` is matched by `imap_path.eq_ignore_ascii_case("INBOX")`, not by an
attribute. RFC 6154 defines no `\Inbox` attribute, because servers don't need to
tag the one folder every account already has, and RFC 3501 makes the name
reserved and case-insensitive. This same check appears in
`supervisor::run_account_once` when picking the folder to IDLE on.

## Stability matters here

The last bucket keeps `list_folders`'s existing alphabetical-by-`imap_path`
order, which works only because `sort_by_key` is **stable** and this is applied
solely to that query's output. Switching to `sort_unstable_by_key` would scramble
the tail of the sidebar.

## Rank doubles as the grouping test

`is_default_folder` is just `rank < OTHER_FOLDER_RANK`, deliberately rather than
a second list of special-uses. Ordering and grouping then cannot drift apart —
adding a folder type to one automatically adds it to the other.

Only those five render unconditionally. Everything else goes under a collapsed
**More folders** disclosure in `root::sidebar`, muted and showing a count. That
includes Junk, Archive and All Mail, which *do* carry SPECIAL-USE attributes but
aren't day-to-day destinations. The numbers are the argument: on the Gmail
account this was built against, it's 5 folders shown instead of 31.

`AppState::sidebar_more_expanded` holds the disclosure state and starts
collapsed. The header renders even when there's nothing to hide, so the sidebar
doesn't change shape as folders arrive during the first sync.

## The same attribute drives the label

`sidebar_folder_name` (same file) is the naming counterpart to the ranking, and
reads the same `special_use` field. Special-use folders are relabelled to a
canonical `Inbox` / `Drafts` / `Sent` / `Flagged` / `Junk` / `Trash` / `Archive`
/ `All Mail` rather than shown under whatever the server calls them.

This is not cosmetic tidying. Gmail serves its own folders as
`[Gmail]/Sent Mail`, `[Gmail]/Bin`, and so on, and a localized account serves
localized names throughout — so without this the sidebar reads differently
depending on the account's language and provider, for the exact set of folders
the user is least likely to want surprises in.

Folders with **no** SPECIAL-USE attribute keep `folder.name` untouched: those
are user-created, and the server's name is the only name they have. INBOX is
special-cased by path here for the same RFC 3501 reason it is in the ranking.

## Backfill

For a database created before `folders.special_use` existed, the column is added
empty by `Store::migrate` (see [[schema-migrations-without-a-framework]]).
`AppState::sync_now` re-lists folders partly for this reason, so the ordering
corrects itself without a restart or a reinstall.
