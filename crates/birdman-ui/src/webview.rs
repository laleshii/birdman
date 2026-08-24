//! The reading pane's HTML view: a real platform webview (WKWebView on macOS,
//! WebKitGTK on Linux) attached as a **child view** of the gpui window.
//!
//! Two constraints are the model, not bugs:
//!
//! - **Z-order is all-or-nothing.** gpui paints its whole window through one
//!   Metal layer, and this is a sibling native view composited over that layer.
//!   Nothing gpui draws can appear on top of it, and gpui's clipping and corner
//!   radii do not apply. Any dropdown or modal over the reading pane renders
//!   *behind* it; there is no fix short of not overlapping the pane.
//! - **Bounds are synced by hand**, from a measuring `canvas()` in
//!   `root::reading_pane`, one frame behind the layout that produced them.
//!
//! On Linux the attach needs an `Xlib` handle ([`xlib_compatible`]) and a
//! GTK that `main.rs` initialized ([`mark_gtk_ready`]); a pure-Wayland session
//! has no attach path at all. A refused attach is handled, not fatal:
//! [`EmailWebView::new`] returns `None` and the pane falls back to plaintext.
//!
//! That fallback executes nothing, so the webview is strictly the larger attack
//! surface: JavaScript is disabled, HTML still goes through `ammonia`, and
//! every top-level navigation is refused.

use std::fs;
use std::path::PathBuf;

use base64::Engine;
use birdman_store::MessageId;

use crate::config::EmailDarkMode;
use crate::theme;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

pub type PaneRect = (f32, f32, f32, f32);

/// Long enough that a drag-resize settles first: each frame of the drag pushes
/// it out again. See [`EmailWebView::set_bounds`].
const RESIZE_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

pub struct EmailWebView {
    webview: WebView,
    /// Compared before every `set_bounds`, so a static layout is not
    /// re-positioned every frame.
    last_bounds: Option<PaneRect>,
    /// Message *and* rendering, because the reader can change the second
    /// without changing the first. Keyed on both so an unchanged selection does
    /// not re-load, and re-scroll to top, every frame.
    loaded: Option<(MessageId, Rendering, (u64, u64))>,
    visible: bool,
    /// `gpui_scale_factor / gdk_scale_factor`, applied to every bound before it
    /// reaches `wry`. GTK3 scale factors are integers, so on a fractionally
    /// scaled display GDK rounds 2.25x down to 2x while gpui uses the true
    /// value, and every bound comes out ~11% short. `1.0` where there is no
    /// second toolkit to disagree.
    scale_correction: f32,
    /// On Linux the paint that should follow a load never gets scheduled:
    /// `wry` hands WebKitGTK a *foreign* GdkWindow, and foreign GdkWindows have
    /// no `GdkFrameClock` for GTK's paint cycle to hang off. A finished load
    /// stays invisible until something unrelated forces a redraw.
    ///
    /// Consumed by [`Self::take_load_finished`], paired with
    /// [`Self::nudge_repaint`].
    load_finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Keeps the caller asking for frames -- gpui stops drawing when idle, and
    /// a load finishing then would have no frame to be noticed in -- and bounds
    /// that, since the page-load callback arrives over IPC and may never come.
    load_deadline: Option<std::time::Instant>,
    /// Compared before reloading, so holding the pane across a fetch is not a
    /// fresh load every frame.
    placeholder: Option<u32>,
}

