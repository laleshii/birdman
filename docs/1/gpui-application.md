---
id: gpui-application
title: 'birdman-ui: the GPUI application'
altitude: 1
topics:
- ui
relations:
- type: refines
  target: birdman-overview
- type: depends_on
  target: imap-sync-engine
- type: depends_on
  target: local-message-store
- type: depends_on
  target: service-boundary
summary: 'The birdman binary: startup paths, AppState as the single UI model driving a dyn MailReceiver, the module map, and app-level keyboard handling.'
---

# birdman-ui: the GPUI application

`crates/birdman-ui` is the `birdman` binary. GPUI and `gpui_platform` are pulled
from Zed's git repo at a pinned rev (see [[gpui-dependency-pinning]]).

## Three startup paths

`main()` in `src/main.rs` branches before any window opens:

1. `config::load()` returns `Config::Unconfigured` → `open_onboarding_window`.
   `onboarding.rs` shows where the config file is and why it is not being picked
   up. See [[account-configuration]].
3. Both present → `launch_main_app`.

A keyring lookup failure is read deliberately loosely: it is not necessarily
"never saved", it could be no credential provider available at all. Either way
the fix is the same screen, so the code does not try to distinguish them.

## `launch_main_app` wires everything, and picks the protocol once

In order: open the `Store`, `ensure_account` for every configured account, wrap
it in `Arc<Mutex<_>>`, build one `AccountConfig` per account, `birdman_imap::spawn`
the engine with all of them, create a shared `SessionCache`, then split each
account in two -- its connectors into `birdman_service::AccountBackends`, its
metadata into an `AccountRuntime`. The service takes the store and the
backends; the UI takes the metadata.

Then: create the `AppState` entity, open the window, and run two loops -- one
translating the engine's `SyncEvent` into `birdman_proto::Event` and publishing
it on the service, one consuming the service's subscription into `AppState`.

The two `match` arms building the receiver and sender are the only places in the
codebase where a protocol is chosen, and they are driven by the account's
declared config types. See [[connector-boundary]] and [[account-configuration]].

## `AppState` is the whole UI model

`src/state.rs` holds one entity that every view reads from: an
`Arc<birdman_service::Service>`, the Tokio runtime handle, `accounts:
Vec<AccountRuntime>`, the folder/message lists, selection, search state, the
prepared-HTML cache, scroll state, and the loaded `Appearance`.

What it does **not** hold is the point: no store handle, no
`Arc<dyn MailReceiver>`, no `Arc<dyn MailSender>`. `AccountRuntime` is metadata
only -- id, display name, email. The UI names an account and a command; the
service knows what to do with it. See [[service-boundary]].

Everything is routed by `birdman_store::AccountId`; there is no "current account"
field, because the active one is derived from the selected folder.

What it deliberately does **not** hold, since the boundary landed: credentials,
a session cache, or account configs. Those were removed as dead once every
operation went through `dispatch`, and `state.rs` now contains zero references
to `birdman_imap`. Whether an account exists is answered by
`store.list_accounts()` — a fact about local data, not about IMAP.

The rule it enforces: sync events carry facts, and every refresh re-reads
`birdman-store` rather than maintaining a parallel in-memory copy. See
[[ui-sync-store-data-flow]].

## Module map

| File | Responsibility |
|---|---|
| `main.rs` | startup branching, wiring, protocol choice, event pump |
| `state.rs` | `AppState` — all UI state, `dispatch`, the actions that mutate it |
| `root.rs` | the three-pane render tree and app-level keyboard shortcuts |
| `compose.rs` | the compose window (multi-field, multi-line editor) |
| `text_input.rs` | shared text-editing primitives — see [[hand-rolled-text-input]] |
| `webview.rs` | the embedded reading pane — see [[reading-pane-webview]] |
| `theme.rs` | runtime palette tokens — see [[theming-and-runtime-config]] |
| `config.rs` | TOML account/appearance/theme config + template writing |
| `assets.rs` | compiled-in SVG icons — see [[icon-assets-and-svg-rendering]] |
| `cursor.rs` | AppKit cursor-rect suppression over the webview |
| `logging.rs` | file logger with size and line-length caps |
| `onboarding.rs` | the pre-main screen, when there is no usable config |
| `palette.rs` | the command registry -- see [[picker-component]] |

There is no `html_render.rs` any more (the out-of-process image renderer went
with the Blitz path), and no `password_prompt.rs`: credentials are the daemon's
business, and storing one is `birdman login`. See [[cli-client]].

## Logging

`logging.rs` writes to `birdman.log` in the data dir, capped at 2MB, with
individual lines truncated at `MAX_LINE_CHARS` (4000). The line cap exists
because IMAP servers return single error strings large enough to make the log
unreadable and slow to write.

When diagnosing, note that stderr is where panics go — a launch that redirects
stderr to `/dev/null` will show a clean log and no window.

## Keyboard shortcuts

`Root::handle_key` in `root.rs`. New/reply/reply-all/forward/search, Up/Down
navigation, Backspace/Delete, `?` for the shortcuts popup, and Cmd/Ctrl+Q and
Cmd/Ctrl+W. The block is guarded by `search_active` so the search box can be
typed into.

Two hazards live here specifically: focus must be restored whenever a focusable
element is unmounted, and a window close reached from inside another update
needs `cx.defer`. Both are in [[gpui-redraw-traps]].

## UI conventions worth matching

See [[gpui-ui-conventions]] for the patterns this code has settled on.
