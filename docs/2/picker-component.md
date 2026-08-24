---
id: picker-component
title: 'The picker: one keyboard contract for every filterable list'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-ui-conventions
- type: references
  target: multi-account-ui
summary: PickerKey and PickerState, why a picker is modal, and why this is the widget a command palette needs.
---

# The picker: one keyboard contract for every filterable list

A *picker* is a modal list: navigated with the arrows, narrowed by typing,
committed with Enter, dismissed with Escape. The move-to-folder list is the
first; the account switcher and a command palette are the obvious next ones.

Two pieces in `crates/birdman-ui/src/text_input.rs`:

```rust
pub enum PickerKey { Dismiss, Previous, Next, Confirm, Insert(String), Backspace, Ignored }
pub fn classify_picker_key(event: &KeyDownEvent) -> PickerKey

pub struct PickerState { pub query: String, pub index: usize }
```

Only the *effects* are per-picker. A picker routes one keystroke:

```rust
pub fn move_picker_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>)
```

## Decisions worth keeping

**Modifier chords are never filter text.** Checked before anything else, so
Cmd+A cannot type an `a` into a filter.

**Text comes from `key_char`, not `key`.** On a non-US layout the two differ;
`key` is closer to the physical key. Control keys have no `key_char`, or one
that is not printable, which is the second filter.

**Stepping clamps rather than wraps.** Against a list that shrinks as you type,
wrapping past the end lands somewhere unpredictable.

**Editing the query resets the highlight.** The old index means nothing against
a different list.

**`Ignored` is not passed through.** A picker is modal: letting Delete reach
the message list would act on mail the user is in the middle of filing.

**Matching is case-insensitive across several fields.** Folders match on both
display name and full path, so "Trash" and "[Gmail]/Tr" find the same one --
the sidebar shows normalised names but a user of a nested tree remembers the
path.

## The command palette is this widget, pointed elsewhere

Cmd+K. `crates/birdman-ui/src/palette.rs` is a table of
`{ name, aliases, shortcut, group, run }`, and the overlay is the move
picker's with a different list in it.

**It replaced the shortcuts overlay.** Two modal lists of the same actions was
one too many, and the palette is the better half: it can *run* what it lists,
and it is searchable. `shortcut` is a column here now, and the `?` panel is
gone.

`aliases` makes it searchable by intent rather than by the wording someone
happened to choose -- "trash" finds Delete, "star" finds Flag.

`shortcut` is **not** optional. The palette is the only place a binding is
advertised, so an action reachable only by opening the palette is an action the
keyboard cannot really reach. Two tests hold the line: every advertised
shortcut must exist in `root.rs` or `main.rs`, and no two commands may claim
the same one.

An entry is closed **before** its command runs, because several open an overlay
of their own and two stacked modals is not a state worth having.

### Grouping

`Group` (Compose, View, Mailbox, Window, Respond, File, Remove) mirrors how the
reading-pane toolbar is grouped, so the palette reads as the same actions in a
different shape rather than as a second, unrelated list. `Group::section()`
folds them into Global and Message.

Only the Message half is labelled -- heading the global half "Actions" said
nothing the reader did not already know.

A rule separates groups **within** a section; a section break gets a gap and
the heading instead. Saying it three ways was the first attempt and it turned
fourteen items into six boxes.

**Tab jumps to the next section**, not the next item. Stepping through seven
global actions to reach the message ones is exactly what the split avoids.

A test pins that groups and sections stay contiguous in the table, because the
renderer emits a rule whenever the group changes -- interleaved entries draw
the same divider twice. It caught exactly that on the first attempt.

### Scrolling a picker is not automatic

The palette had `overflow_y_scroll` and still would not follow the highlight:
moving an index does not move a viewport. It needs a `ScrollHandle` and a
`scroll_to_item` on every arrow, on Tab, and on filtering (which resets to the
top).

The prompt line sits **outside** the scrolling box, which keeps it visible and
-- the reason it had to move -- makes the handle's child indices line up with
command indices. With the prompt inside, every scroll was off by one.

Its real value is as a check on the rest of the UI: an action that cannot be
listed here is reachable only by pointing at it. That is what makes "usable
entirely from the keyboard" a property rather than an aspiration -- and it is
why [[service-boundary]] insists operations are *values* with names.

## Rendering

The picker owns the keyboard while open, so there is no separate text field to
focus and no cursor to draw -- the filter line doubles as the prompt. See
[[floating-overlays]] for where a picker may float and why the move picker
fills the reading pane instead of dropping down.
