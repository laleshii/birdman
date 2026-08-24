---
id: ui-font-selection
title: The UI font is chosen at runtime, not hardcoded
altitude: 3
topics:
- ui
relations:
- type: part_of
  target: gpui-application
summary: gpui matches font families by literal name with no generic fallback, so a wrong name fails silently and takes bold with it; main::ui_font_family picks the first family that is actually installed.
---

# The UI font is chosen at runtime, not hardcoded

`main::ui_font_family` walks a best-first candidate list against
`cx.text_system().all_font_names()` and returns the first family that is
actually installed. `Root::font_family` holds the result.

## Why not just name a font

gpui's cosmic-text backend matches a requested family by **literal string**
against font files' embedded family names. There is no generic-alias
resolution: `"sans-serif"` matches zero faces. A name that matches nothing
doesn't error — it falls through to whatever font matching lands on.

That failure has a specific and misleading tell. With only one face in the
resolved family, `font_weight(BOLD)` has nothing to choose between and
**silently becomes a no-op**. The bug reads as "bold isn't working", which
sends you looking at the element tree rather than at the font name. It has
already happened once here: the family was pinned to `"Liberation Sans"`,
correct on Linux and not installed on macOS at all.

Every candidate in the list registers both a Regular and a Bold face — the
message list's sender line depends on it.

## Order

`.SF NS` first. That's the macOS system UI font (the leading dot is Apple's own
naming, not a typo). It is deliberately ahead of `Helvetica Neue`, which was the
previous macOS pick and is tight and hard to read at 11–14px — which is
precisely why Apple stopped using it as the system font. If UI text ever looks
cramped again, check which family actually resolved before adjusting sizes.

gpui exposes no `letter-spacing`/tracking style helper, so the family is the
only lever available for that.

## What actually resolved, and why icons are a fallback problem

On the machine this was built on the list resolves to **Inter**, not `.SF NS`:
gpui's font database doesn't list the hidden macOS system fonts, so the
dot-prefixed name matches nothing and the next candidate wins. That's fine —
Inter is a better UI face than Helvetica Neue at these sizes, which was the
original complaint — but it has a consequence worth knowing.

Inter contains almost none of the app's icon glyphs. Every one of them arrives
through **per-glyph font fallback**, and different codepoints land in different
fonts:

| glyph | falls back to |
|---|---|
| `⌕` search, `⚙` gear, `◧` sidebar | Menlo |
| `⟳` sync, `⧉` attachment | Apple Symbols |
| `▸` disclosure | Lucida Grande |
| `★` flag | Inter itself |
| `🔍` `📎` | Apple Color Emoji |

That last row is the trap: emoji-presentation codepoints render in **colour**
next to monochrome symbols, and the icon set stops looking like a set. Both
were in use until this was measured.

## Icons are now pinned, not fallen back to

`main::icon_font_family` resolves a **separate** family for icon glyphs, stored
on `AppState::icon_font` and applied with `.font_family(...)` on each icon
element. Relying on fallback meant four different fonts in one toolbar; pinning
one means the icons are a set by construction.

Apple Symbols leads the candidate list because it measured most complete for
this app's glyphs — **14/16**, against Menlo's 10/16 (Menlo lacks `⟳`, `⧉` and
`☰`), Arial Unicode MS 11/16, STIXGeneral 10/16, Lucida Grande 4/16. Menlo is
the intuitive guess and it is not the right one.

Re-measure before adding an icon rather than assuming coverage;
`CTFontGetGlyphsForCharacters` against a candidate family answers it in a few
lines, and `CTFontCreateForString` tells you where an unmatched codepoint would
otherwise land.
