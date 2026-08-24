---
id: email-dark-mode-adaptation
title: Adapting HTML email to a dark app, and the two ways it goes wrong
altitude: 2
topics:
- rendering
relations:
- type: part_of
  target: reading-pane-webview
summary: 'How force_dark is decided, why the override is aggressive, and the two traps: the sanitizer stripping class/id, and repainting every border.'
---

# Adapting HTML email to a dark app, and the two ways it goes wrong

Birdman's chrome is dark. Almost all HTML email is designed for a white page.
`document_style(force_dark)` in `crates/birdman-ui/src/webview.rs` reconciles the
two, and this is the part of the renderer most likely to produce a visual bug
report.

## The decision

`EmailWebView::show` picks a mode from config
(`appearance.email_dark_mode`, see [[theming-and-runtime-config]]):

| Mode | Behaviour |
|---|---|
| `always` | force dark |
| `never` | never force |
| `auto` (default) | force dark unless the message ships its own dark support |

"Ships its own dark support" is `sanitized.contains("prefers-color-scheme")`. A
media query is the only honest signal that a sender thought about dark
rendering at all. The previous test — "does it have any styling?" — was useless:
essentially every HTML email carries inline `style=`, so it classified almost
everything as styled and the dark treatment never applied.

## Why the forced override is aggressive

When forcing dark, colour rules go out as `!important` and deliberately flatten
the message: backgrounds stripped on `*`, text forced to the palette. A
marketing email designed around its own light background loses that design.

That cost is accepted because both alternatives are worse. Leave it alone and a
white page burns out the pane. Set only the page background and every
sender-coloured card, table and heading keeps its light fill with dark text —
unreadable, not merely plain. Backgrounds are cleared on `*` rather than
`html`/`body` for that exact reason: layout tables and wrapper `div`s are where
email puts its light fills.

## Trap 1: the sanitizer must pass `class` and `id`

`ammonia::Builder::default()` allows neither. With only `style` added as a
generic attribute, this happens:

```
in:  <table class="container" id="c" bgcolor="#ffffff">
out: <table>
```

Every `.class` and `#id` rule in the email's own `<style>` block silently stops
matching, while element selectors keep working. The symptom is bizarre and
easy to misdiagnose as a dark-mode bug: an email whose `body { background:
#181818 }` applied but whose `table.container { background: #010101 }` did not,
rendering as a **white card floating on a dark surround**.

`sanitize` therefore adds `["style", "class", "id"]`. Both are inert here —
JavaScript is disabled and `<link>` is stripped, so they are only selector hooks
for the `<style>` element already allowed.

This was never dark-mode-specific: it broke class-based styling in every email.

`bgcolor` and `width` are still stripped, which is left alone deliberately —
dropping `bgcolor="#ffffff"` helps the forced-dark path.

## Trap 2: do not repaint borders

The forced-dark rule used to include `border-color: <border> !important` on `*`.
It looks reasonable and is wrong.

HTML email uses `border: Npx solid transparent` and white-on-white borders as
**spacing devices**, constantly. Repainting every border in a visible colour
materialises stray vertical rules and empty boxes the sender never drew. The
symptom is a message that is legibly dark but structurally littered.

Borders are now left alone. A sender's *visible* border was chosen for a light
page and merely reads a little bright against dark, which is the cheaper
mistake. Only `hr` is recoloured, where a divider is unambiguously the intent.

The general lesson for this stylesheet: **an override that cannot distinguish
"invisible on purpose" from "visible" will invent content.** `background-color`
survives that test — a transparent background forced to transparent is a no-op.
`border-color` does not.

## Trap 3: inline `!important` outranks every selector

The forced-dark rules say `background-color: transparent !important` on `*`.
That loses to `style="background-color: #f3f4f0 !important"`, because an inline
`!important` declaration sits at the very top of the author cascade -- no
selector, at any specificity, can outrank it.

What made it harmful rather than merely ineffective is that the *other* half of
the override still won: the same senders set `color` inline **without**
`!important`, so text was forced to the palette's light foreground while the
background stayed pale. Light-on-light, and unreadable -- worse than not
adapting at all.

The two halves of an override can come apart. That is the same failure as the
border trap above, arriving by a different route.

The fix cannot be CSS. `strip_inline_backgrounds` removes `background*`
declarations from inline `style` attributes before the document reaches the
webview, where the cascade cannot protect them. Only when forcing dark, and
only `background*`.

## Bare URLs are linkified

Unrelated to dark mode but part of the same pipeline: plenty of senders emit a
URL as plain text and rely on the client to make it clickable. Gmail and Apple
Mail both do; a client that does not leaves a link the reader cannot follow.

`linkify_bare_urls` walks the markup rather than running a regex over it,
tracking whether it is inside an `<a>` or a `<style>` so it cannot nest anchors,
rewrite an `href`, or splice a tag into a stylesheet. It runs **after**
sanitizing, which is what makes the walk safe: ammonia emits balanced tags, so
"text between tags" is a real boundary rather than a guess.

## What is still not handled

Images. A logo drawn with a transparent background for a light page still looks
wrong, and there is no fix short of inverting it, which wrecks photographs.
Forcing `color` on `*` also flattens intentional text hierarchy — muted
secondary text becomes primary. Both are known and accepted.

## Background and colour are stripped as a pair

`strip_inline_paint` (formerly `strip_inline_backgrounds`) removes inline
`background*`, `color` and `-webkit-text-fill-color` when dark is being forced.
The pair is the invariant, and getting it wrong in *either* direction produces
unreadable mail:

