---
id: message-list-virtualization
title: 'The message list: virtualization, infinite scroll, and a hand-built scrollbar'
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: references
  target: multi-account-ui
summary: Why rows are a fixed 60px with two tiers and an unread dot, how keyset paging drives infinite scroll and the unread filter, and why the scrollbar drag is driven from the root element.
---

# The message list: virtualization, infinite scroll, and a hand-built scrollbar

`message_list`, `scrollbar`, and `message_list_header` in
`crates/birdman-ui/src/root.rs`.

## `uniform_list` and the fixed row height

`MESSAGE_ROW_HEIGHT` is 60.0, and every row must be exactly that tall.
`uniform_list` measures the *first* row and lays out the rest from that single
measurement — which is the point: it is what lets it skip laying out anything
off-screen, so scrolling a mailbox of thousands stays cheap.

Because heights are fixed, nothing in a row may wrap freely. Do not add a
wrapping element to a row expecting it to grow.

## Row anatomy

A leading gutter holding the unread dot, then a column of two text tiers:

- **Unread dot** — `UNREAD_DOT_SIZE` (8), `rounded_full`, filled with
  `theme::ACCENT` when unread. The gutter is **always present** and only the
  fill is conditional, so read and unread rows stay aligned. Same reasoning as
  the sidebar's selection indicator.
- **Sender** — `SENDER_TEXT_SIZE` (14), always bold. Unread is signalled by the
  dot, text colour and row background, not by weight.
- **Subject** — `SUBJECT_TEXT_SIZE` (12), never bold, `truncate()`.

A third tier — a two-line body preview — was built and then removed at the
maintainer's direction. [[message-preview-pipeline]] documents what it cost to
source that text from truncated MIME; the `preview` column still exists and is
still populated.

`truncate()` is gpui's `overflow_hidden` + `whitespace_nowrap` + `text_ellipsis`
combo. It needs `min_w(px(0.))` beside it: a flex item defaults to
`min-width: auto` and will otherwise size the column from its own content
instead of shrinking to it.

## Infinite scroll over keyset paging

`MESSAGE_PAGE_LIMIT` is 200. `AppState::load_more_messages` appends the next
page when the measuring `canvas()` in `message_list` sees the viewport near the
end. Pages come from `Store::list_messages_page(folders, cursor, limit,
unread_only)` using a `(date, id)` keyset cursor, not `OFFSET`.

`has_more_messages` is simply "the cursor is `Some`", and the cursor is derived
from whether the page came back full. That is why filtering composes with
paging for free.

**`refresh_messages` must not reset to page one.** It re-reads
`messages.len()`, clamped between `MESSAGE_PAGE_LIMIT` and
`MESSAGE_REFRESH_CAP` (2000). A refresh fires on every sync event, and resetting
to the first page discarded everything scrolled past and yanked the viewport
back under the reader. This was one of two causes of an apparent "arrow keys
jump three rows" bug — the other was `ScrollStrategy::Center`, now `Nearest`.

## The unread filter

Clicking the "N unread" count in the header toggles `AppState::unread_only`,
which is passed into the store query as `AND flag_seen = 0`.

**Filtering happens in SQL, not on the loaded page.** With 8,782 messages and
737 unread, filtering client-side would return a handful of rows per page and
make infinite scroll crawl.

One deliberate wrinkle: with the filter on, opening a message marks it read, and
the next refresh would drop it out of the list while it is being read.
`refresh_messages` re-inserts the currently-selected message in date order if
the filtered page no longer contains it — but only after the cursor has been
computed, so the pinned row cannot become the cursor.

The count is not clickable while searching: search results are their own set,
and `visible_messages()` reads from them rather than from the paged folder
query.

## GPUI ships no scrollbar

There is no scrollbar widget in gpui, so this one is hand-built. Two failed
approaches are recorded in the code and worth not repeating:

1. `UniformListDecoration` — the obvious hook. It does not deliver mouse events
   to what it renders, so dragging did not work.
2. Trusting `ScrollHandle::max_offset()` — `uniform_list` does not populate it
   the way a plain scrollable `Div` does, because its layout deliberately
   bypasses the normal content-size machinery.

Instead the scrollbar is an absolutely-positioned **sibling** of the
`uniform_list`, with geometry computed from item count × `MESSAGE_ROW_HEIGHT`
and a live measured viewport height.

## Drag is driven from the root element

`ScrollbarDrag { handle, dragging, drag_start, viewport_height, content_height }`
and `drive_scrollbar_drag(target, mouse_y, window)`, invoked from **`Root`'s**
`on_mouse_move` — not the message pane's.

The reason is specific: dragging the list scrollbar and drifting sideways over
the reading pane stalled the drag. gpui keeps *receiving* mouse moves over the
child webview, but only dispatches them to elements whose bounds contain the
point, so a pane-scoped handler stops firing. A root-level handler does not.
`on_mouse_down` stays on the track itself so a drag cannot be started from a
message row.

## State that must live in `AppState`

Render functions are rebuilt every frame, so anything stored only in one would
not survive between frames: `list_scroll_handle`, `list_viewport_height`,
`list_scrollbar_dragging`, and `list_scrollbar_drag_start`. A drag spans frames;
a local would not.