impl EmailWebView {
    /// `None` if the platform refuses, which is not fatal: the caller leaves
    /// the plaintext body showing underneath.
    pub fn new<W: raw_window_handle::HasWindowHandle>(
        window: &W,
        gpui_scale_factor: f32,
    ) -> Option<Self> {
        let handle = match xlib_compatible(window) {
            Ok(handle) => handle,
            Err(err) => {
                log::error!("no webview available, HTML bodies will show plaintext only: {err}");
                return None;
            }
        };
        let load_finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let webview = WebViewBuilder::new()
            // wry defaults this to `true`, which is wrong here. Once the child
            // view takes first responder, AppKit routes every keystroke to it,
            // gpui sees none, and macOS beeps at each because nothing consumes
            // them -- the window looks frozen until you click back on the list.
            .with_focused(false)
            .with_javascript_disabled()
            .with_navigation_handler(|url: String| {
                // Top-level navigation only: subresources do not come through
                // here, so remote images are unaffected.
                if url.starts_with("http://") || url.starts_with("https://") {
                    let _ = open::that_detached(&url);
                    return false;
                }
                // `about:`/`data:` is our own `load_html`; refuse everything
                // else rather than enumerating schemes.
                url.starts_with("about:") || url.starts_with("data:")
            })
            .with_bounds(rect(0.0, 0.0, 1.0, 1.0))
            .with_on_page_load_handler({
                let flag = load_finished.clone();
                move |event, _url| {
                    log::debug!(
                        "page load event: {}",
                        match event {
                            wry::PageLoadEvent::Started => "started",
                            wry::PageLoadEvent::Finished => "finished",
                        }
                    );
                    if matches!(event, wry::PageLoadEvent::Finished) {
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            })
            .build_as_child(&handle);
        // The fallback is silent by design, so a refused attach would otherwise
        // look identical to rendering that merely came out badly.
        let webview = match webview {
            Ok(webview) => webview,
            Err(err) => {
                log::error!("no webview available, HTML bodies will show plaintext only: {err}");
                return None;
            }
        };
        let _ = webview.set_visible(false);
        // An escape hatch for a display neither gpui nor GDK reports correctly.
        // From the environment, not config, like `BIRDMAN_LOG`.
        let scale_correction = std::env::var("BIRDMAN_WEBVIEW_SCALE")
            .ok()
            .and_then(|raw| raw.trim().parse::<f32>().ok())
            .unwrap_or_else(|| {
                gdk_scale_factor()
                    .map(|gdk_scale| gpui_scale_factor / gdk_scale)
                    .unwrap_or(1.0)
            });
        log::debug!(
            "webview scale correction {scale_correction} (gpui {gpui_scale_factor}, gdk {:?})",
            gdk_scale_factor()
        );
        Some(Self {
            webview,
            last_bounds: None,
            loaded: None,
            visible: false,
            scale_correction,
            load_finished,
            load_deadline: None,
            placeholder: None,
        })
    }

    /// `document` must already have been through [`prepare_document`]: this
    /// runs on **every frame** the message is visible, so it only prepends the
    /// stylesheet and loads, and only when the message actually changed.
    /// Reloading otherwise resets the reader's scroll position each frame.
    pub fn show(
        &mut self,
        message_id: MessageId,
        document: &str,
        rendering: Rendering,
        max_width: f32,
        extra_css: &str,
    ) {
        // The user stylesheet is hot-reloaded, so it is part of what "already
        // loaded" means. Hashed, since the key outlives the document.
        let style_key = (max_width.to_bits() as u64, fingerprint(extra_css));
        // Keyed on the rendering too: the reader can flip dark off for the
        // message they are already looking at.
        if self.loaded != Some((message_id, rendering, style_key)) {
            let style = document_style(rendering, max_width, extra_css);
            // Not in `prepare_document`: this depends on `force_dark`, which
            // can change under a cached document.
            let body = if rendering == Rendering::ForceDark {
                std::borrow::Cow::Owned(strip_inline_paint(document))
            } else {
                std::borrow::Cow::Borrowed(document)
            };
            if self
                .webview
                .load_html(&format!("<style>{style}</style>{body}"))
                .is_ok()
            {
                self.loaded = Some((message_id, rendering, style_key));
                self.placeholder = None;
                log::debug!("load_html issued for message {message_id:?}");
                // A stale `true` would be consumed by this document's first
                // frame and spend the nudge too early.
                self.load_finished
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                self.load_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
            }
        }
        self.set_visible(true);
    }

    pub fn hide(&mut self) {
        self.set_visible(false);
        self.placeholder = None;
    }

    /// Keeps the view mapped rather than unmapping it during a fetch. An absent
    /// child view stops covering the pane, and matching the colour behind it is
    /// not enough -- the rendering mode of an unfetched message is unknown.
    ///
    /// The spinner is CSS, not script: JavaScript is disabled here.
    pub fn show_placeholder(&mut self, background: u32) {
        if self.placeholder != Some(background) {
            let accent = theme::hex(theme::ACCENT);
            let html = format!(
                "<style>html,body{{margin:0;height:100%;background:#{background:06x};\
                 display:flex;align-items:center;justify-content:center}}\
                 .d{{width:10px;height:10px;border-radius:50%;background:#{accent:06x};\
                 animation:p 1.1s ease-in-out infinite}}\
                 @keyframes p{{0%,100%{{opacity:.25}}50%{{opacity:1}}}}</style><div class=d></div>"
            );
            if self.webview.load_html(&html).is_ok() {
                self.placeholder = Some(background);
                // The real document loads again after this, so the "already
                // showing that message" check must not match.
                self.loaded = None;
                self.load_finished
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                self.load_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
            }
        }
        self.set_visible(true);
    }

    /// The caller keeps asking for frames while this holds, so
    /// [`Self::take_load_finished`] is actually looked at.
    pub fn load_pending(&self) -> bool {
        self.load_deadline.is_some()
    }

    /// The document reported itself loaded, or waiting has gone on long enough.
    /// Either way, force the paint.
    pub fn take_load_finished(&mut self) -> bool {
        let Some(deadline) = self.load_deadline else {
            return false;
        };
        let finished = self
            .load_finished
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        if !finished && std::time::Instant::now() < deadline {
            return false;
        }
        self.load_deadline = None;
        true
    }

    /// Resizes a pixel taller and straight back, which goes through WebKit's
    /// own layout and damage path rather than the GTK frame clock that does not
    /// tick here (see `load_finished`).
    ///
    /// **Taller, never shorter**: a view briefly a pixel short shows a line of
    /// whatever is underneath, which is the flicker this exists to remove.
    pub fn nudge_repaint(&mut self) {
        let Some((x, y, width, height)) = self.last_bounds else {
            return;
        };
        log::debug!("forcing repaint after load ({width}x{height})");
        let c = self.scale_correction;
        let _ = self
            .webview
            .set_bounds(rect(x * c, y * c, width * c, (height + 1.0) * c));
        let _ = self
            .webview
            .set_bounds(rect(x * c, y * c, width * c, height * c));
    }

    fn set_visible(&mut self, visible: bool) {
        if self.visible != visible && self.webview.set_visible(visible).is_ok() {
            self.visible = visible;
            if visible {
                // Coming back from hidden, where nothing else will paint
                // it: the document is still `loaded` so `show` will not
                // reload, and the restored bounds are usually identical so
                // `set_bounds` arms nothing. A tiler collapsing and
                // reopening the pane hits this; a drag-resize never does.
                self.load_deadline = Some(std::time::Instant::now() + RESIZE_SETTLE);
            }
        }
    }

    /// Switching messages does not move the reading pane, so the previous
    /// message's rect is this one's. Knowing that lets the caller skip the
    /// hide-for-one-frame dance and swap the document in place.
    pub fn is_positioned_at(&self, bounds: PaneRect) -> bool {
        self.last_bounds == Some(bounds)
    }

    pub fn set_bounds(&mut self, bounds: PaneRect) {
        if self.last_bounds == Some(bounds) {
            return;
        }
        let (x, y, width, height) = bounds;
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        // `last_bounds` stays in gpui's space; only what reaches `wry` is
        // corrected.
        let c = self.scale_correction;
        if self
            .webview
            .set_bounds(rect(x * c, y * c, width * c, height * c))
            .is_ok()
        {
            self.last_bounds = Some(bounds);
            // A resize re-lays-out but never schedules the paint that should
            // follow -- the same missing frame clock as a finished load. Arming
            // the deadline rather than nudging here is deliberate: a
            // drag-resize pushes it out every frame, so the repaint happens
            // once when the size settles.
            self.load_deadline = Some(std::time::Instant::now() + RESIZE_SETTLE);
        }
    }
}

/// Removes `background*`, `color` and `-webkit-text-fill-color` from inline
/// styles. The stylesheet cannot do this: an inline `!important` sits at the
/// top of the author cascade, and no selector at any specificity outranks it.
///
/// **Background and colour must go as a pair** -- stripping one is worse than
/// stripping neither, and this has been wrong in both directions (light on
/// light, then dark on dark). Only runs when dark is being forced.
fn strip_inline_paint(html: &str) -> String {
    strip_style_element_paint(&strip_style_attribute_paint(html))
}

/// The same for `<style>` blocks, which win without abusing the cascade at all:
/// on **source order** (ours is prepended, theirs comes later) and on
/// **specificity** (ours is `*`, theirs can be an id).
///
/// Raising our specificity is not a fix, only a bid in an auction the sender
/// can always outbid. Removing the declarations ends the auction.
fn strip_style_element_paint(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(at) = find_ignore_case(rest, "<style") {
        let (before, tail) = rest.split_at(at);
        out.push_str(before);
        // Past the opening tag; its attributes are none of our business.
        let Some(open_end) = tail.find('>') else {
            out.push_str(tail);
            return out;
        };
        out.push_str(&tail[..=open_end]);
        let body = &tail[open_end + 1..];
        let Some(close) = find_ignore_case(body, "</style") else {
            out.push_str(&without_paint_css(body));
            return out;
        };
        out.push_str(&without_paint_css(&body[..close]));
        rest = &body[close..];
    }
    out.push_str(rest);
    out
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle)
}

/// At every nesting depth: `@media` and `@supports` wrap real rules, and
/// treating their contents as one flat declaration list would leave a sender's
/// dark-mode and mobile overrides standing.
fn without_paint_css(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;

    while let Some(open) = rest.find('{') {
        let (selector, tail) = rest.split_at(open + 1);
        out.push_str(selector);
        let Some(close) = matching_brace(tail) else {
            out.push_str(&without_paint(tail));
            return out;
        };
        let block = &tail[..close];
        // A block containing a block is a nested at-rule: recurse rather than
        // mangling its inner selectors into "declarations".
        if block.contains('{') {
            out.push_str(&without_paint_css(block));
        } else {
            out.push_str(&without_paint(block));
        }
        out.push('}');
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

fn matching_brace(rest: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (at, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_style_attribute_paint(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(at) = rest.find("style=\"") {
        let (before, tail) = rest.split_at(at + "style=\"".len());
        out.push_str(before);
        let Some(end) = tail.find('"') else {
            out.push_str(tail);
            return out;
        };
        let (declarations, after) = tail.split_at(end);
        out.push_str(&without_paint(declarations));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// `color` is matched **exactly**, never by prefix: `border-color` and
/// `text-decoration-color` are structural and stay, and a prefix match would
/// swallow `color-scheme` too.
fn without_paint(declarations: &str) -> String {
    declarations
        .split(';')
        .filter(|declaration| {
            let property = declaration
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            !property.starts_with("background")
                && property != "color"
                // Beats `color` in WebKit, so leaving it reintroduces the bug
                // for the markup that uses it.
                && property != "-webkit-text-fill-color"
        })
        .filter(|declaration| !declaration.trim().is_empty())
        .collect::<Vec<_>>()
        .join(";")
}

/// Which of the three treatments a message gets. Here rather than in the caller
/// so the reading pane and the toolbar's sun/moon cannot disagree.
///
/// The dark block must actually *paint* something. "Does `prefers-color-scheme`
/// appear anywhere" is not the test: the common pattern is a dark block that
/// swaps a logo and nothing else, from a sender assuming the client will handle
/// dark -- stepping back for them leaves light-designed text on a dark canvas.
#[cfg(test)]
fn rendering_for(dark_mode: EmailDarkMode, document: &str) -> Rendering {
    rendering_from(dark_mode, supports_dark_mode(document))
}

/// Split from [`supports_dark_mode`], which scans the whole document: both
/// callers need the decision every frame, but the document changes only on
/// selection. `AppState` keeps the flag; this keeps the rule.
pub(crate) fn rendering_from(dark_mode: EmailDarkMode, supports_dark: bool) -> Rendering {
    match dark_mode {
        EmailDarkMode::Always => Rendering::ForceDark,
        EmailDarkMode::Never => Rendering::Light,
        EmailDarkMode::Auto if supports_dark => Rendering::SenderDark,
        EmailDarkMode::Auto => Rendering::ForceDark,
    }
}

pub(crate) fn supports_dark_mode(sanitized: &str) -> bool {
    // Lowercased whole: a hand-rolled case-insensitive scan measured three
    // times slower, because `str::find` is vectorised and a byte loop is not.
    //
    // Still ~500us on a 100KB newsletter, so ask this **once per document**,
    // never per frame. `AppState::selected_supports_dark` caches it.
    let lowered = sanitized.to_ascii_lowercase();
    let mut rest = lowered.as_str();
    while let Some(at) = rest.find("prefers-color-scheme") {
        rest = &rest[at + "prefers-color-scheme".len()..];
        // Only `: dark` -- a light block says nothing about dark.
        let Some(after_colon) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        if !after_colon.trim_start().starts_with("dark") {
            continue;
        }
        // Only the block, and only after a dark query matched, so the
        // allocation is per hit rather than per document.
        if let Some(block) = media_block(rest) {
            if block.contains("color:") || block.contains("background") {
                return true;
            }
        }
    }
    false
}

/// Brace-matched, not "up to the next `}`": a media block contains whole rules,
/// so stopping at the first close brace sees only the first selector.
fn media_block(rest: &str) -> Option<&str> {
    let open = rest.find('{')?;
    let mut depth = 0usize;
    for (at, byte) in rest[open..].char_indices() {
        match byte {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[open + 1..open + at]);
                }
            }
            _ => {}
        }
    }
    None
}

/// How a message is painted. **Three ways, not two** -- collapsing "as
/// designed" into "let the engine decide" (`color-scheme: light dark`) is what
/// made the sun button useless, handing back the same dark canvas the reader
/// was trying to escape with the sender's light text still on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rendering {
    ForceDark,
    SenderDark,
    Light,
}

fn document_style(rendering: Rendering, max_width: f32, extra_css: &str) -> String {
    const TYPOGRAPHY: &str = "margin: 0; padding: 16px; \
         font-family: system-ui, -apple-system, \"Helvetica Neue\", \"Liberation Sans\", Arial, sans-serif; \
         font-size: 15px; \
         line-height: 1.5;";
    // Centers a fixed-width message and caps a fluid one. `max-width` rather
    // than `width`, so a narrower message keeps its own size; `!important`
    // because a fluid layout declares `width: 100%` on the capped element.
    let centering = if max_width > 0.0 {
        format!("body > * {{ margin-inline: auto; max-width: {max_width}px !important; }}")
    } else {
        "body > * { margin-inline: auto; }".to_string()
    };
    let centering = centering.as_str();

    // The scrollbar gutter has to match this.
    let background = document_background(rendering);

    // Matched to `root::SCROLLBAR_WIDTH`, the thumb inset by drawing into the
    // content box behind a transparent border.
    //
    // Track and corner are **painted, not transparent**: WebKit's gutter sits
    // outside the document's background box, so `transparent` resolves to
    // nothing behind it and the corner came out a hard black square.
    let scrollbar = format!(
        "::-webkit-scrollbar {{ width: 12px; height: 12px; background-color: #{background:06x}; }} \
         ::-webkit-scrollbar-track {{ background-color: #{background:06x}; }} \
         ::-webkit-scrollbar-corner {{ background-color: #{background:06x}; }} \
         ::-webkit-scrollbar-thumb {{ \
             background-color: #{thumb:06x}; \
             border-radius: 999px; \
             border: 3px solid transparent; \
             background-clip: content-box; \
         }} \
         ::-webkit-scrollbar-thumb:hover {{ background-color: #{thumb_hover:06x}; }}",
        thumb = theme::hex(theme::SCROLLBAR_THUMB),
        thumb_hover = theme::hex(theme::SCROLLBAR_THUMB_HOVER),
    );

    if rendering != Rendering::ForceDark {
        // `light dark` only where the sender demonstrably restyles for dark, or
        // the engine darkens a canvas their colours were never chosen for.
        let scheme = match rendering {
            Rendering::SenderDark => "light dark",
            _ => "light",
        };
        // Not `!important`: this is the default the message was designed
        // against, not an override.
        return format!(
            "html {{ color-scheme: {scheme}; background-color: #ffffff; }} \
             body {{ {TYPOGRAPHY} background-color: #ffffff; }} \
             {centering} \
             {scrollbar} \
             a, a * {{ cursor: pointer !important; }} \
             {extra_css}"
        );
    }
    format!(
        "html {{ color-scheme: dark; background-color: #{background:06x} !important; }} \
         body {{ {TYPOGRAPHY} background-color: #{background:06x} !important; color: #{fg:06x} !important; }} \
         *, *::before, *::after {{ \
             background-color: transparent !important; \
             background-image: none !important; \
             color: #{fg:06x} !important; \
         }} \
         hr {{ border-color: #{border:06x} !important; }} \
         {centering} \
         {scrollbar} \
         a, a * {{ color: #{link:06x} !important; }} \
         a, a * {{ cursor: pointer !important; }} \
         {extra_css}",
        fg = theme::hex(theme::TEXT_PRIMARY),
        border = theme::hex(theme::BORDER),
        link = theme::hex(theme::ACCENT),
    )
}

/// So a hot-reloaded stylesheet invalidates the loaded document without keeping
/// a copy of it to compare against.
fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        position: LogicalPosition::new(x as f64, y as f64).into(),
        size: LogicalSize::new(width as f64, height as f64).into(),
    }
}

/// gpui's X11 backend hands out `RawWindowHandle::Xcb`; `wry`'s
/// `build_as_child` pattern-matches only `RawWindowHandle::Xlib` and rejects
/// everything else. The window ID is a plain XID either way -- Xlib and XCB are
/// two client libraries on the same wire protocol, and `wry` does nothing with
/// the handle but reparent by that ID -- so re-presenting it as `Xlib` is safe.
struct XlibCompat(raw_window_handle::RawWindowHandle);

impl raw_window_handle::HasWindowHandle for XlibCompat {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: `self.0` is a plain XID, borrowed for this call just like the
        // handle it came from.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.0) })
    }
}

