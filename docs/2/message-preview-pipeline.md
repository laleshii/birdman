---
id: message-preview-pipeline
title: 'Message previews: where the snippet text comes from'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: depends_on
  target: folder-and-uid-sync
- type: references
  target: message-list-virtualization
summary: Why previews ride a truncated BODY.PEEK[TEXT] on the envelope FETCH, why the fragment is reparsed with its headers glued back on, and why existing rows stay blank.
---

# Message previews: where the snippet text comes from

The message list's third row tier (see [[message-list-virtualization]]) is a
two-line body preview. Getting text for it is not obvious, because envelope
sync deliberately never downloads bodies.

## IMAP gives you nothing for free here

`BODY.PEEK[HEADER]` — what envelope sync fetched before this existed — returns
the RFC 822 header block only. There is no body text in it, so no amount of
header parsing produces a preview.

RFC 8970 defines a `PREVIEW` fetch item built for exactly this, and it is the
right answer where it exists. **Gmail does not advertise it.** Its capability
list is:

```
IMAP4rev1 UNSELECT IDLE NAMESPACE QUOTA ID XLIST CHILDREN
X-GM-EXT-1 XYZZY SASL-IR AUTH=XOAUTH2 AUTH=PLAIN ...
```

No `PREVIEW`, no `SNIPPET`. The snippets in Gmail's *web* UI are computed
server-side for Google's own client and are not exposed over IMAP — worth
knowing, because "Gmail obviously has previews" is a reasonable thing to assume
and it does not survive contact with the protocol.

## What runs instead

`sync_folder` fetches a bounded slice of the body on the **same** FETCH command
as the headers:

```
(UID FLAGS BODY.PEEK[HEADER] BODY.PEEK[TEXT]<0.2048>)
```

One extra item, not a second round trip, capped at `PREVIEW_FETCH_BYTES`
(2048) so envelope sync doesn't quietly become a body download. Still
`PEEK`, so it doesn't set `\Seen`.

## The fragment is reparsed with its headers glued back on

`preview_from_fragment` concatenates the header block and the body fragment and
parses the pair as one message. This is the part that is easy to get wrong: a
bare `BODY[TEXT]` fragment is **undecodable in isolation**. For a
`multipart/alternative` message it begins mid-boundary, and its transfer
encoding (`base64`, `quoted-printable`) is declared in headers the fragment
does not contain. Handing `mail-parser` the real headers is what lets it find
the boundary, the Content-Type and the encoding, and pull out the first text
part.

The fragment is cut at a fixed byte count, so it routinely ends mid-word,
mid-tag or mid-part. `mail-parser` is liberal about that; anything that still
fails yields `None` and the row simply has no preview.

## Two decoding traps

Both of these produce output that *looks* like text, so they don't announce
themselves as bugs — they just make previews subtly wrong.

**1. A part cut mid-way isn't decoded.** `mail-parser` won't apply a part's
`Content-Transfer-Encoding` until it has seen that part end, and the fetch cuts
at a fixed byte count, which usually lands inside a part. The preview then
comes back carrying its raw encoding: `Hello=0Athere` for quoted-printable, or
a wall of base64. `preview_from_fragment` fixes this by finding the
`boundary=` parameter in the headers and appending a synthetic `--BOUND--`
closing delimiter. Same fragment, measured:

| | `text_body` |
|---|---|
| raw | `Hello=0Athere this is t` |
| with closing delimiter | `Hello\nthere this is t` |

Non-multipart messages need no repair — a truncated single part decodes fine
on its own, base64 included.

**2. `text_body` is not necessarily plaintext.** `mail-parser`'s
`text_bodies()` falls back to listing the `text/html` part when a message has
no `text/plain` one, so `ParsedMessage::text_body` can be raw markup. Reading
it and assuming plaintext is how `&gt; Begin forwarded message:` ends up in a
preview. `ParsedMessage::text_plain_body` filters on the part's actual media
type (treating a part with no `Content-Type` as `text/plain`, per RFC 2045) and
is what the preview reads.

