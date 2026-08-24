---
id: reading-pane-flicker
title: Why the reading pane draws nothing rather than the plaintext
altitude: 3
topics:
- ui
relations:
- type: part_of
  target: reading-pane-webview
summary: The plaintext is available first and the HTML is not, so painting the fallback while waiting flashed a styleless copy of the message.
---

# Why the reading pane draws nothing rather than the plaintext

Selecting a message used to flash: a styleless copy of the text appeared for a
frame, then the webview replaced it with the real thing.

The cause is an ordering that looks harmless. The plaintext is in the store and
comes back **synchronously**; the HTML has to be sanitized, have its inline
images embedded, and be handed to a native view, so it arrives a frame or more
later. Rendering "whatever we have" therefore renders the wrong thing first and
corrects it.

`selected_html_pending` is claimed the moment `select_message` sees that the
body has an HTML part, and the pane draws **nothing** while it is set. A cached
document resolves synchronously, so there is no gap at all for a message
already read once.

Better to show nothing for an instant than to show the wrong thing and correct
it: a blank pane reads as loading, a flash reads as a bug.

## The same list, twice

Two separate conditions decide whether the plaintext may be painted:

- The webview is hidden while any pane-covering overlay is up
  ([[floating-overlays]]), and the fallback must not be exposed underneath it.
- An HTML body is still being prepared.

The first is a list of overlays that has to be extended whenever a new one is
added -- and it was missed once, when the move picker arrived: the picker
briefly showed the plaintext underneath itself, because the condition still
only named the palette.

Neither omission is a compile error. Both look like a rendering glitch.

## A cached document skips the round trip entirely

`AppState::html_document_cache` holds the last 16 prepared documents --
sanitized, linkified, images embedded. When a selection hits it, `select_message`
sets `selected_html_source` **synchronously** and never touches the store: there
is no body to fetch that will not be painted, and no `selected_body_loading` to
turn on and off again.

This regressed when the store reads moved off the main thread for latency
reasons (see [[reading-pane-latency]]). Making every path asynchronous is the
right default and the wrong answer for the one path that has the answer already
-- it bought a "Loading message..." frame on messages that could have drawn
instantly.

## One waiting state, not two

The pane used to distinguish "fetching the body" from "preparing the HTML":
`Loading message...` for the first, nothing at all for the second. Between them
was a blank frame, and the reader has no use for the distinction anyway. It is
one `awaiting` condition now, so the sequence is `Loading` → document with
nothing in between.

The plaintext still never gets a frame of its own: `selected_html_pending` is
set when the selection starts rather than when the body arrives, so there is no
window in which the text is the only thing available.

## The hide-for-one-frame was unconditional

The webview is hidden for a frame after a selection because "the first frame
still holds the previous message's rect". True when the rect changes -- and it
does not, because switching messages does not move the reading pane.

So every selection paid a blank frame to re-measure a rect that was already
correct. `EmailWebView::is_positioned_at` answers whether the platform view is
already exactly there, asked **before** `set_bounds` (which would make it true
by side effect). If it is, the document is swapped in place and nothing blanks.

The hide still happens when the rect genuinely differs -- first message after
launch, a resize, the sidebar toggling.

## The overlay list is one function now

`AppState::overlay_covers_reading_pane`.

The webview composites above every gpui layer, so anything drawn over the pane
is invisible until it is hidden -- and which overlays count was a condition
maintained by hand in **two** places: the code that hides the webview and the
code that decides what to paint underneath. It drifted on every addition. The
move picker and the log panel each shipped invisible, and each was fixed by
remembering to edit both lines.

The sender dropdown was going to be the third. It is worth recording that the
fix there was to delete the dropdown: hiding the message the reader is looking
at, to show two menu items, is a bad trade for the one click it saves. The
address copies on click and the second action is a button beside it. An overlay
over this pane costs the message underneath, so the question to ask first is
whether it needs to be an overlay.

One function answers it now. A new overlay is one arm there rather than two
edits in two files, and forgetting is no longer a thing that compiles.

## Notifications carve a strip out of the webview

They sit bottom-right, which is over the pane -- so drawing them there would
make them invisible for exactly as long as they were up.

Rather than hide the webview (blanking the message for every "Copied"), the
webview's *bounds* shrink by the height of the stack while anything is showing.
The message loses a couple of rows for two seconds and stays readable.

That is the third answer this constraint has had, and the three are worth
comparing:

- **Palette, move picker, log panel** -- hide the webview. They are modal, take
  the whole window, and the message is not what you are looking at.
- **Sender actions** -- do not use an overlay at all. Two actions did not
  justify one.
- **Notifications** -- shrink the webview. They are non-modal, small, and
  frequent, so both of the above would be wrong.

The question is not "how do I draw over the webview" -- there is no answer to
that -- but "what does this element actually need", and it has come out
differently every time.
