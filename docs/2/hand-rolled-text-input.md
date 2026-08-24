---
id: hand-rolled-text-input
title: 'Text input: shared primitives because GPUI ships no widget'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
summary: text_input.rs holds byte-offset cursor math and common key handling shared by compose, the password prompt, and the search box; what's intentionally left per-field, and the missing selection support.
---

# Text input: shared primitives because GPUI ships no widget

GPUI has no reusable text input widget. Its own `examples/input.rs` is a
~800-line custom cursor/selection/IME element you're expected to adapt. Birdman
didn't adapt it; it built a small shared primitive instead.

## Why `text_input.rs` exists

Before it, every hand-rolled field reimplemented editing separately and drifted:
the search box ended up with no cursor and no paste, while `compose.rs` had
cursor math but also no paste. `crates/birdman-ui/src/text_input.rs` is the
extraction of what they all need.

Three call sites share it: `compose.rs`'s To/Cc/Subject/Body fields,
`root.rs`'s search box, and the pickers built on `PickerState` -- see
[[picker-component]].

## What's shared

- `prev_char_boundary` / `next_char_boundary` — cursor movement that respects
  UTF-8 boundaries.
- `try_common_edit_key` — character insertion, Left/Right, Backspace/Delete,
  Space, Home/End, and clipboard paste.

**Cursors are byte offsets into the string, not character indices.** That's why
the boundary helpers exist and why you must never do arithmetic on a cursor
directly.

## What's deliberately not shared

Because it genuinely differs per field:

- multi-line Home/End/Up/Down (only `compose.rs`'s body field)
- what Enter does — submit a form, advance to the next field, or insert a newline
- rendering — masked for the password field, plain elsewhere

The convention: **callers check their own special-case keys first, then fall
through to `try_common_edit_key`** for everything else. Follow that shape when
adding a field.

## No text selection

There is no selection support anywhere — no shift+arrow, no click-drag, no
select-all. It's a known follow-up, not an oversight: `compose.rs`'s module doc
states building it properly was more than that pass's scope justified. Editing a
reply in place works; selecting text doesn't.

## Cursor rendering

`compose.rs` has a helper that renders content line by line and, when a field is
active, splits the cursor's line into before/caret/after so there's a real
visible insertion point. It works unmodified on single-line content — the search
box reuses it.

`ComposeView::cursor` is a byte offset into whichever field currently has focus,
and it's **reset to that field's end** whenever focus moves (Tab, Enter, or a
click on another row). Fields don't remember a cursor position from before they
lost focus.

`current_field_and_cursor` returns `(&mut String, &mut usize)` — a disjoint-field
accessor that exists because a single `&mut self` method returning `&mut String`
would borrow all of `self`, `self.cursor` included.

## Read-only text copies; it does not pretend to select

The subject line was briefly a focusable read-only field with keyboard
selection, and it was worse than not having it. Real selection needs glyph
hit-testing gpui does not expose -- `InteractiveText` offers clicks, hover and
tooltips but no selection, because Zed builds selection on its editor rather
than on the element. What can be built without it looks like a text field,
cannot be dragged over, and behaves like nothing else on the machine.

Clicking the subject copies the whole thing, which is what the selecting was
for. The same applies to the sender's address.

The editable fields keep their keyboard selection, because there the caret is
already real and visible -- the reader put it somewhere by typing.

## Focus follows intent, not clicks

Clicking a filter while typing a search keeps the caret in the search box
(`keep_search_focus`). Narrowing a search is a refinement of the thing you are
in the middle of typing, not a departure from it, and losing the caret means
going back and clicking again before the next keystroke lands.