`SCROLLBAR_WIDTH` is 12 with a `SCROLLBAR_THUMB_WIDTH` of 6 inset inside it, and
`SCROLLBAR_MIN_THUMB_HEIGHT` (24) keeps the thumb grabbable in a huge mailbox.
The widget is shared with the sidebar (see [[sidebar-pane-layout]]) and takes a
caller-supplied `id` — two elements sharing an id in one frame collide.

## Header ordering

`message_list_header` renders the folder name (or "Search Results") with the
search and New-message buttons, then the message/unread counts, then the search
box, following Apple Mail's mailbox header. It takes `visible` as a parameter —
`message_list`'s already-computed `s.visible_messages().to_vec()` — reused
rather than recomputed so the two cannot disagree about what is shown.

There is deliberately no "showing N" count: with infinite scroll the number
rendered is an artefact of how far you have scrolled.

## Removing a message advances the selection

`forget_selected_message` is shared by delete, archive and move, so all three
behave the same way: the next message down inherits the selection, falling back
to the one above when the last row was removed.

The successor is resolved **before** the removal, while the neighbours are
still in the list.

Opening it marks it read, because that is what selecting a message always does.
That matches Apple Mail on delete, and the alternative -- emptying the reading
pane and making the reader re-find their place -- is worse.

## Unread bubbles

Default folders only, summed across accounts in the merged view. See
[[multi-account-ui]].

## The unread filter builds the list; it does not police it

`unread_only` selects what a refresh *builds* from. A message read while you are
looking at it keeps its row, greyed, until the view is rebuilt -- re-toggling the
filter, or changing folder.

Filtering live is what made deleting one message look like it deleted several:
the refresh a delete triggers re-ran the unread query, and everything read since
the view opened vanished alongside the message actually removed.

`refresh_messages` therefore carries forward rows that are on screen and no
longer in the unread result, merged back in date order. Two constraints on that:

- **Scoped to the current folder set.** `select_folder` deliberately leaves the
  previous folder's rows up until the new ones arrive, so an unscoped carry
  would pull a whole other mailbox in.
- **Rows this window removed are already gone** from `self.messages`, so a
  delete or a move is not undone by it. A message deleted in *another* client
  lingers until the view is rebuilt, which is the price of not re-querying to
  find out.

## Read state is applied optimistically

`set_seen_locally` runs when the open is *issued*, not when it returns, and is
reverted if the command fails. Waiting a round trip to clear the unread dot made
the app feel like it had not registered the keypress -- visibly so when moving
quickly, where several rows sat marked unread behind the cursor.

## The filter does not follow you between folders

`select_folder` clears `unread_only`.

Carried across, it meant opening a folder and being shown an empty list because
everything in it happened to be read -- indistinguishable from an empty folder,
and the control that would explain it sits in the header of the list that is not
there.

## Filters are a value, not a run of booleans

`birdman_store::MessageFilter { unread, attachments }` travels from the UI through
`birdman-proto`, the service and the client into `Store::list_messages_page`.

The second filter is where this stopped being a matter of taste.
`unread_only: bool` appeared in seven signatures; adding `attachments_only:
bool` beside it would have made every call site
`messages(folders, cursor, limit, false, true)` -- two adjacent booleans the
compiler cannot tell apart, in four crates. A field is also what the *next*
filter costs, instead of another parameter everywhere.

`MessageFilter::default()` is "everything", and the clauses compose: both set
narrows to messages that are unread *and* carry an attachment, which
`filters_compose_rather_than_replace_each_other` pins.

Changing `Query::Messages` is a wire change, so `PROTOCOL_VERSION` went to 2.
That is what the `Hello` handshake is for -- an older client is told, rather
than failing later on whichever field happens to differ.

## Counts follow the list, not the search box

The header keyed its counts on whether search was *active*, so opening an empty
search box reported "0 messages" -- the mailbox appearing to empty on a
keystroke. It keys on `search_results.is_some()` now: an open, empty box is
still showing the folder, so it still reports the folder's numbers.

## Search obeys the filters

`Query::Search` carries the same `MessageFilter` the folder list does, and the
clauses are the same ones. It used to ignore them entirely, on the reasoning
that "the result set is already its own thing" -- which left the filter buttons
lit above results they had not touched, with nothing to explain why. Toggling a
filter while searching re-runs the search rather than the folder query.

## Counts are moved, not re-read

Marking a message read locally now moves the header's unread count and the
sidebar's badge with it.

This became necessary the moment the daemon's own echo was suppressed
([[daemon-and-clients]]). Before that, the announcement triggered a refresh that
re-read the counts as a side effect; suppressing it stopped the churn and took
the counts with it, so the dot cleared on the row while the header still said
"766 unread". Applying the count locally is the other half of applying the flag
locally.

Only a *real* change moves a number -- re-marking a read message read must not
decrement anything -- and the header's count covers the selected folder plus its
descendants, so a message read in a child moves the parent's number too.

`refresh_messages` additionally re-reads the sidebar's unread map, which comes
from a different query and used to lag behind the header after a sync delivered
new mail. The optimistic path keeps them in step between syncs; the authoritative
read corrects them at one.
