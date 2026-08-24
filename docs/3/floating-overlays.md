---
id: floating-overlays
title: 'Floating UI in gpui: deferred paints it, occlude protects it'
altitude: 3
topics:
- ui
- engineering/practices
relations:
- type: part_of
  target: gpui-ui-conventions
summary: Why an overlay needs both deferred and occlude, and the correctness bug that follows from getting only one of them.
---

# Floating UI in gpui: deferred paints it, occlude protects it

gpui has no popup primitive and no z-index. Anything that floats is built from
two independent mechanisms, and using one without the other produces a bug that
does not look like a layering problem.

## The two halves

**`gpui::deferred(...)`** delays painting until after the element's ancestors,
so later siblings do not paint over it. Paint order *is* the ordering.

**`.occlude()`** sets `HitboxBehavior::BlockMouse`, so mouse events stop at the
element's bounds instead of passing through to whatever is underneath.

Combined with `.absolute()` inside a `.relative()` parent, so opening the
overlay does not displace the content around it.

```rust
.child(gpui::deferred(
    div().occlude().absolute().top(px(HEADER_HEIGHT)).left(px(0.)).right(px(0.))
        // ...
))
```

## The bug from getting it half right

The account switcher shipped with `deferred` and without `occlude`. It looked
correct: the list drew on top of the folder rows.

Clicks went **through it**. The popup's second option sat directly over the
folder list's second row, so choosing the second account also selected the
second folder -- and the folder click, running later, won. The reported symptom
was "selecting the Montis account selects Flagged instead of Inbox", which
points nowhere near a z-order mistake.

The general shape: `deferred` is about pixels, `occlude` is about input, and
nothing warns when they disagree. **Any floating element needs both.**

## The constraint that decides where a popup can live

An overlay may float over gpui's own drawing. It may **not** float over the
reading pane.

The reading pane is a native child webview composited over gpui's entire layer,
so anything gpui draws on top of it renders *behind* it -- see
[[reading-pane-webview]]. `deferred` does not help; the compositing happens
outside gpui.

Two workable answers, both used here:

- **Stay inside a gpui-drawn pane.** The account switcher is 200px wide and
  lives in the sidebar, so it never reaches the reading pane.
- **Hide the webview.** The move picker, the command palette and the log panel
  all fill the reading pane, so none can avoid the overlap; instead the webview
  is hidden for as long as one is open. The condition is a plain chain of `&&`
  in `Root::render`'s match, and **every new pane-covering overlay has to be
  added to it** -- forgetting is not a compile error, it is a webview sitting
  on top of your modal.

  The same list appears a second time, deciding whether the pane may paint its
  plaintext fallback. That one was missed when the move picker was added, so
  the picker briefly exposed the fallback underneath itself.

## Backdrops are translucent

`theme::color_alpha(BG_APP, 0.82)`. An opaque backdrop hides the whole app to
show a list of a dozen items, which makes an overlay feel like a new screen
rather than something on top of the one you were reading.

The panels themselves stay opaque: text over moving content is the part that
actually hurts to read.

## Ids must be unique per frame

A floating list is rendered per item, and gpui element ids must be unique
within a frame. Two latent collisions were found this way: a fixed
`"folder-group"` id that broke as soon as an account had two orphan groups, and
a `"more-folders"` id that collided once the sidebar showed more than one
account.

Scope ids by whatever makes them unique -- `("more-folders", account.0)` -- and
prefer a real key over an index, since a filtered list's indices shift under
the user.
