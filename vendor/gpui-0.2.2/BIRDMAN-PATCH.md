# Birdman compatibility patch

This is `gpui` 0.2.2 under its original Apache-2.0 license, vendored unchanged
except for `src/platform/linux/x11/window.rs`: `X11Window`'s
`HasWindowHandle`/`HasDisplayHandle` impls are `unimplemented!()` stubs in the
published crate, which panics the instant a caller (here, the reading pane's
embedded webview -- see `crates/birdman-ui/src/webview.rs`) asks an X11 window
for its raw handle. That call is unavoidable on Linux: `main.rs` deliberately
steers onto XWayland for exactly this attach (see
`prefer_xwayland_if_available`), so the panic fires on first launch of
`birdman-desktop` on any Linux desktop, Wayland or X11.

The replacement bodies are copied from gpui's `main` branch
(`crates/gpui_linux/src/linux/x11/window.rs` as of the fix landing there),
with one adjustment: `main`'s `u64::from(state.display.id())` doesn't compile
against 0.2.2's `DisplayId(pub(crate) u32)`, which has no such `From` impl yet,
so this reads the tuple field directly (`state.display.id().0`) instead. Every
other piece of data read (`X11WindowStatePtr::x_window`, `::xcb`, and
`X11WindowState::display`) already exists unchanged in 0.2.2.

There is a second, unrelated change, in `src/platform/mac/window.rs`.
`window_did_change_key_status` takes the window-state mutex and, on the
spurious-`becomeKey` path only, calls `resignKeyWindow` while still holding it.
That posts `windowDidResignKey:` synchronously, re-entering the same function on
the main thread, where it blocks forever on a mutex it already owns. Every other
path in that function drops the lock before calling out; this one does not, so
the fix is to do the same -- read `native_window` out, drop the lock, then send
the message.

It deadlocks on launch, before any window appears, whenever AppKit runs its
persistent-UI restorer: `NSPersistentUIRestorer` does a batch window-ordering
pass in which one window becomes key while another resigns, which is exactly the
re-entrant sequence. Only a bundled `.app` gets state restoration, so this is
invisible to a bare `cargo build` binary and fires every time for the bundle
`scripts/install.sh` produces. `NSQuitAlwaysKeepsWindows` does not avoid it --
the restorer runs regardless.

Remove both patches once a gpui release newer than 0.2.2 is published and
Birdman upgrades to it.
