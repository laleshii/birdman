---
id: attachment-pipeline
title: 'Attachments: blobs, materialised copies, and getting a file out'
altitude: 2
topics:
- storage
- ui
relations:
- type: part_of
  target: local-message-store
- type: references
  target: security-boundaries
- type: references
  target: message-preview-pipeline
summary: Content-addressed blobs versus named materialised copies, the two-stage list-then-copy query split, why drag-out is gone, quarantine, and the sweep's two different expiry rules.
---

# Attachments: blobs, materialised copies, and getting a file out

Attachment *contents* never live in SQLite. They are files on disk under
`<data_dir>/attachments/`, in two trees that mean different things.

## Two trees, two lifetimes

**Blobs** are content-addressed — `attachments/a3/a3f9…` — sharded two levels
deep. That is right for storage: the same PDF mailed to you twice is one file,
and `insert_attachment` writes nothing when it finds the path already there.

**Materialised copies** are a second copy of a blob under a name a human would
recognise, one directory per message. They exist only because a
content-addressed name is useless the moment a file leaves the app: opened in
another program, or saved out to a directory, it has to arrive called
`invoice.pdf`, not `a3f9…`.

They are **copied, never hard linked**. An editor writing back through a link
would corrupt the content-addressed original that every other copy of that
message shares.

## The sweep's two rules

`Store::sweep_attachment_cache` applies a different rule to each tree, and the
asymmetry is the point.

- Materialised copies expire **on use**: a directory untouched for
  `MATERIALISED_TTL` (7 days) goes, and reopening the message brings it back at
  the cost of one file copy. Nothing is lost by being wrong.
- Blobs are the *only* copy, so age says nothing — a three-year-old message
  still has to open. They go only when **no attachment row references them**,
  which means the message was deleted. On a real mailbox that was 294 files and
  58MB left behind, because deleting a message cascades its rows and leaves the
  files.

Safe to be wrong in exactly one direction, and it is: a blob deleted while still
wanted is written back by `insert_attachment` on the next body fetch.

`touch()` sets **`mtime`, not `atime`** — `relatime` mounts refuse to update an
access time that is already recent, which is precisely the update this needs.

## Listing and materialising are separate queries

`Query::Attachments` reads metadata; `Query::MaterialiseAttachments` writes the
copies. They were one call, and the split is a latency fix rather than tidiness.

Names and sizes come from the envelope and are one cheap read. The copies are a
file each. Because the client serialises queries on a single connection, doing
both in one request meant the *body* read of the very message these belong to
queued behind the file copying — the header stayed empty for the whole of it.

So `AppState::load_attachments` runs in two stages: list first and render the
pills immediately with a pulse where the size will be, then materialise on a
connection of its own. `Attachment::path` stays `None` until that copy exists,
so nothing offers a file that is not yet on disk.

## Getting a file out

Clicking a pill hands it to the OS default handler. From a terminal,
`birdman attachments <id> --save DIR` writes every attachment out under its real
name, which is the portable route and the one a script can use.

**There is no drag-out, and this is a dependency constraint rather than a
choice.** It needs gpui's `external_drag_payload` / `ExternalDragPayload::Files`,
which exist only on Zed's `main` and not in the published gpui the desktop crate
depends on. Taking them back would mean a git dependency, which makes the crate
unpublishable to crates.io and drags the GPL-3.0 `zlog`/`ztracing` in with it —
see the note on the dependency in `crates/birdman-ui/Cargo.toml`.

Worth knowing if it is ever restored: gpui promotes a drag to a native one only
once the pointer **leaves the viewport**, so an in-window preview is the only
thing on offer, and one cannot work here — the reading pane's body is a native
webview composited above everything gpui draws (see [[reading-pane-webview]]),
so a preview survives the inch of header above the pills and then vanishes.

## Why the pills are in the header

`root::attachment_pill` draws below the sender and above the body — in the app's
chrome, not at the top of the message. The pane's content area is covered by the
webview, so a pill placed there would be invisible the moment an HTML body
loaded.

## Two hazards worth knowing

`safe_attachment_name` sanitises the sender's filename before it reaches the
filesystem. This is the only place a sender-chosen name ever does. Not
theoretical: document scanners produce names shaped like
`9876543210 / 20240117 121339.PDF` — a reference number, a separator with
spaces around it, a timestamp — and `dir.join()` on that silently writes into a
subdirectory. See [[security-boundaries]].

Clicking a pill hands the file to the OS default handler, which means running
whatever is associated with its extension. That is why a materialised copy
carries `com.apple.quarantine` (`0081`, matching what Safari and Mail write), so
Gatekeeper intercepts an executable that arrived by mail rather than launching
it.

## has_attachments is derived late

The paperclip comes from the real body, in `Store::store_message_body`, not from
the envelope. `BODYSTRUCTURE` is the obvious IMAP shortcut and cannot be used —
see the note in `birdman_imap::sync` about the malformed response that aborts a
whole folder's FETCH. A message whose body has never been fetched therefore
reports `false` however many files it carries, and opening it re-runs the
attachment load once the body lands.