## Snippet cleanup

`birdman_mime::preview_snippet` reads `text_plain_body` and nothing else, then
collapses whitespace runs.

An HTML fallback was built and then removed. Stripping tags out of the HTML
part sounds reasonable and isn't: the preview is the first 2 KB of a body, and
that slice of an HTML part is usually mid-`<style>` or mid-comment, with no
opening tag in view to tell a stripper what it's inside. Even skipping
`<style>`/`<script>` contents wholesale, the output was CSS fragments and stray
markup often enough that **no preview reads better than a wrong one**. A
message with no `text/plain` part simply gets none — which is also why the
message list must handle a missing preview gracefully rather than reserve space
for it.

## Existing rows stay blank

The preview fetch only runs for UIDs the sync considers *new*. `Store::migrate`
adds the column as NULL and does not backfill, so every message synced before
this existed keeps an empty preview until its folder resyncs from scratch. On a
mailbox of any size that is nearly all of them. Deriving previews locally from
`message_bodies` is not a workaround either — bodies are fetched lazily on open,
so only a handful of messages have one. A real backfill means a batched
`BODY.PEEK[TEXT]` pass over historical UIDs, which does not exist yet.

## Attachment pills

Rendered in the reading pane's **header**, below the sender. Not at the top of
the message where they belong visually: the content area is covered by a native
webview that composites above everything gpui draws
([[reading-pane-webview]]), so a pill placed there is invisible the moment an
HTML body loads.

Each pill is draggable out of the window via `external_drag_payload`, which
promotes the drag to a native file drag on the way out, so an attachment can be
dropped into Finder or a messenger. Nothing follows the cursor until then.

That is a limit rather than a choice, and a custom preview was tried and
removed. gpui promotes to a native drag only once the pointer leaves the
viewport -- `promote_external_drag_to_platform` returns early while inside, and
is private -- so an in-window preview is the only thing on offer. It cannot work
here: the pane's body is a native webview composited above everything gpui
draws ([[reading-pane-webview]]), so the preview showed for the inch of header
above the pills and then vanished for the rest of the drag. Two previews, one
of which disappears mid-drag, is worse than one that starts at the window edge.

Hiding the webview for the duration, the way the overlays do, would fix the
visibility and blank the message while you drag from it. Not obviously better. Clicking opens it with the system's default
handler through the `open` crate -- the portable choice, where Quick Look would
have been macOS-only and a built-in viewer would only ever cover the formats we
got round to.

Materialising happens on open rather than on drag, because a drag has to hand
the platform a path that already exists and by then there is no moment to make
one.

## Listing and materialising are two requests

`Store::attachments` reads names and sizes; `materialise_attachments` writes the
copies. They were one call, and that was wrong twice over.

**For the reader:** the header stayed empty for the whole of it, then filled in
at once. Names and sizes come from the envelope and are known immediately, so
the pills can list themselves straight away with a pulse where the size will
be -- a named file that is not ready yet says far more than "Preparing
attachments...".

**For everything else:** the client serialises queries on one connection, so
copying files inside a query put every other read behind it -- including the
**body of the very message** those attachments belong to. Listing is a cheap
read on the shared connection; materialising goes off-thread on its own, the
way commands do. Neither waits on the other now.

A pill without a path is not draggable or clickable. That is not a restriction
so much as the honest consequence of there being no file yet, and it is what
keeps the drag guarantee simple: if you can drag it, it exists.

## Attachment rows used to accumulate

`store_message_body` inserted them without clearing first, and a body is
re-fetched often enough -- a repair, a resync, a cleared cache -- that one
message in a real mailbox held the same PDF **eight times**. Storing a body is
idempotent; inserting is not.

It deletes the message's rows first now, and `migrate` removes the duplicates a
store already accumulated, keeping the lowest id of each identical set. Only
rows are affected: the blobs are content-addressed, so the re-insert finds the
same file on disk and writes nothing.
