//! Hands cursor control to the reading pane's webview while the pointer is
//! over it.
//!
//! gpui claims one cursor rect over its entire view and AppKit re-asserts it on
//! every mouse move, so WebKit's direct `[NSCursor set]` never survives and no
//! CSS fixes it (upstream: wry#1763). `disableCursorRects` lets WebKit win, but
//! cursor rects are the *only* mechanism gpui itself uses, so it has to be
//! scoped to the webview's rect rather than left off.

use gpui::Window;

// `disableCursorRects`/`enableCursorRects` are counted by AppKit, so toggling
// must be edge-triggered and paired one-for-one. That is what this tracks.
#[cfg(target_os = "macos")]
thread_local! {
    static YIELDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(target_os = "macos")]
pub fn yield_to_webview(window: &Window, yield_cursor: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    YIELDED.with(|yielded| {
        if yielded.get() == yield_cursor {
            return;
        }
        // Fully qualified: gpui's inherent `window_handle()` shadows the trait
        // method that yields the platform one.
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return;
        };

        // SAFETY: gpui hands out an NSView for its own window and only ever
        // touches it on the main thread, which is where element mouse handlers
        // run. `window`, `disableCursorRects`, `enableCursorRects` and
        // `invalidateCursorRectsForView:` are all plain AppKit selectors on
        // NSView/NSWindow taking no ownership.
        unsafe {
            let view: *mut AnyObject = appkit.ns_view.as_ptr().cast();
            let ns_window: *mut AnyObject = msg_send![&*view, window];
            if ns_window.is_null() {
                return;
            }
            if yield_cursor {
                let _: () = msg_send![&*ns_window, disableCursorRects];
            } else {
                let _: () = msg_send![&*ns_window, enableCursorRects];
                // Re-arm immediately, or the cursor keeps whatever WebKit last
                // set until something else invalidates.
                let _: () = msg_send![&*ns_window, invalidateCursorRectsForView: view];
            }
            yielded.set(yield_cursor);
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn yield_to_webview(_window: &Window, _yield_cursor: bool) {}

pub fn contains(rect: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    let (rx, ry, width, height) = rect;
    width > 0.0 && height > 0.0 && x >= rx && x < rx + width && y >= ry && y < ry + height
}

#[cfg(test)]
mod tests {
    use super::contains;

    const RECT: (f32, f32, f32, f32) = (100.0, 50.0, 200.0, 400.0);

    #[test]
    fn inside_and_outside() {
        assert!(contains(RECT, 150.0, 100.0));
        assert!(!contains(RECT, 99.0, 100.0));
        assert!(!contains(RECT, 150.0, 49.0));
    }

    #[test]
    fn edges_are_half_open() {
        // Half-open, so two adjacent rects can't both claim the same pixel.
        assert!(contains(RECT, 100.0, 50.0));
        assert!(!contains(RECT, 300.0, 100.0));
        assert!(!contains(RECT, 150.0, 450.0));
    }

    #[test]
    fn an_unmeasured_rect_contains_nothing() {
        assert!(!contains((0.0, 0.0, 0.0, 0.0), 0.0, 0.0));
    }
}