- Removing neither: the sender's `background-color: … !important` won while
  their non-important `color` lost to ours. Light text on a light background.
- Removing only backgrounds: our dark background won, but a sender's
  `color: #333 !important` survived. Dark text on a dark background -- what a
  retailer's newsletter looked like, and how this was found.

Stripping `color` only changes anything for the `!important` case; a plain
inline `color` was already losing to the `*` rule. `color` is matched exactly,
not by prefix, so `border-color`, `text-decoration-color` and `color-scheme`
survive -- those are structure the sender drew, not a surface fighting the
theme.

`-webkit-text-fill-color` is included because it beats `color` in WebKit, so
leaving it would reintroduce the bug for exactly the markup that uses it.

## `prefers-color-scheme` is not a claim of dark support

The detection was "does `prefers-color-scheme` appear anywhere?", on the
reasoning that it was the only honest signal a sender had thought about dark at
all. A real newsletter falsified that. Its entire dark block is:

```css
@media(prefers-color-scheme:dark){
  .dark-img{display:block!important;width:auto!important}
  .light-img{display:none!important}
}
```

An image swap. No colours, no backgrounds. That sender is not handling dark --
they are **assuming the client will**, as Gmail and Apple Mail do, and swapping
a logo to suit. Treating it as self-styled meant stepping back from the one
message that needed help most, and the sender's own markup was the reason.

`supports_dark_mode` now requires the dark block to actually paint: `color:` or
`background` somewhere inside it. `display`, `width` and `visibility` are asset
swaps. The block is found by brace matching rather than reading to the next
`}` -- a media block contains whole rules, so stopping at the first close brace
only ever sees its first selector.

## Three renderings, not two

`Rendering::{ForceDark, SenderDark, Light}`.

The missing third is what made the sun button useless. "Not forcing dark" was
rendered as `color-scheme: light dark`, which invites the engine to darken the
canvas on its own. For a sender who really restyles, that is exactly right. For
a reader who asked to see the message *as designed*, it handed back the same
dark canvas they were escaping, with the sender's light-designed text still on
it -- the identical dark-on-dark failure, reached from the other direction.

`Light` is `color-scheme: light` plus a white background, deliberately without
`!important` so a sender's own background still wins. It is the default the
message was designed against, not an override.

`rendering_for` lives in `webview.rs` and is called by both the pane and the
toolbar, so the sun/moon cannot disagree with what is on screen.

## `<style>` blocks outrank us for free

Stripping inline paint was only half the surface. A sender's `<style>` block
does not need `!important` abuse to beat the forced-dark rules -- it beats them
on the ordinary cascade. One monitoring service's digest ships:

```css
body             { background-color: #FFFFFF !important; }
#backgroundTable { background-color: #FFFFFF !important; }
```

Both win, for different reasons:

- Against `body` we lose on **source order**. Our stylesheet is prepended,
  theirs appears later in the document, and equal selectors at equal importance
  are settled last-one-wins.
- Against `#backgroundTable` we lose on **specificity**. Our rule is `*`, the
  weakest selector CSS has; an id is near the strongest.

Raising our specificity is not a fix, only a bid in an auction the sender can
always outbid. `strip_style_element_paint` removes the declarations instead,
which ends the auction -- the same reasoning that already applied to inline
styles, arrived at a second time because only half the surface was covered.

`without_paint_css` recurses into nested blocks, because `@media` and
`@supports` wrap real rules and a flat pass would leave everything inside them
standing -- which is exactly where senders put their mobile and dark overrides.

## The reader can turn it off, per message

A sun in the reading pane toolbar (`ToolbarAction::DarkMode`, also `Cmd D`)
drops the selected message back to the sender's own rendering; a moon puts it
back. `AppState::dark_override` is a `HashMap<MessageId, bool>`.

A map, not a set of "turned off" ids. With a set, the toggle could only ever
remove adaptation -- and under `Auto` a message can already be un-adapted
because of what the sender declared, so "turn it off" did nothing at all. That
is what a moon button that appeared broken actually was. `toggle_dark_mode` now
keys off what is *currently on screen* and writes the opposite, so it always
changes something.

Per message, not global: the reason to reach for this is that *this* email
adapts badly, and a global switch would cost every other message its adaptation
to fix one. Session-scoped, because a preference this specific is not worth
persisting -- the next time that message renders correctly the setting would be
silently wrong.

Two details that are easy to get wrong:

- **The webview's cache key includes `force_dark`.** It was keyed on the message
  id alone, which is right for avoiding a re-load (and a scroll-to-top) every
  frame -- but it means toggling the mode on a message you are already reading
  shows you the same document again.
- **The icon reports what actually happened, not what was configured.** Under
  `Auto` the same setting adapts one message and leaves the next alone, so
  `selected_is_darkened` re-runs `supports_dark_mode` against the loaded
  document. A sun over a message we never touched would offer to undo something
  that never happened.

## Reading width

`appearance.reading_max_width`, default 720px, `0` to disable.

Most newsletters are a fixed-width table -- 600px is the convention -- and were
never affected. The fluid ones stretch to whatever the pane gives them, which on
a wide window is a line length nobody reads comfortably. `max-width` rather than
`width`, so a narrower message keeps its own size, and `!important` because a
fluid layout normally declares `width: 100%` on the very element being capped.

It joins the webview's cache key alongside the message id and the rendering: the
config hot-reloads, and a cache that ignored the width would keep showing the
old layout.
