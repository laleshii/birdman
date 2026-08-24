---
id: ui-slot-lists
title: 'Slot lists: config-driven UI composition'
altitude: 2
topics:
- ui
- config
relations:
- type: refines
  target: theming-and-runtime-config
- type: part_of
  target: gpui-application
summary: The ordered-slot-list pattern that lets the config reorder and hide parts of a component, why row height is derived rather than configured, and why hiding a whole region is a separate mechanism.
---

# Slot lists: config-driven UI composition

`crates/birdman-ui/src/config.rs`, consumed in `crates/birdman-ui/src/root.rs`.

Theming answers *what colour*. A slot list answers *what is there, and in what
order*. Both are read from the same config file and both hot-reload through the
same mtime poll (see [[theming-and-runtime-config]]).

## The pattern

A component that is a sequence of small parts is declared as an ordered list of
named slots, not as a hand-built element tree. `ToolbarAction` was the first
one; `MessageSlot` is the second and generalises it.

Three rules make the pattern uniform across components:

- **Omission is hiding.** There is no `show_date = false`. A slot the list
  doesn't name isn't drawn. One mechanism, one place to look.
- **Order in the list is order on screen.**
- **`spacer` is a slot.** It draws nothing and pushes everything after it to the
  trailing edge, which lets a layout express alignment without a separate
  alignment setting. The toolbar also has a `divider`.

Unrecognised names are dropped with a warning and cost you exactly that one
slot. An empty *result* keeps the default instead, because a component with
nothing in it is far more often a typo than a design — with one deliberate
exception, below.

## Where the gutter came from

`MessageRow` is a gutter plus a stack of lines, not a flat list, because the
hand-built row it replaced had the unread dot sitting beside *both* text lines
at once. A flat list could not have expressed that, and a slot list that cannot
express the layout it replaced is not a generalisation of it.

This is also why `gutter = []` is honoured while `lines = []` is not: an empty
gutter is the natural way to say "no unread dot", whereas empty lines leave the
list nothing to draw at all.

## Row height is derived, never configured

`MessageRow::height()` sums the configured lines. Nothing in the app hard-codes
`60.0` any more.

That is not tidiness. `uniform_list` measures the first row and lays every other
row out from that one measurement, and both the scrollbar geometry and the
infinite-scroll trigger multiply the row count by the row height. A configured
height that disagreed with what is drawn would not *look* wrong — the list would
scroll wrong, drifting further out of true the further down you went. Deriving
it means adding a `preview` line costs nothing but the line.

The corollary is that no slot may wrap. Every one is single-line and
ellipsised, with `overflow_hidden` on the row as the backstop.

## Colours are tokens, sizes are numbers

A slot's style override names a palette [[theming-and-runtime-config]] token
(`color = "accent"`), never a hex. A slot that pinned a literal colour would
survive a theme change and then clash with everything around it.

Read/unread stays a property of the slot rather than something the row
special-cases, via `color` and `color_unread`. Setting only `color` sets both —
otherwise the single most obvious one-line customisation would be invisible on
exactly the half of the mailbox people look at.

## Hiding a region is a different mechanism

`[appearance.show]` is booleans for whole regions: sidebar, toolbar, message
list header, scrollbars. This is deliberately *not* the slot mechanism. A slot
list says what a component is made of; `show` says whether the component exists.
Folding them together would mean two places to look when something is missing
from the window.

It also resolves an old ambiguity: `toolbar_actions = []` is rejected as a typo,
so before `show.toolbar` there was no way to ask for no toolbar at all.

## What the catalogue is bounded by

`MessageSlot`'s variants are limited to what `birdman_store::MessageSummary`
already carries. A slot needing a column the list query doesn't select would be
a store change wearing a config change's clothes, and the row would render blank
until someone noticed. `preview` was already in the summary and unused, so
turning it on was a config line rather than a feature.
