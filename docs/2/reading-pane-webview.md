---
id: reading-pane-webview
title: The reading pane webview, and why the image renderer is still there
altitude: 2
topics:
- rendering
- ui
relations:
- type: part_of
  target: gpui-application
- type: references
  target: email-dark-mode-adaptation
summary: wry attaches a platform webview as a child of the gpui window; what that buys, the z-order and Wayland constraints that come with it, and why the Blitz path stays as fallback.
---

# The reading pane webview, and why the image renderer is still there

`crates/birdman-ui/src/webview.rs`. HTML bodies render in a real platform webview
(WKWebView on macOS, WebKitGTK on Linux) attached as a **child view** of the
gpui window — not a window of its own.

## Why this wasn't the design from the start

gpui ships no webview element. Its entire element set is
`div`/`img`/`text`/`svg`/`canvas`/`list`/`surface`, and there is no
`wry`/`WKWebView` anywhere in `gpui` or `gpui_apple`. That is why this app originally
rasterized each email with Blitz in a helper process and composited the result
as an image — with no way to show HTML, that was the only option, and it has by
construction no links, no text selection and no hover. That path is gone.

What makes the webview possible is narrower than it sounds: `wry`'s
`build_as_child` takes any `HasWindowHandle`, gpui's `Window` implements it,
and both resolve to **raw-window-handle 0.6.2** — one version across the whole
graph. No shim, no fork. Check that version agreement first if attachment ever
stops compiling; two 0.6.x copies would make `HasWindowHandle` a different
trait to each side.

`ammonia` is a direct dependency of `birdman-ui` and sanitizes on this side.
Removing the Blitz crate also removed the workspace's build split: `stylo` is
no longer anywhere in the graph, so `cargo build` and `cargo test --workspace`
both work as one command again.

## Two constraints that are the model, not bugs

**Z-order is all-or-nothing.** gpui paints its whole window through one Metal
layer; the webview is a sibling native view composited over that layer as a
unit. Nothing gpui draws can appear on top of it, and gpui's clipping and
corner radii don't apply. Any dropdown, context menu or in-window modal placed
over the reading pane will render *behind* it. There is no fix short of not
overlapping the pane — design around it.

**Bounds are pushed by hand.** gpui's layout knows nothing about the native
view, so `AppState::reading_pane_rect` is measured every frame by a `canvas()`
in `root::reading_pane` and replayed into `set_bounds` from `Root::render`, one
frame behind.

Two non-obvious things about that probe:

- It sits on a **non-scrolling wrapper** on purpose. A probe inside
  `reading-pane-content` would scroll away with the content and drag the
  webview off-screen.
- It calls **`window.request_animation_frame()`** when the rect changes, *not*
  `window.refresh()`. Storing the new value is not enough: the probe runs during
  prepaint, after `Root::render` has already read the old rect and positioned
  the webview from it, and writing a `Cell` schedules no further frame. A layout
  change that causes no other redraw — collapsing the sidebar is exactly that —
  otherwise leaves the webview parked at its previous bounds indefinitely.
  `refresh()` does not work here and the reason is a trap worth knowing:
  see [[gpui-redraw-traps]]. Either call is guarded on an actual change, or it
  is an unconditional redraw loop.

## There is no fallback

`EmailWebView::new` returning `None` means HTML bodies show only their
plaintext part. The Blitz image renderer that used to cover that case has been
removed at the maintainer's direction.

This matters most on **Wayland**, which is untested: `build_as_child` is
documented as not working there, and that path needs
`WebViewBuilderExtUnix::build_gtk` with a `gtk::Fixed`, while X11 additionally
wants GTK initialized with its loop advanced — against a gpui backend doing its
own X11/Wayland windowing. macOS attaches cleanly. If Linux turns out to need
the `build_gtk` path, that is now the fix to write; there is nothing to fall
back to.

## Security

