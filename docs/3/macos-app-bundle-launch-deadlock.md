---
id: macos-app-bundle-launch-deadlock
title: The macOS .app bundle deadlocks gpui 0.2.2 on launch
altitude: 3
topics:
- ui
relations:
- type: references
  target: reading-pane-webview
---

# The macOS .app bundle deadlocks gpui 0.2.2 on launch

Patched in `vendor/gpui-0.2.2/src/platform/mac/window.rs`; see that crate's
`BIRDMAN-PATCH.md`.

## The bug

`window_did_change_key_status` takes the window-state mutex and, on the
spurious-`becomeKey` path only, calls `resignKeyWindow` while still holding it.
That posts `windowDidResignKey:` **synchronously**, which re-enters the same
function on the same thread, where it blocks forever on a mutex it already owns.
Every other path in that function drops the lock before calling out; that one
branch does not.

The stack is unambiguous:

    window_did_change_key_status  +276      (holds the mutex)
      -[NSWindow resignKeyWindow]
    window_did_change_key_status  +820      (re-enters)
      parking_lot::RawMutex::lock_slow
        _pthread_cond_wait

## Why it only appears in a bundle

AppKit's `NSPersistentUIRestorer` runs a batch window-ordering pass at launch in
which one window becomes key while another resigns — exactly the re-entrant
sequence. **Window state restoration only runs for a bundled `.app` with a
bundle identifier.** A bare executable, which is what `cargo build` and
`cargo install` produce, never triggers it.

That is the trap: the app is fine every way a developer normally runs it, and
hangs every time for anyone who installs it via `scripts/install.sh`. It hangs
before any window appears, having already loaded accounts, so the log looks like
a clean startup that simply stops.

`NSQuitAlwaysKeepsWindows = false` in `Info.plist` does **not** avoid it — the
restorer runs regardless. That was tried and reverted.

## Diagnosing this class of hang

`sample <pid>` is the tool. A deadlock shows the same stack in 100% of samples
ending in `_pthread_cond_wait` under a lock; a healthy idle app sits in
`-[NSApplication run]` waiting for events. Note also that the daemon and the
client both append to `birdman.log` across runs, so counting events in it
without segmenting by process start will mix separate launches together — that
mistake produced a confident and completely wrong diagnosis of this bug.