/// `wry`'s X11 embedding touches GDK before it looks at the handle, and GDK
/// *panics* rather than erroring if GTK was never initialized.
/// `xlib_compatible` checks this before producing an `Xlib` handle, so a failed
/// GTK init degrades to the no-webview fallback instead of aborting.
static GTK_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called once from `main.rs` after `gtk::init()` succeeds.
#[cfg(target_os = "linux")]
pub fn mark_gtk_ready() {
    GTK_READY.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn xlib_compatible<W: raw_window_handle::HasWindowHandle>(
    window: &W,
) -> Result<XlibCompat, raw_window_handle::HandleError> {
    let raw = window.window_handle()?.as_raw();
    let is_x11 = matches!(
        raw,
        raw_window_handle::RawWindowHandle::Xlib(_) | raw_window_handle::RawWindowHandle::Xcb(_)
    );
    if is_x11 && !GTK_READY.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(raw_window_handle::HandleError::NotSupported);
    }
    let raw = match raw {
        raw_window_handle::RawWindowHandle::Xcb(xcb) => raw_window_handle::RawWindowHandle::Xlib(
            raw_window_handle::XlibWindowHandle::new(xcb.window.get() as _),
        ),
        other => other,
    };
    Ok(XlibCompat(raw))
}

/// `None` if GTK is not ready or has no monitor to ask, which
/// [`EmailWebView::scale_correction`] reads as no correction.
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn gdk_scale_factor() -> Option<f32> {
    use gtk::gdk::prelude::MonitorExt;
    let display = gtk::gdk::Display::default()?;
    let monitor = display.primary_monitor().or_else(|| display.monitor(0))?;
    Some(monitor.scale_factor() as f32)
}

