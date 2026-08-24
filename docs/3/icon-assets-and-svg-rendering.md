---
id: icon-assets-and-svg-rendering
title: 'Icons: why they are bundled SVGs and how GPUI colours them'
altitude: 3
topics:
- ui
relations:
- type: part_of
  target: gpui-ui-conventions
summary: Why Unicode glyphs failed across platforms, the two edits made to every vendored Lucide file, and the alpha-mask rule that makes an uncoloured svg() invisible.
---

# Icons: why they are bundled SVGs and how GPUI colours them

`crates/birdman-ui/src/assets.rs` plus `crates/birdman-ui/assets/icons/*.svg`.

## Why not Unicode glyphs

Icons were Unicode glyphs until this existed, and that approach has no portable
answer. The UI font (Inter) carries almost none of the needed symbols, so every
icon came from per-glyph fallback into whatever font happened to match — Menlo,
Apple Symbols, Lucida Grande and Apple Color Emoji **all at once**, the last
rendering in colour beside monochrome ones. That was diagnosed by measuring the
resolved fallback per glyph.

Pinning one symbol font fixes the inconsistency but not the portability: Apple
Symbols is macOS-only, and DejaVu Sans, Noto Sans Symbols 2, Symbola and Segoe
UI Symbol are none of them present on both platforms by default. Shipping the
icons is the only version that behaves the same everywhere.

It is also what gpui expects — Zed itself ships ~294 SVGs and draws them with
`svg()`.

## The files

[Lucide](https://lucide.dev), ISC licensed, 24×24 stroked outlines. **Two local
edits were applied when they were vendored**, and both are load-bearing:

1. `currentColor` replaced with `black`.
2. `class` attributes dropped.

gpui rasterizes an SVG to an **alpha mask** and fills it with the element's
`text_color`. The file's own colour is discarded, but it must still resolve to
something opaque — and `currentColor` has no cascade to resolve against here.

## The rule that makes icons invisible

**gpui skips painting an `svg()` whose own `style.text.color` is `None`.**

Colour must be set on the **svg element itself**, not inherited from a parent
div. This is what Zed does, and getting it wrong produces icons that are
silently absent rather than mis-coloured — there is no error.

`icon(path)` and `sized_icon(path, size)` in `root.rs` set it. Hover states use
`group` / `group_hover` with a named group rather than parent inheritance, for
the same reason.

Size comes from `.size(...)`; the `width`/`height` in the file are ignored.

## The asset table

`ICONS` is a fixed `&[(&str, &[u8])]` built with `include_bytes!` rather than
reading from disk: the icons must be present in a shipped binary, and
`include_bytes!` turns a typo'd path into a compile error instead of a silently
missing icon.

A test asserts the table and the call sites agree **in both directions** — an
entry nothing renders is dead weight in the binary, and a path drawn but absent
from the table draws nothing at runtime rather than failing. It scans `root.rs`
and `state.rs`; widen `SOURCES` if another module starts drawing icons, or the
check quietly stops covering it.

## Sizing conventions

Icon buttons are square and match the text buttons' height: `CONTROL_HEIGHT`
(26) with `ICON_BUTTON_INSET` (5) on each side, giving
`ICON_SIZE = CONTROL_HEIGHT - 2 * INSET`. Inline icons in list rows use
`INLINE_ICON_SIZE` (13).
