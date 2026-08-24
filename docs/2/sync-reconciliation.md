---
id: sync-reconciliation
title: 'Reconciling with the server: deletions, renames, and CONDSTORE'
altitude: 2
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
- type: references
  target: folder-and-uid-sync
summary: How sync notices what the server removed or renamed, the guards on destructive paths, and the HIGHESTMODSEQ fast path.
---

# Reconciling with the server: deletions, renames, and CONDSTORE

Sync had a whole category of blind spot: it noticed everything the server
*added* and nothing it *removed*. New messages arrived, flags updated,
UIDVALIDITY resets were handled -- but a message deleted in Gmail stayed in
Birdman forever, and a message moved between folders appeared in both.

The cause was structural rather than an oversight. The envelope pass only
fetches uids **above** the highest one stored, and the flag pass walked the
*server's* uids and updated matches. Neither could see a local row the server
no longer had.

## Messages: `reconcile_existing`

One pass now does flags and absences together, because the server's answer to
"what uids exist" is both.

```
UID SEARCH ALL                 -> the surviving uid set
UID FETCH 1:* (FLAGS) ...      -> flag changes
```

Local rows whose uid is absent from the set are deleted.

`UID SEARCH ALL` rather than reading uids out of the flag fetch: it returns
just numbers, where the fetch returns a response per message. On a 40,000
message mailbox that difference is the whole cost of the pass.

### Two guards on the destructive half

Both exist because deleting the local copy of a folder on a bad answer is
unrecoverable-ish (it re-downloads, but slowly and visibly).

- **Deletion only after the commands complete.** `?` bails on any stream error,
  so a partial listing can never be mistaken for "the rest were deleted".
- **A completed-but-empty listing is refused** when the mailbox claims to hold
  messages. That is inconsistent enough not to act on; it logs and skips.

## Folders: prune, but adopt renames first

`prune_vanished_folders` removes stored folders the server no longer lists.
Without it the folder list only ever grows -- an account that has lived through
Gmail's `[Gmail]` / `[Google Mail]` namespace change ends up with two complete
sets of special-use folders, and the sidebar shows two of each default.

Guarded on a **non-empty** `LIST`, for the same reason as above.

`adopt_renamed_folders` runs first, because a rename is indistinguishable from
"one folder vanished, another appeared" -- and treating it that way throws away
the cache and re-downloads the folder.

`UIDVALIDITY` is what separates them: a rename preserves the uid space, a new
folder gets its own. Each newly-listed path is `STATUS`-ed for its uidvalidity
(which, unlike `SELECT`, does not disturb the session's current mailbox) and
matched against the vanished folders'.

Only **unambiguous** matches are followed -- exactly one vanished and one new
folder sharing a uidvalidity. Merging two folders by accident is far worse than
a slow re-download. And it returns before issuing any command when nothing
vanished or nothing is new, which is every ordinary sync.

## CONDSTORE: the fast path

`SELECT (CONDSTORE)` reports `HIGHESTMODSEQ`, stored per folder.

It covers *every* metadata change -- new mail, flag edits, expunges -- so an
unchanged value proves there is nothing to reconcile and the entire pass is
skipped. On Gmail's All Mail that is one command instead of a scan over 40,000
messages.

When it has changed, flags are fetched with `(CHANGEDSINCE n)` so only the
changed ones come back. Without CONDSTORE it falls back to the full fetch,
because there is no way to ask which changed.

**A modseq is only meaningful within its uid space.** It is cleared alongside
the messages on a UIDVALIDITY reissue -- carrying it across would make the next
sync skip reconciling a mailbox that had just lost everything.

The parser risk was checked before building this, given that a malformed
`BODYSTRUCTURE` once killed folder sync outright ([[folder-and-uid-sync]]):
`imap-proto` has `rfc4551.rs` producing `AttributeValue::ModSeq`, so MODSEQ in
a FETCH response parses. Servers without CONDSTORE ignore the SELECT parameter
and return `None`, which every branch already reads as "reconcile everything".

## Still missing

**QRESYNC.** Without it, spotting deletions requires enumerating the server's
uids every time. `UID SEARCH ALL` makes that cheap rather than free; QRESYNC's
`VANISHED` would make it free.