/// No GDK to disagree with. Gated on the same target list as the `gtk`
/// dependency in `Cargo.toml`.
#[cfg(not(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
fn gdk_scale_factor() -> Option<f32> {
    None
}

/// Public because `root::reading_pane` paints the same colour behind the native
/// view and the two must agree: a mismatch shows as a flash for the whole
/// length of an IMAP round trip.
pub fn document_background(rendering: Rendering) -> u32 {
    match rendering {
        Rendering::ForceDark => theme::hex(theme::BG_MESSAGE),
        Rendering::SenderDark | Rendering::Light => 0xffffff,
    }
}

/// Pure and gpui-free so it can run off the foreground thread, which it must:
/// embedding reads every inline attachment off disk and base64-encodes it, and
/// sanitizing parses the whole document.
pub fn prepare_document(
    html: &str,
    inline_images: &[InlineImage],
    load_remote_images: bool,
) -> String {
    let sanitized = sanitize(&embed_inline_images(html, inline_images));
    let sanitized = if load_remote_images {
        sanitized
    } else {
        let (blocked, _) = block_remote_images(&sanitized);
        format!(
            r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:">{blocked}"#
        )
    };
    linkify_bare_urls(&sanitized)
}

const BLOCKED_IMAGE: &str =
    "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

fn block_remote_images(html: &str) -> (String, usize) {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut in_style = false;
    let mut blocked = 0;

    while let Some(at) = rest.find('<') {
        let (text, tail) = rest.split_at(at);
        if in_style {
            let (text, count) = strip_remote_css_urls(text);
            out.push_str(&text);
            blocked += count;
        } else {
            out.push_str(text);
        }

        let end = tail.find('>').map(|end| end + 1).unwrap_or(tail.len());
        let mut tag = tail[..end].to_string();
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("<img ") || lower.starts_with("<img>") {
            let (cleaned, count) = strip_remote_src(&tag);
            tag = cleaned;
            blocked += count;
        }
        if lower.contains("style=") {
            let (cleaned, count) = strip_remote_css_urls(&tag);
            tag = cleaned;
            blocked += count;
        }
        out.push_str(&tag);

        if lower.starts_with("<style") {
            in_style = true;
        } else if lower.starts_with("</style") {
            in_style = false;
        }
        rest = &tail[end..];
    }

    if in_style {
        let (text, count) = strip_remote_css_urls(rest);
        out.push_str(&text);
        blocked += count;
    } else {
        out.push_str(rest);
    }
    (out, blocked)
}