The image renderer executes nothing at all, so a webview is strictly more
attack surface. It's configured to give back what it can: JavaScript disabled
outright, HTML still sanitized by ammonia first, and **every top-level
navigation refused**. An `http`/`https` click is handed to
`open::that_detached` and opens in the user's real browser, which is what a
mail client should do anyway. Subresource loads (an email's remote images)
don't pass through the navigation handler, so this doesn't block them — the
remote-content tradeoff is unchanged, see [[remote-and-inline-images]].

## Typography and the injected stylesheet

`document_style(force_dark)` builds the stylesheet prepended to every body. A
webview's own default is Times New Roman at 16px, which no mail client shows, so
it sets a sans stack at 15px/1.5. Those rules sit on `html`/`body` and are
emitted *before* the message's markup, so any sender rule wins — a floor, not an
override.

It also contributes two things that are not typography:

- **Centering.** `body > * { margin-inline: auto }` centres the fixed-width
  600px table layout most newsletters use, instead of pinning it left with dead
  space beside it. Safe unconditionally: `margin-inline: auto` only affects a
  block with a definite width.
- **A painted scrollbar gutter.** Track *and corner* are given the page colour
  explicitly. WebKit's scrollbar gutter sits outside the document's background
  box, so `transparent` there resolves to nothing behind it rather than to the
  page — the corner where two scrollbars meet came out as a hard black square.

Whether the document is forced dark, and what that costs, is its own topic:
see [[email-dark-mode-adaptation]].

## Cursor: gpui and WebKit fight over it

`crates/birdman-ui/src/cursor.rs`. Symptom: `cursor: pointer` over a link in an
email never shows, no matter what the injected stylesheet says.

The cause is one line in `gpui_macos`'s `reset_cursor_rects`:

```rust
let bounds = NSView::bounds(this ...);
let _: () = msg_send![this, addCursorRect: bounds cursor: cursor];
```

A single cursor rect over gpui's **entire view**, which AppKit re-asserts on
every mouse move. The webview sits inside that rect, and WebKit doesn't use
cursor rects at all — it calls `[NSCursor set]` directly. AppKit's rect wins,
every time. Upstream: wry#1763.

`disableCursorRects` turns AppKit's handling off for the window and lets
WebKit's `set` stick. It can't be left off permanently: cursor rects are the
*only* mechanism gpui uses (it never calls `[cursor set]` itself), so the
I-beam in the search field and the pointer on buttons would stop working. It's
therefore scoped — off while the pointer is inside `reading_pane_rect`, back on
when it leaves, driven from the root's `on_mouse_move`.

That scoping relies on a fact worth recording, because it is not obvious and it
also underpins the root-level scrollbar drag handling: **gpui keeps receiving
`mouseMoved` over the child webview.** Verified by logging positions while
warping the cursor across the window — positions well inside the reading pane
(local x=835, pane starting at 520) arrive normally. What gpui does *not* do is
dispatch them to elements whose bounds don't contain the point, which is why
per-pane handlers stop firing and root-level ones don't.

Toggling is edge-triggered. `disableCursorRects` is counted by AppKit, so
calling it per-move would need matching enables to unwind.

## `with_focused(false)`, or the window goes deaf

wry defaults `focused` to `true`. For a reading pane that default is actively
wrong, and the symptom does not look like a focus problem at all.

The webview is a native child view. Once it takes first responder, AppKit routes
every keystroke to it, gpui receives none, and macOS beeps at each one because
nothing consumes them. Arrows stop moving the selection, the palette will not
open, and the window reads as frozen -- "it hangs for a while until the app
becomes functional again", where the recovery is clicking back on the list.

Nothing in the pane wants the keyboard: JavaScript is disabled and the document
is not editable. So it does not get it.

Still open: clicking *into* the pane, to scroll or select text, hands it first
responder the ordinary AppKit way. Keys are dead again until the reader clicks
back. Taking focus back from a native subview needs platform code that does not
exist here yet.

## Ammonia's defaults delete HTML email's layout

`ammonia::Builder::default()` allows `align, char, charoff, summary` on
`<table>` and no `width` at all on `<td>`. HTML email is built almost entirely
out of the attributes it drops: `width`, `height`, `cellpadding`, `cellspacing`,
`border`, `valign`, `bgcolor`.

A `width="600"` newsletter -- the near-universal convention -- therefore arrived
with every width gone and rendered fluid: stretched across the pane with its
cells collapsed against each other. Comparing the same message side by side with
Apple Mail, which keeps them, is what made it obvious.

They are re-allowed per tag. All are layout hints: no scripting, no navigation,
no network. `bgcolor` is a *presentational hint*, which sits below author CSS in
the cascade, so unlike an inline `style` it cannot fight the forced-dark
stylesheet. The `on*` handlers ammonia strips stay stripped, and a test pins
that adding these did not widen anything else.
