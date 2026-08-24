---
id: sidebar-pane-layout
title: 'Sidebar layout: why the header and footer are pinned'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: references
  target: message-list-virtualization
summary: The sidebar's three-part flex column, why only the folder list may grow, and the squashed-buttons bug that comes back the moment a sibling loses flex_shrink_0.
---

# Sidebar layout: why the header and footer are pinned

`sidebar` in `crates/birdman-ui/src/root.rs` is a fixed-height flex column with
exactly three parts:

1. **Header** — sync + hide-sidebar buttons, then the account address.
2. **Folder list** — the only part allowed to grow: `flex_1` + `min_h(0)` +
   `overflow_y_scroll`.
3. **Footer** — sync status text and the settings (gear) button.

The header and footer are both `flex_shrink_0`.

## The bug that keeps coming back

That pinning is load-bearing, not cosmetic. The folder rows were originally
direct `.children(...)` of the sidebar column with no scroll container, and the
footer relied on `mt_auto` to sit at the bottom. With few folders that looks
right. With enough folders to overflow the column, flexbox does what it is
supposed to do and **shrinks every sibling that can shrink** — so the button
row and the status/settings footer collapse toward zero height and disappear,
rather than the list scrolling.

The symptom reads as "the sync and settings buttons vanished", which sounds
like a rendering or state bug and is actually just flex sizing. Two things
prevent it:

- `flex_shrink_0` on the header and footer, so they keep their intrinsic height.
- `min_h(px(0.0))` on the scroll wrapper. A flex item defaults to
  `min-height: auto`, which refuses to shrink below its content — without it the
  scroll container grows to fit every folder and never scrolls, which puts the
  footer back off-screen even with `flex_shrink_0` set.

Both are needed. Either one alone still misbehaves.

## Every scroll container in the app needs `min_h(0)`

This has now been the cause of three separate "scrolling is broken" reports, in
three different panes, so it's worth stating as a rule rather than a local
detail: **a `flex_1` box that scrolls also needs `min_h(px(0.))`.** Flexbox
gives a flex item `min-height: auto`, which refuses to shrink below its
content, so the box grows to fit everything inside it and its
`overflow_y_scroll` never has anything to scroll. There's no error and nothing
looks wrong until you try to scroll.

The horizontal twin is real too: `min_w(px(0.))` on `reading-pane` is what lets
a long subject wrap, because otherwise the pane widens to fit the subject on
one line and `w_full` on the subject resolves against a width the subject
itself set.

Current scroll containers, all of which carry it:

- the sidebar's folder list wrapper (`sidebar`)
- the message list's row wrapper (`message_list`)
- `reading-pane-content` (`reading_pane`)

## The folder list's scrollbar is the cheap kind

GPUI ships no scrollbar (see [[message-list-virtualization]]), so this one is
hand-built too — but it is much simpler than the message list's, because the
folder list is a **plain scrollable `Div`** rather than a `uniform_list`. That
means `ScrollHandle::max_offset()` and `bounds()` are populated normally, so the
viewport/content ratio is read straight off the handle with no measuring
`canvas()` involved.

`sidebar_scroll_handle` and `sidebar_scrollbar_dragging` live on `AppState` for
the same across-frames reason the message list's equivalents do: render
functions are rebuilt every frame.

## The scrollbar element is shared

`scrollbar(id, thumb_top, thumb_height, on_down)` in `root.rs` is used by both
the message list and the folder list. It takes a caller-supplied `id` because
two elements sharing an id in one frame collide. The mouse-handler split is the
same in both callers, and for the same reason: move/up on a wide wrapper so a
drag drifting off the 10px track doesn't stall, mouse-down scoped to the track
so a drag can't start from a list row.
