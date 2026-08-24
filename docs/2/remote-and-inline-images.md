---
id: remote-and-inline-images
title: 'Images in HTML mail: the remote-loading tradeoff and cid: embedding'
altitude: 2
topics:
- rendering
relations:
- type: part_of
  target: reading-pane-webview
summary: 'Remote images load by deliberate choice, now fetched by the webview itself rather than a bounded provider; inline cid: attachments render only by being rewritten to data: URIs first.'
---

# Images in HTML mail: the remote-loading tradeoff and cid: embedding

Two completely separate paths get an image into a rendered email body. Confusing
them is easy, so they're described together here.

## Remote images: a knowingly-made privacy tradeoff

The original design blocked all remote content at the network layer as an
anti-tracking-pixel measure. That was reversed, and the reasoning is recorded in
the original module doc: every real marketing/newsletter
email in practice references its images remotely rather than as inline
attachments, so blocking made "images never load" the default experience for
most mail.

The tradeoff is explicit: **a sender can now tell a message was opened**, the
same as in any mail client that auto-loads images (which is most of them). If
you're considering re-adding a block, treat this as a decision to re-litigate,
not an oversight to fix.

There is currently no per-message or per-sender "load images?" toggle.
Remote loading is one global appearance setting: `load_remote_images =
"always"` by default, or `"never"` to block it. Changing the setting clears
prepared-document caches so an old rendering policy cannot survive hot reload.
The blocked mode combines URL rewriting with a document CSP; rewriting alone
cannot safely cover CSS escapes, imports, and every URL-bearing CSS function.

## Fetching is now WebKit's, not ours

This used to go through `HttpNetProvider`, a `NetProvider` implementation that
allowed **only** `http`/`https`, timed out after 6s, and capped responses at
20 MiB with a capped reader rather than trusting `Content-Length`.

That went away with the image renderer. The webview fetches subresources
itself, so those bounds no longer exist and the browser engine's own policies
apply instead. Note this is a real change in posture, not just a change of
implementation: **there is no longer a size cap or a scheme allowlist of our
own on image loads.** Top-level *navigation* is still refused outright (see
[[reading-pane-webview]]), but subresource loads don't pass through that
handler.

## Inline `cid:` images: rewritten before they ever reach the renderer

**Nothing fetches anything for inline attachments.** The bytes
are already on disk from sync (see [[message-schema-and-lazy-bodies]]), and
`crates/birdman-ui/src/webview.rs::embed_inline_images` replaces
`cid:<content-id>` with a `data:<mime>;base64,...` URI *before* the HTML is
handed to the helper process.

Consequences to keep in mind:

- `sanitize` adds `data` to ammonia's allowed URL schemes purely to let these
  survive. Remove that and inline images break.
- A `cid:` URL that reaches the webview unrewritten is simply dropped, same as
  any unrecognized scheme.
- The rewrite is a **plain string replace, not an HTML parse**. It's safe here
  because the needle (`cid:` plus an exact content-id) is specific enough that a
  false positive would require another attribute to coincidentally contain the
  identical content-id string — which sanitization/rendering treats as inert text
  anyway. Both `cid:id` and `cid:<id>` forms are handled.
- The store side is `get_inline_attachments`, which returns only
  `is_inline = 1` rows. Regular attachments are listed by the reading pane, not
  embedded.