fn strip_remote_src(tag: &str) -> (String, usize) {
    let mut out = tag.to_string();
    let mut blocked = 0;
    loop {
        let lower = out.to_ascii_lowercase();
        let hit = ["src=\"http://", "src=\"https://"]
            .into_iter()
            .filter_map(|needle| lower.find(needle))
            .min();
        let Some(at) = hit else { break };
        let value_start = at + "src=\"".len();
        let Some(close) = out[value_start..].find('"') else {
            break;
        };
        out.replace_range(value_start..value_start + close, BLOCKED_IMAGE);
        blocked += 1;
    }
    (out, blocked)
}

fn strip_remote_css_urls(css: &str) -> (String, usize) {
    let mut out = css.to_string();
    let mut blocked = 0;
    let mut from = 0;
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(hit) = lower[from..].find("url(").map(|at| from + at) else {
            break;
        };
        let value_start = hit + "url(".len();
        let value = lower[value_start..].trim_start_matches([' ', '\t', '\r', '\n']);
        let value = value.strip_prefix(['\'', '"']).unwrap_or(value);
        if value.starts_with("http://") || value.starts_with("https://") {
            let Some(close) = out[value_start..].find(')') else {
                break;
            };
            out.replace_range(hit..value_start + close + 1, "url(none)");
            blocked += 1;
            from = hit + "url(none)".len();
        } else {
            from = value_start;
        }
    }
    (out, blocked)
}

/// Wraps bare `http(s)://` URLs in anchors, as Gmail and Apple Mail do.
///
/// A tag walk, not a regex: a regex would rewrite URLs already inside an `href`
/// or an existing anchor, producing nested anchors and mangled attributes.
///
/// Must run **after** `sanitize`, which is what makes the walk safe -- ammonia
/// emits balanced tags, so "text between tags" is a real boundary.
fn linkify_bare_urls(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut in_anchor = 0usize;
    let mut in_style = false;

    while let Some(at) = rest.find('<') {
        let (text, tail) = rest.split_at(at);
        if in_anchor == 0 && !in_style {
            linkify_text_into(&mut out, text);
        } else {
            out.push_str(text);
        }

        let end = tail.find('>').map(|e| e + 1).unwrap_or(tail.len());
        let tag = &tail[..end];
        out.push_str(tag);

        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("<a ") || lower.starts_with("<a>") {
            in_anchor += 1;
        } else if lower.starts_with("</a") {
            in_anchor = in_anchor.saturating_sub(1);
        } else if lower.starts_with("<style") {
            in_style = true;
        } else if lower.starts_with("</style") {
            in_style = false;
        }
        rest = &tail[end..];
    }

    if in_anchor == 0 && !in_style {
        linkify_text_into(&mut out, rest);
    } else {
        out.push_str(rest);
    }
    out
}

/// `)` is included despite being legal in a URL: "(see https://example.com/x)"
/// is far more common than a trailing paren that matters.
const URL_TRAILING_TRIM: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\''];

