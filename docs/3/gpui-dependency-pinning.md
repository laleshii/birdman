---
id: gpui-dependency-pinning
title: GPUI is pinned to a Zed git rev, on purpose
altitude: 3
topics:
- engineering/practices
relations:
- type: part_of
  target: gpui-application
summary: gpui and gpui_platform come from Zed's git repo at a hardcoded rev in each crate rather than via [patch], and why that workaround exists.
---

# GPUI is pinned to a Zed git rev, on purpose

`gpui` and `gpui_platform` are not stable crates.io releases. They're pulled
from `https://github.com/zed-industries/zed` at
`rev = "00c0e96e769062e373203c62830f510fa121db76"`, declared directly in
`crates/birdman-ui/Cargo.toml`.

Left unpinned they would float on `main` — pre-1.0, with frequent breaking
changes. Pinning is not optional maintenance debt; it's what keeps the build
reproducible.

## Why the rev is repeated per-crate instead of a `[patch]`

The root `Cargo.toml` records the attempt: `cargo` refused to override an
unpinned git source with a pinned rev of the same URL, erroring with
"patches must point to different sources". So every crate that depends on gpui
declares the rev itself.

If you bump the rev, bump it in **every** crate that names it, or Cargo will
resolve two separate checkouts.

## The known future problem

The root manifest flags this explicitly: `gpui-component`, if added, depends on
gpui via git with **no rev of its own**, which would pull a second, unpinned
checkout. Options noted for when that happens: `[patch]` again with whatever
incantation works against `gpui-component`'s actual manifest, a vendored fork of
its `Cargo.toml`, or an upstream fix.

`gpui_platform` also carries explicit features here: `font-kit`, `x11`, and
`wayland` — this is a Linux-targeted build.
