---
id: gpui-ui-conventions
title: GPUI patterns this codebase has settled on
altitude: 2
topics:
- engineering/practices
relations:
- type: part_of
  target: gpui-application
- type: references
  target: gpui-redraw-traps
summary: Views as free functions over &AppState, routing events to the AppState entity instead of cx.listener, optimistic local updates with a status-line rollback, and RGBA-to-BGRA conversion.
---

# GPUI patterns this codebase has settled on

Conventions in `crates/birdman-ui` that a new change should match.

## Views are free functions over `&AppState`

`root.rs` has exactly one `impl Render` (`Root`), and everything else is a free
function taking `(s: &AppState, state: &Entity<AppState>)` and returning
`impl IntoElement` — `sidebar`, `message_list`, `reading_pane`,
`message_list_header`, the various buttons. `s` is for reading, `state` is for
handlers that need to mutate. Add a pane as another such function, not another
`Render` impl.

## Routing events to the entity instead of `cx.listener`

`cx.listener` needs a `Context<Root>`, which these free functions don't have —
and `on_key_down` requires a plain `Fn` regardless. So handlers close over the
`Entity<AppState>` and call `state.update(cx, |state, cx| ...)` directly. See
`cx_listener_search` in `root.rs` for the named example; click handlers use the
same pattern inline.

## Optimistic local update, then remote, then status line on failure

`toggle_flag` and `delete_selected` both mutate the in-memory lists immediately,
`cx.notify()`, and only then fire the IMAP operation. On failure they set
`AppState::status` (`"Flag update failed: {err}"`, `"Delete failed: {err}"`)
rather than rolling the optimistic change back. The UI stays responsive; the
status line is the single error surface.

Note the list-update idiom, which appears wherever a message's flags change
locally — both the folder list and the search results may hold the same message:

```rust
for list in [Some(&mut self.messages), self.search_results.as_mut()] {
    if let Some(list) = list {
        if let Some(m) = list.iter_mut().find(|m| m.id == message_id) {
            m.flags.flagged = target_flags.flagged;
        }
    }
}
```

## `AppState::status` is the only error surface

Sync progress, sync errors, failed flag/delete, failed on-demand body fetch, and
a failed editor launch all write to `status`. There's no toast or dialog system.
The body-fetch case was a real fix: previously silent, the reading pane just
showed "(no plaintext body)", indistinguishable from a genuinely empty message.

## Failing soft on user input

`run_search` calls `store.search(...).ok()` deliberately: FTS5 syntax errors from
arbitrary user input (unbalanced quotes, a leading `-`) degrade to "no results"
rather than surfacing a query-language error.

## The titlebar is app-drawn

The window sets `appears_transparent`, which on macOS extends the content view
up under the titlebar. That gets the titlebar painted in `BG_APP` like the rest
of the app, at two costs that are easy to hit and easy to misread:

- The traffic lights now overlap content. `root::TITLEBAR_HEIGHT` reserves the
  strip and `traffic_light_position` in `main` places them inside it. The two
  are coupled — changing one without the other puts the lights over the
  sidebar's buttons.
- **The system stops drawing the window title.** `root::titlebar` paints
  "Birdman" itself. It's centered rather than leading so it clears the traffic
  lights without needing to know their width.

## Buttons rendered in two places

`sidebar_toggle_button` takes a `currently_visible` flag and is rendered both
inside the sidebar and in the message list's header, because a button that hides
its own only means of being reached would have no way back. `new_message_button`
lives in the message list header rather than the reading pane toolbar, since that
toolbar only exists once a message is selected and starting a new message
shouldn't depend on one being open.