fn linkify_text_into(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(start) = find_url_start(rest) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        // `<` cannot occur here anyway -- this is text between tags.
        let mut end = after
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"')
            .unwrap_or(after.len());
        while end > 0 && after[..end].ends_with(URL_TRAILING_TRIM) {
            end -= after[..end]
                .chars()
                .next_back()
                .map(char::len_utf8)
                .unwrap_or(1);
        }
        let url = &after[..end];
        if url.ends_with("//") {
            out.push_str(url);
        } else {
            out.push_str(&format!(r#"<a href="{url}">{url}</a>"#));
        }
        rest = &after[end..];
    }
    out.push_str(rest);
}

/// At a word boundary, so `xhttp://` is not treated as one.
fn find_url_start(text: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(found) = text[from..].find("http") {
        let at = from + found;
        let rest = &text[at..];
        let is_url = rest.starts_with("http://") || rest.starts_with("https://");
        let boundary = at == 0
            || text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '.' && c != '/' && c != '@');
        if is_url && boundary {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

/// Duplicated from `mail_render::sanitize` because `birdman-ui` cannot link
/// `mail-render` (see the root `Cargo.toml`'s build-split note); `ammonia`
/// alone pulls `html5ever`, not `stylo`, so it does not reintroduce that.
///
/// Still needed with JavaScript disabled: it strips frames, objects, forms and
/// `<link>`, none of which need script to be a problem.
///
/// `rm_tags` unwraps an element but keeps its text; `add_clean_content_tags`
/// discards the text too. Metadata content needs the second.
fn sanitize(html: &str) -> String {
    ammonia::Builder::default()
        .add_tags(["style"])
        .rm_clean_content_tags(["style"])
        // Content dropped, not just the tag: ammonia unwraps by default, and
        // templates set the title to the subject, so it renders as the subject
        // printed twice.
        .add_clean_content_tags(["title"])
        .add_generic_attributes(["style", "class", "id"])
        // Ammonia's defaults drop these, so a `width="600"` newsletter -- the
        // near-universal convention -- arrives fluid, stretched across the pane
        // with its cells collapsed.
        //
        // Layout hints only: no scripting, navigation or network. `bgcolor` is
        // presentational, so it sits below author CSS and cannot fight the
        // forced-dark stylesheet the way an inline `style` could.
        .add_tag_attributes(
            "table",
            [
                "width",
                "height",
                "bgcolor",
                "cellpadding",
                "cellspacing",
                "border",
                "valign",
            ],
        )
        .add_tag_attributes("td", ["width", "height", "bgcolor", "valign", "nowrap"])
        .add_tag_attributes("th", ["width", "height", "bgcolor", "valign", "nowrap"])
        .add_tag_attributes("tr", ["height", "bgcolor", "valign"])
        .add_tag_attributes("tbody", ["bgcolor", "valign"])
        .add_tag_attributes("body", ["bgcolor"])
        .add_url_schemes(["data"])
        .rm_tags(["iframe", "object", "embed", "form", "link", "meta"])
        .clean(html)
        .to_string()
}

/// Mirrors `birdman_store::InlineAttachment` rather than using it, so this module
/// needs no dependency on `birdman-store` for one field shape.
pub struct InlineImage {
    pub content_id: String,
    pub content_type: Option<String>,
    pub cached_path: PathBuf,
}

/// The *only* way an inline image reaches the renderer: nothing else fetches
/// anything, so a `cid:` not rewritten here simply does not render.
///
/// A plain string replace, not an HTML parse. Safe because the needle is an
/// exact content-id, and a false positive needs another attribute to contain
/// that same literal string.
fn embed_inline_images(html: &str, inline_images: &[InlineImage]) -> String {
    let mut out = html.to_string();
    for image in inline_images {
        let Ok(bytes) = fs::read(&image.cached_path) else {
            continue;
        };
        let mime = image
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let data_uri = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        for needle in [
            format!("cid:{}", image.content_id),
            format!("cid:<{}>", image.content_id),
        ] {
            out = out.replace(&needle, &data_uri);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// Pins the cost that makes `AppState::selected_supports_dark` necessary,
    /// so nobody reintroduces a per-frame call on the assumption it is cheap.
    #[test]
    fn supports_dark_mode_is_too_expensive_for_a_render_path() {
        let doc = format!("<div>{}</div>", "x".repeat(100_000));
        let started = std::time::Instant::now();
        std::hint::black_box(supports_dark_mode(&doc));
        let once = started.elapsed();
        assert!(
            once > std::time::Duration::from_micros(20),
            "{once:?} -- if this got cheap, the caching comment above is stale"
        );
    }

    #[test]
    fn an_image_swap_is_not_dark_mode_support() {
        let html = r#"<style>@media(prefers-color-scheme:dark){
            .dark-img{display:block!important;width:auto!important;visibility:inherit!important}
            .light-img{display:none!important}
        }</style><p>hi</p>"#;
        assert!(!supports_dark_mode(html));
        assert_eq!(
            rendering_for(EmailDarkMode::Auto, html),
            Rendering::ForceDark
        );
    }

    #[test]
    fn a_dark_block_that_paints_counts() {
        let html = r#"<style>@media (prefers-color-scheme: dark) {
            body { background-color: #111; }
            .t { color: #eee; }
        }</style>"#;
        assert!(supports_dark_mode(html));
        assert_eq!(
            rendering_for(EmailDarkMode::Auto, html),
            Rendering::SenderDark
        );
    }

    #[test]
    fn a_light_only_query_says_nothing_about_dark() {
        let html =
            r#"<style>@media (prefers-color-scheme: light) { body { color: #000; } }</style>"#;
        assert!(!supports_dark_mode(html));
    }

    #[test]
    fn the_whole_media_block_is_examined_not_just_its_first_rule() {
        let html = r#"<style>@media(prefers-color-scheme:dark){
            .a{display:none}
            .b{background:#000}
        }</style>"#;
        assert!(supports_dark_mode(html));
    }

    #[test]
    fn a_user_stylesheet_has_the_last_word() {
        let style = document_style(Rendering::ForceDark, 720.0, "body { font-size: 20px }");
        assert!(style.ends_with("body { font-size: 20px }"), "{style}");
        assert!(
            style.contains("background-color: transparent !important"),
            "the reset survives"
        );
    }

    #[test]
    fn no_user_stylesheet_adds_nothing() {
        let plain = document_style(Rendering::Light, 720.0, "");
        assert!(!plain.contains("  }"), "no empty tail: {plain}");
    }

    #[test]
    fn a_reading_width_caps_without_forcing() {
        let style = document_style(Rendering::ForceDark, 720.0, "");
        assert!(style.contains("max-width: 720px !important"), "{style}");
        assert!(style.contains("margin-inline: auto"), "{style}");
    }

    #[test]
    fn zero_means_no_cap_at_all() {
        let style = document_style(Rendering::ForceDark, 0.0, "");
        assert!(!style.contains("max-width"), "{style}");
        assert!(
            style.contains("margin-inline: auto"),
            "still centred: {style}"
        );
    }

    #[test]
    fn asking_for_the_senders_own_rendering_gets_a_light_canvas() {
        let style = document_style(Rendering::Light, 720.0, "");
        assert!(style.contains("color-scheme: light;"), "{style}");
        assert!(!style.contains("light dark"), "{style}");
        assert!(style.contains("background-color: #ffffff"), "{style}");
    }

    #[test]
    fn a_sender_that_restyles_keeps_the_engines_choice() {
        assert!(
            document_style(Rendering::SenderDark, 720.0, "").contains("color-scheme: light dark")
        );
    }

    #[test]
    fn an_explicit_setting_ignores_what_the_message_claims() {
        let claims_dark = r#"<style>@media(prefers-color-scheme:dark){b{color:#fff}}</style>"#;
        assert_eq!(
            rendering_for(EmailDarkMode::Always, claims_dark),
            Rendering::ForceDark
        );
        assert_eq!(
            rendering_for(EmailDarkMode::Never, claims_dark),
            Rendering::Light
        );
    }

    #[test]
    fn an_inline_important_background_is_removed_not_outranked() {
        // The real shape: an inline !important background no stylesheet beats.
        let html = r#"<div style="color: #2d2d2d;font-size: 16px;background-color: #f3f4f0 !important;">hi</div>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("background-color"), "{out}");
        assert!(
            !out.contains("#2d2d2d"),
            "the text colour goes with it: {out}"
        );
        assert!(
            out.contains("font-size: 16px"),
            "everything else survives: {out}"
        );
    }

    #[test]
    fn table_layout_attributes_survive_sanitizing() {
        let html = r##"<table width="600" cellpadding="0" cellspacing="0" border="0" bgcolor="#ffffff">
            <tr><td width="300" valign="top" bgcolor="#eeeeee">a</td><td width="300">b</td></tr>
        </table>"##;
        let out = sanitize(html);
        assert!(out.contains(r#"width="600""#), "{out}");
        assert!(out.contains(r#"cellpadding="0""#), "{out}");
        assert!(out.contains(r#"cellspacing="0""#), "{out}");
        assert!(out.contains(r#"valign="top""#), "{out}");
        assert!(out.contains(r#"width="300""#), "{out}");
    }

    #[test]
    fn allowing_layout_attributes_does_not_allow_anything_active() {
        let html = r#"<table width="600" onclick="x()"><tr><td onmouseover="y()">a</td></tr></table>
                      <img src="x.png" onerror="z()">"#;
        let out = sanitize(html);
        assert!(out.contains(r#"width="600""#), "{out}");
        assert!(!out.contains("onclick"), "{out}");
        assert!(!out.contains("onmouseover"), "{out}");
        assert!(!out.contains("onerror"), "{out}");
    }

    #[test]
    fn a_style_block_cannot_outrank_the_forced_background() {
        let html = r#"<style>
            body { background-color: #FFFFFF !important; font-size: 14px; }
            #backgroundTable { background-color: #FFFFFF !important; }
        </style><table id="backgroundTable"><tr><td>hi</td></tr></table>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("#FFFFFF"), "{out}");
        assert!(
            out.contains("font-size: 14px"),
            "the rest of the rule stays: {out}"
        );
        assert!(
            out.contains("backgroundTable"),
            "selectors are untouched: {out}"
        );
    }

    #[test]
    fn nested_at_rules_are_stripped_too() {
        let html = r#"<style>@media (max-width: 600px) {
            .wrap { background: #fff !important; padding: 0 }
        }</style>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("#fff"), "{out}");
        assert!(
            out.contains("max-width: 600px"),
            "the query survives: {out}"
        );
        assert!(out.contains("padding: 0"), "{out}");
    }

    #[test]
    fn markup_outside_style_blocks_is_left_alone() {
        let html = r#"<style>p{color:red}</style><p>text with the word background in it</p>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("red"), "{out}");
        assert!(out.contains("text with the word background in it"), "{out}");
    }

    #[test]
    fn an_unclosed_style_block_does_not_lose_the_message() {
        let html = r#"<p>before</p><style>body{background:#fff"#;
        let out = strip_inline_paint(html);
        assert!(out.contains("before"), "{out}");
    }

    #[test]
    fn an_inline_important_colour_cannot_outrank_the_forced_foreground() {
        let html = r#"<h1 style="color:#333333 !important;font-weight:700">Nieuw binnen</h1>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("#333333"), "{out}");
        assert!(out.contains("font-weight:700"), "{out}");
    }

    #[test]
    fn webkit_text_fill_colour_goes_too() {
        let out = strip_inline_paint(r#"<p style="-webkit-text-fill-color:#000;margin:0">x</p>"#);
        assert!(!out.contains("text-fill"), "{out}");
        assert!(out.contains("margin:0"), "{out}");
    }

    #[test]
    fn structural_colour_properties_are_not_paint() {
        let out = strip_inline_paint(
            r#"<td style="border-color:#ddd;text-decoration-color:#00f;color-scheme:light">x</td>"#,
        );
        assert!(out.contains("border-color:#ddd"), "{out}");
        assert!(out.contains("text-decoration-color:#00f"), "{out}");
        assert!(out.contains("color-scheme:light"), "{out}");
    }

    #[test]
    fn every_background_longhand_goes() {
        let html = r#"<td style="background:#fff;background-image:url(x);background-position:top;padding:4px">x</td>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("background"), "{out}");
        assert!(out.contains("padding:4px"), "{out}");
    }

    #[test]
    fn a_surface_and_its_text_are_removed_as_a_pair() {
        // The pair is the invariant, not either half.
        let out = strip_inline_paint(r#"<p style="color:red;background:blue;padding:2px">x</p>"#);
        assert!(!out.contains("red"), "{out}");
        assert!(!out.contains("blue"), "{out}");
        assert!(out.contains("padding:2px"), "{out}");
    }

    #[test]
    fn markup_without_style_attributes_is_untouched() {
        let html = "<p>plain</p><a href=\"https://example.com\">link</a>";
        assert_eq!(strip_inline_paint(html), html);
    }

    #[test]
    fn several_style_attributes_are_all_handled() {
        let html = r#"<div style="background:#000;color:#fff;padding:1px"><span style="background:#111">x</span></div>"#;
        let out = strip_inline_paint(html);
        assert!(!out.contains("background"), "{out}");
        assert!(!out.contains("#fff"), "{out}");
        assert!(out.contains("padding:1px"), "{out}");
    }

    #[test]
    fn an_unterminated_style_attribute_does_not_lose_content() {
        let html = r#"<div style="background:#000"#;
        assert!(strip_inline_paint(html).contains("div"));
    }

    #[test]
    fn a_bare_url_in_text_becomes_a_link() {
        let html = "<p>details:\nhttps://shop.example.com/store/x/orders/123</p>";
        let out = linkify_bare_urls(html);
        assert!(
            out.contains(r#"<a href="https://shop.example.com/store/x/orders/123">"#),
            "{out}"
        );
    }

    #[test]
    fn a_url_already_inside_an_anchor_is_left_alone() {
        let html = r#"<a href="https://example.com/a">https://example.com/a</a>"#;
        assert_eq!(linkify_bare_urls(html), html, "must not nest anchors");
    }

    #[test]
    fn a_url_inside_an_attribute_is_left_alone() {
        let html = r#"<img src="https://example.com/pixel.gif">"#;
        assert_eq!(linkify_bare_urls(html), html);
    }

    #[test]
    fn a_url_inside_a_style_block_is_left_alone() {
        // <style> survives sanitize, and an anchor inside CSS would corrupt it.
        let html = "<style>body{background:url(https://example.com/b.png)}</style>";
        assert_eq!(linkify_bare_urls(html), html);
    }

    #[test]
    fn sentence_punctuation_is_not_swallowed_into_the_href() {
        let out = linkify_bare_urls("<p>see https://example.com/page.</p>");
        assert!(out.contains(r#"href="https://example.com/page""#), "{out}");
        assert!(
            out.ends_with(".</p>"),
            "the full stop belongs to the sentence: {out}"
        );
    }

    #[test]
    fn an_escaped_ampersand_survives_into_the_href() {
        // Already escaped by sanitize(); re-escaping would break the URL.
        let out = linkify_bare_urls("<p>https://example.com/?a=1&amp;b=2</p>");
        assert!(
            out.contains(r#"href="https://example.com/?a=1&amp;b=2""#),
            "{out}"
        );
    }

    #[test]
    fn a_url_glued_to_a_word_is_not_linkified() {
        let html = "<p>notaurlhttps://example.com</p>";
        assert_eq!(linkify_bare_urls(html), html);
    }

    #[test]
    fn text_without_any_url_is_returned_unchanged() {
        let html = "<p>Just some words, and an email you@example.com.</p>";
        assert_eq!(linkify_bare_urls(html), html);
    }

    #[test]
    fn linkifying_runs_after_sanitize_in_the_real_pipeline() {
        let out = prepare_document("<p>go to https://example.com/x now</p>", &[], false);
        assert!(out.contains(r#"<a href="https://example.com/x">"#), "{out}");
    }

    #[test]
    fn remote_images_can_be_blocked_globally() {
        let out = prepare_document(
            r#"<img src="https://example.com/pixel.gif"><p style="background:url('http://example.com/x.png')">x</p>"#,
            &[],
            false,
        );
        assert!(!out.contains("example.com/pixel.gif"), "{out}");
        assert!(!out.contains("example.com/x.png"), "{out}");
        assert!(out.contains("Content-Security-Policy"), "{out}");
    }

    #[test]
    fn remote_images_can_be_allowed_globally() {
        let out = prepare_document(r#"<img src="https://example.com/pixel.gif">"#, &[], true);
        assert!(out.contains("https://example.com/pixel.gif"), "{out}");
        assert!(!out.contains("Content-Security-Policy"), "{out}");
    }

    use super::*;

    #[test]
    fn embeds_matching_inline_image_as_data_uri() {
        let dir =
            std::env::temp_dir().join(format!("birdman-html-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("logo.png");
        std::fs::write(&path, b"fake-png-bytes").unwrap();

        let html = r#"<img src="cid:logo123">"#;
        let images = vec![InlineImage {
            content_id: "logo123".to_string(),
            content_type: Some("image/png".to_string()),
            cached_path: path.clone(),
        }];

        let result = embed_inline_images(html, &images);
        assert!(result.contains("data:image/png;base64,"));
        assert!(!result.contains("cid:"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leaves_html_untouched_when_no_attachment_matches() {
        let html = r#"<img src="cid:unrelated">"#;
        assert_eq!(embed_inline_images(html, &[]), html);
    }
}
