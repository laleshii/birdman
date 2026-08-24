---
id: gpui-redraw-traps
title: 'GPUI traps: refresh() during draw, focus leaks, and nested window.update'
altitude: 3
topics:
- engineering/practices
- ui
relations:
- type: part_of
  target: gpui-ui-conventions
summary: 'Three GPUI behaviours that fail silently rather than erroring: refresh() is a no-op while drawing, clearing state without restoring focus kills all shortcuts, and nested window.update cannot find the window.'
---

# GPUI traps: refresh() during draw, focus leaks, and nested window.update

Three behaviours that produce no error, no warning and no panic — just an app
that misbehaves in a way that looks like something else. Each cost real time in
this repo, and at least one of them recurred three times.

## `window.refresh()` is a no-op while drawing

```rust
// gpui
if self.invalidator.not_drawing() { ... }
```

`refresh()` only schedules a frame when the window is *not* currently drawing.
Call it from inside a `canvas()` prepaint, a paint callback, or anywhere else
within the draw cycle, and it silently does nothing.

**Symptom:** something only updates when you move the mouse. Moving the mouse
generates input, input schedules a frame, and the pending state is finally
picked up. It reads exactly like a hit-testing or hover bug, which is why it was
misdiagnosed here twice before the cause was found.

**Fix:** `window.request_animation_frame()`, which has no such guard.

This bit the reading pane's bounds probe and the webview reveal path. If you
write state during prepaint that the *next* render must see, you need
`request_animation_frame`.

## Clearing UI state without restoring focus kills every shortcut

GPUI delivers key events to the focused node. If the focused element is removed
from the tree and nothing takes focus, **every keyboard shortcut in the app
silently stops working** — no error, and the window still looks fine.

This happened twice with the search box, and the second case is the subtler one:

- The search *toggle button* cleared `search_active` and unmounted the box
  without restoring focus.
- **Escape inside the box** was worse. The box's own handler cleared
  `search_active` before `Root`'s handler ran and read it, so `Root` never saw
  the state it was branching on.

The fix is `AppState::root_focus_handle`, set once when `Root` is constructed,
plus a single `close_search` that every dismissal path goes through. Anything
that unmounts a focusable element must hand focus somewhere explicit.

The observable symptom was "the `?` shortcuts popup stopped working", which
points nowhere near the search box.

## Nested `window.update` cannot find the window

GPUI `take()`s the window out of its registry for the duration of an update. A
`window.update(...)` reached from inside another update fails with **"window not
found"** even though the window plainly exists and the handle is valid.

Hit while implementing Cmd+W: the action fired, the window was located, and the
close failed. `cx.defer(...)` moves the inner update to after the outer one
completes and fixes it.

Related: closing the last window left a headless process behind. GPUI does not
quit on its own — `on_window_closed` has to call `cx.quit()` when no windows
remain.

## The common shape

All three fail *silently*. When something in this UI "doesn't happen", or
happens only after unrelated input, suspect one of these before suspecting your
own logic — and prefer instrumenting over reasoning. Every one of these was
settled by a log line proving what actually ran, after at least one confident
wrong explanation.
