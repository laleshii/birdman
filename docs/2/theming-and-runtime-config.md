---
id: theming-and-runtime-config
title: Runtime theming and Omarchy-style hot reload
altitude: 2
topics:
- ui
- config
relations:
- type: part_of
  target: gpui-application
- type: references
  target: account-configuration
- type: references
  target: ui-slot-lists
summary: Token-based palette swappable at runtime, the [appearance] and [theme] config sections, and mtime polling that picks up a theme file edit without a restart.
---

# Runtime theming and Omarchy-style hot reload

`crates/birdman-ui/src/theme.rs` and `crates/birdman-ui/src/config.rs`.

## Tokens, not colours

Call sites keep the shape `theme::color(theme::BG_APP)`. What changed when
theming landed is that the second half is a **`Token`** — a role — resolved
against whichever `Palette` is currently loaded, rather than a literal.

```rust
static CURRENT: RwLock<Palette> = RwLock::new(Palette::DEFAULT);
pub fn color(token: Token) -> Rgba
pub fn hex(token: Token) -> u32      // for CSS text in the webview stylesheet
pub fn set_palette(palette: Palette)
```

Keeping the call-site shape identical is why this was a tractable change at
~100 sites. `hex` exists because the reading pane's injected stylesheet is CSS
text, not gpui colours.

A poisoned lock falls back to `Palette::DEFAULT` rather than panicking — losing
a colour should not take the app down.

The defaults come from ghostty's scheme, so the app sits in the same visual
family as the terminal it is developed in, plus surface steps a terminal has no
equivalent for. A terminal needs one background; a three-pane app needs several
that read as distinct without becoming stripes.

There is a unit test asserting every token maps to its **own** palette field. It
does not assert distinct colours — a theme may legitimately reuse one — it
catches the copy-pasted match arm.

## Configuration

`~/Library/Application Support/birdman/config.toml` (platform data dir) carries
two optional sections beyond `[account]`:

- `[appearance]` — `email_dark_mode` (`auto`/`always`/`never`, see
  [[email-dark-mode-adaptation]]), `toolbar_actions` (presence *and* order of
  the reading-pane toolbar buttons, including `spacer`), and `theme_file`.
- `[theme]` — hex colours per token.

`toolbar_actions` is parsed into a `Vec<ToolbarAction>` that `root.rs` renders
directly, so the toolbar's contents are data rather than code.

Note for anyone editing the config template: it is a raw string that must use
`r##"..."##`. Hex colours contain `"#`, which closes an `r#"..."#` literal.
The same clash bites in test fixtures.

## Hot reload by mtime polling

`AppState::watch_appearance` polls `watched_paths()` every
`APPEARANCE_POLL_INTERVAL` (1s) and compares an `appearance_fingerprint()`.
On a change it re-parses, calls `theme::set_palette`, and notifies.

Polling rather than a filesystem watcher is deliberate: the set of paths is two
files, the interval is human-scaled, and it is **symlink-safe**, which a naive
watcher is not. That matters because the intended workflow — the Omarchy
pattern the feature was modelled on — is a `theme_file` symlink repointed at a
different palette, where the watched path itself never changes.

Verified working live: editing the theme file recolours the running app within
a second, including the reading pane, since the injected stylesheet is rebuilt
from `theme::hex` on the next body render.

## Composition is the other half

The palette answers *what colour*; it says nothing about what a component is
made of. `[appearance.message_row]` and `[appearance.show]` answer that, using
the same file, the same hot reload, and the same "a typo costs you one setting,
not the UI" policy. Slot styles name palette tokens rather than hex, so the two
layers stay attached. See [[ui-slot-lists]].

## The reading pane has its own surface

`bg_message` (`#2f343d`), a step lighter than `bg_app`.

A message is someone else's content laid on the window, and while it shared the
app's background a forced-dark email had no edges -- it blended into the chrome
around it and the pane stopped reading as a distinct thing.

It continues the existing `bg_sidebar` -> `bg_list` -> `bg_app` progression
upward rather than introducing a new colour: the same blue-grey, one rung
further up.

Applied to the message **body** only. The header above it -- subject, sender,
toolbar -- is chrome and keeps `bg_app`.

Painted in two places for one surface: the gpui body region and the webview's
own stylesheet. The native view arrives a frame after the layout that sizes it,
so a mismatch underneath shows through until it does. The scrollbar gutter takes
the same colour, since it sits outside the document's background box and
resolves to nothing rather than to the page.

## A user stylesheet, appended not substituted

`appearance.reading_css_file` is read into `Appearance::reading_css` and
appended to the pane's stylesheet, so a reader's rules are the last word in the
cascade. Held as text rather than a path because the pane asks for it every
frame.

Appended rather than replacing, because the built-in sheet is doing two
different jobs. One is taste -- typography, palette, reading width -- all of
which is already configurable. The other is mechanism: `strip_inline_paint`, the
`*` reset, `color-scheme`. That half is what stops an email rendering
dark-on-dark, it has been got wrong three separate ways
([[email-dark-mode-adaptation]]), and when it fails it looks like an Birdman bug
rather than a config mistake. Appending gives every override without that cliff.

The stylesheet is part of the webview's cache key, hashed rather than compared
so the key does not carry a copy of it. Without that a hot reload would edit a
file and change nothing on screen.
