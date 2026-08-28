use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, point, prelude::*, px, uniform_list, App, Context, Entity, FocusHandle,
    FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Render, Window,
};

use chrono::Datelike as _;

use crate::config::{MessageSlot, ToolbarAction};
use crate::state::AppState;
use crate::theme;

pub struct Root {
    pub state: Entity<AppState>,
    pub focus_handle: FocusHandle,
    /// Lazily created on the first render: building it needs the platform
    /// window handle, which is only in hand from inside `render`. `None` means
    /// the platform refused it and the plaintext part shows underneath.
    pub webview: Option<crate::webview::EmailWebView>,
    /// The pane rect is measured a frame behind, and the header changes height
    /// with the message, so showing the webview on the frame the selection
    /// changes paints it at the *previous* message's geometry. Holding it back
    /// one frame removes the jump.
    pub webview_positioned_for: Option<birdman_store::MessageId>,
    /// Resolved against the installed fonts by `main::ui_font_family`; a
    /// wrong-but-plausible family name fails silently.
    pub font_family: gpui::SharedString,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The catch-all: anything blocking the UI thread shows up here as a
        // frame over budget, whether or not anybody thought to time it.
        let _frame =
            birdman_config::logging::Timed::new("render", birdman_config::logging::Timed::FRAME);
        let state = self.state.clone();

        // Driven from here, not `reading_pane`: it needs `Window`, and a native
        // child view is not part of the element tree at all -- it is positioned
        // by side effect, one frame behind the layout that measures it.
        if self.webview.is_none() {
            self.webview = crate::webview::EmailWebView::new(&*window, window.scale_factor());
        }

        let s = state.read(cx);
        if let Some(view) = self.webview.as_mut() {
            let rect = s.reading_pane_rect.get();
            // Any gpui overlay over the reading pane must hide the webview
            // first: it is a native child view composited over gpui's whole
            // layer, so an overlay drawn on top of it renders *behind* it.
            match (s.selected_message, s.selected_html_source.as_deref()) {
                (Some(id), Some(html))
                    if rect.2 > 0.0 && rect.3 > 0.0 && !s.overlay_covers_reading_pane() =>
                {
                    // Before `set_bounds`, which makes it true by side effect.
                    let already_placed = view.is_positioned_at(rect);
                    view.set_bounds(rect);
                    // Only once this message's layout has been measured -- the
                    // first frame after a selection still holds the previous
                    // rect. Unless it is the rect the view already has, where
                    // hiding and re-showing is a blank frame between documents.
                    if self.webview_positioned_for == Some(id) || already_placed {
                        self.webview_positioned_for = Some(id);
                        view.show(
                            id,
                            html,
                            s.selected_rendering(),
                            s.appearance.reading_max_width,
                            &s.appearance.reading_css,
                        );
                        // `load_html` is async and on Linux the paint that
                        // should follow is never scheduled (see
                        // `EmailWebView::load_finished`), so keep frames coming
                        // until the load reports back.
                        if view.take_load_finished() {
                            view.nudge_repaint();
                        } else if view.load_pending() {
                            window.request_animation_frame();
                        }
                    } else {
                        view.hide();
                        self.webview_positioned_for = Some(id);
                        // Must be `request_animation_frame`, never `refresh()`:
                        // `refresh` is a no-op while the window is drawing, and
                        // this runs mid-draw.
                        window.request_animation_frame();
                    }
                }
                // Hold the pane with a placeholder rather than unmapping the
                // view -- see `EmailWebView::show_placeholder`.
                (Some(_), None)
                    if (s.selected_body_loading || s.selected_html_pending)
                        && rect.2 > 0.0
                        && rect.3 > 0.0
                        && !s.overlay_covers_reading_pane() =>
                {
                    view.set_bounds(rect);
                    view.show_placeholder(s.last_document_background.unwrap_or_else(|| {
                        crate::webview::document_background(s.selected_rendering())
                    }));
                    if view.take_load_finished() {
                        view.nudge_repaint();
                    } else if view.load_pending() {
                        window.request_animation_frame();
                    }
                }
                _ => {
                    view.hide();
                    self.webview_positioned_for = None;
                }
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event, window, cx| this.handle_key(event, window, cx)))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::color(theme::BG_APP))
            .text_color(theme::color(theme::TEXT_PRIMARY))
            .font_family(self.font_family.clone())
            .text_size(px(13.0))
            // Scrollbar drags are driven from the **root**: gpui delivers
            // `on_mouse_move` only while the cursor is inside the handler's own
            // element, so a pane's handler stops firing the moment a drag
            // drifts out of it -- and the neighbouring reading pane is covered
            // by a webview that swallows the events entirely.
            .on_mouse_move({
                let list = scrollbar_drag_targets(s);
                let pane_rect = s.reading_pane_rect.clone();
                // Only with a body loaded: nothing to hand the cursor to
                // otherwise.
                let webview_showing =
                    s.selected_message.is_some() && s.selected_html_source.is_some();
                move |event: &gpui::MouseMoveEvent, window: &mut Window, _cx: &mut App| {
                    let over_webview = webview_showing
                        && crate::cursor::contains(
                            pane_rect.get(),
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                        );
                    crate::cursor::yield_to_webview(window, over_webview);
                    if !event.dragging() {
                        // The release happened somewhere we never saw it -- over
                        // the webview, typically.
                        for target in &list {
                            target.dragging.set(false);
                        }
                        return;
                    }
                    for target in &list {
                        drive_scrollbar_drag(target, f32::from(event.position.y), window);
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let list = scrollbar_drag_targets(s);
                move |_event: &gpui::MouseUpEvent, _window: &mut Window, _cx: &mut App| {
                    for target in &list {
                        target.dragging.set(false);
                    }
                }
            })
            .relative()
            .children(titlebar())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .when(s.sidebar_visible, |el| el.child(sidebar(s, &state)))
                    .child(message_list(s, &state))
                    .child(reading_pane(s, &state)),
            )
            .when(s.move_picker_open, |el| {
                el.child(move_picker_overlay(s, &state))
            })
            .when(s.palette_open, |el| el.child(palette_overlay(s, &state)))
            .when(s.logs_open, |el| el.child(logs_overlay(s, &state)))
            .when(!s.notifications.is_empty(), |el| {
                el.child(notifications(s, &state))
            })
            .when_some(s.subject_menu, |el, at| el.child(subject_menu(&state, at)))
    }
}

/// Guarded by `search_active`: key events bubble here from the focused search
/// box, since both it and this outer div register bubble-phase handlers.
impl Root {
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Paired with `SEARCHKEY`: one keystroke should produce at most one of
        // each. Two of either names a doubly-registered element.
        log::debug!(
            "ROOTKEY {:?} search_active={}",
            event.keystroke.key,
            self.state.read(cx).search_active
        );
        let search_active = self.state.read(cx).search_active;
        let key = event.keystroke.key.as_str();

        if key == "escape" && self.state.read(cx).subject_menu.is_some() {
            self.state
                .update(cx, |state, cx| state.close_subject_menu(cx));
            return;
        }
        if self.state.read(cx).logs_open {
            match event.keystroke.key.as_str() {
                "escape" => self.state.update(cx, |state, cx| state.set_logs(false, cx)),
                "r" => self.state.update(cx, |state, cx| state.restart_daemon(cx)),
                _ => {}
            }
            return;
        }
        if self.state.read(cx).account_picker_open {
            self.state
                .update(cx, |state, cx| state.account_picker_key(event, cx));
            return;
        }
        if self.state.read(cx).palette_open {
            self.state
                .update(cx, |state, cx| state.palette_key(event, window, cx));
            return;
        }
        // Modal: letting Delete or Reply through would act on the mail behind.
        if self.state.read(cx).move_picker_open {
            self.state
                .update(cx, |state, cx| state.move_picker_key(event, cx));
            return;
        }
        if search_active {
            match key {
                "escape" => self
                    .state
                    .update(cx, |state, cx| state.close_search(window, cx)),
                "up" => self
                    .state
                    .update(cx, |state, cx| state.select_adjacent(-1, cx)),
                "down" => self
                    .state
                    .update(cx, |state, cx| state.select_adjacent(1, cx)),
                _ => {}
            }
            return;
        }

        let m = &event.keystroke.modifiers;
        // Most palette bindings live here; `crate::palette`'s test pins that
        // the two agree.
        if m.secondary() && !m.shift {
            // A header selection wins Cmd+C: it is only ever set by the reader
            // having just dragged over something.
            if key == "c"
                && self
                    .state
                    .update(cx, |state, cx| state.copy_header_selection(cx))
            {
                return;
            }
            match key {
                "n" => self.state.update(cx, |state, cx| state.compose_new(cx)),
                "r" => self.state.update(cx, |state, cx| state.reply(false, cx)),
                "f" => self
                    .state
                    .update(cx, |state, cx| state.toggle_search(window, cx)),
                "u" => self
                    .state
                    .update(cx, |state, cx| state.toggle_unread_only(cx)),
                "i" => self
                    .state
                    .update(cx, |state, cx| state.toggle_attachments_only(cx)),
                "b" => self.state.update(cx, |state, cx| state.toggle_sidebar(cx)),
                "l" => self
                    .state
                    .update(cx, |state, cx| state.toggle_flag_selected(cx)),
                "e" => self
                    .state
                    .update(cx, |state, cx| state.archive_selected(cx)),
                "d" => self
                    .state
                    .update(cx, |state, cx| state.toggle_dark_mode(cx)),
                _ => {}
            }
            return;
        }
        if m.secondary() && m.shift {
            match key {
                "r" => self.state.update(cx, |state, cx| state.reply(true, cx)),
                "f" => self.state.update(cx, |state, cx| state.forward(cx)),
                "s" => self.state.update(cx, |state, cx| state.sync_now(cx)),
                "a" => self
                    .state
                    .update(cx, |state, cx| state.toggle_account_picker(cx)),
                "m" => self
                    .state
                    .update(cx, |state, cx| state.set_move_picker(true, cx)),
                _ => {}
            }
            return;
        }
        if m.control || m.platform || m.alt {
            return;
        }
        match key {
            "up" => self
                .state
                .update(cx, |state, cx| state.select_adjacent(-1, cx)),
            "down" => self
                .state
                .update(cx, |state, cx| state.select_adjacent(1, cx)),
            "backspace" | "delete" => self.state.update(cx, |state, cx| state.delete_selected(cx)),
            "/" => self
                .state
                .update(cx, |state, cx| state.open_search(window, cx)),
            _ => {}
        }
    }
}

/// Only the folder list may grow (`flex_1`); the header and footer are
/// `flex_shrink_0`. Load-bearing, not cosmetic: without it a long folder list
/// makes flexbox squash its siblings toward zero height instead of scrolling.
fn sidebar(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    // A plain scrollable `Div`, so unlike `message_list` the handle populates
    // `max_offset()`/`bounds()` and nothing has to be derived by measuring.
    let scroll_handle = s.sidebar_scroll_handle.clone();
    let dragging = s.sidebar_scrollbar_dragging.clone();
    let viewport_height = f32::from(scroll_handle.bounds().size.height);
    let overflow_y = f32::from(scroll_handle.max_offset().height);
    let content_height = viewport_height + overflow_y;
    let show_scrollbar = s.appearance.show.scrollbars && overflow_y > 0.0 && viewport_height > 0.0;

    // Offsets run negative as the list scrolls down, so `max_offset_y` is the
    // most-negative offset.
    let max_offset_y = -overflow_y;
    let current_offset_y = f32::from(scroll_handle.offset().y);
    let scroll_fraction = if max_offset_y < 0.0 {
        (current_offset_y / max_offset_y).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_height = if content_height > 0.0 {
        (viewport_height * viewport_height / content_height).max(SCROLLBAR_MIN_THUMB_HEIGHT)
    } else {
        SCROLLBAR_MIN_THUMB_HEIGHT
    };
    let max_thumb_top = (viewport_height - thumb_height).max(0.0);
    let thumb_top = scroll_fraction * max_thumb_top;

    let (defaults, others): (Vec<_>, Vec<_>) = s
        .visible_folders()
        .into_iter()
        .partition(|f| crate::state::is_default_folder(f));
    let account_id = s
        .account_scope
        .account()
        .or_else(|| s.accounts.first().map(|a| a.id));
    let multi_account = s.accounts.len() > 1;
    let account_label = scope_label(s, s.account_scope);

    let hidden: Vec<(
        birdman_store::AccountId,
        Option<String>,
        Vec<&birdman_store::Folder>,
    )> = if s.is_merged_view() {
        s.accounts
            .iter()
            .filter_map(|account| {
                let list: Vec<_> = s
                    .folders
                    .iter()
                    .filter(|f| f.account_id == account.id && !crate::state::is_default_folder(f))
                    .collect();
                (!list.is_empty()).then(|| (account.id, Some(account.display_name.clone()), list))
            })
            .collect()
    } else {
        account_id
            .filter(|_| !others.is_empty())
            .map(|id| vec![(id, None, others)])
            .unwrap_or_default()
    };
    // A fixed key in the merged view, since it belongs to no single account.
    const MERGED_MORE_KEY: u64 = u64::MAX;
    let more_key = if s.is_merged_view() {
        MERGED_MORE_KEY
    } else {
        account_id.map(|a| a.0 as u64).unwrap_or(0)
    };

    let drag_start = s.sidebar_scrollbar_drag_start.clone();

    let down_handle = scroll_handle.clone();
    let down_dragging = dragging;
    let down_drag_start = drag_start.clone();
    let on_down = move |event: &MouseDownEvent, _window: &mut Window, _cx: &mut App| {
        down_drag_start.set((
            f32::from(event.position.y),
            f32::from(down_handle.offset().y),
        ));
        down_dragging.set(true);
    };

    div()
        .flex()
        .flex_col()
        .w(px(200.0))
        .h_full()
        .flex_shrink_0()
        .bg(theme::color(theme::BG_SIDEBAR))
        .border_r_1()
        .border_color(theme::color(theme::BORDER))
        // Asymmetric: the scrollbar is pinned to this box's right edge, so
        // padding there would leave it floating in the middle of a gap.
        .pl_2()
        .pr_0p5()
        .py_2()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .pr_1p5()
                .child(sidebar_toggle_button(state, true)),
        )
        .child(account_header(s, state, &account_label, multi_account))
        .child(
            // `min_h(0)` matters as much as `flex_1`: a flex item's default
            // `min-height: auto` refuses to shrink below its content, so the
            // scroll container would grow to fit every folder and never scroll.
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    div()
                        .id("folder-list")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&scroll_handle)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(defaults.into_iter().map(|f| folder_row(s, state, f)))
                        .children(
                            (!hidden.is_empty()).then(|| more_folders_header(s, state, more_key)),
                        )
                        .when(s.sidebar_more_expanded, |el| {
                            el.children(hidden.into_iter().map(|(id, group_label, list)| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .children(group_label.map(|label| {
                                        div()
                                            .flex_shrink_0()
                                            .mt_1()
                                            .px_2()
                                            .text_size(px(10.0))
                                            .text_color(theme::color(theme::TEXT_MUTED))
                                            .truncate()
                                            .child(label)
                                    }))
                                    .children(grouped_folder_rows(s, state, &list, id))
                            }))
                        }),
                )
                .when(show_scrollbar, |el| {
                    el.child(scrollbar(
                        "folder-list-scrollbar",
                        thumb_top,
                        thumb_height,
                        on_down,
                    ))
                }),
        )
        .child({
            // Archive/move/delete/flag in flight take priority over the sync
            // state -- the reader just did something and wants to know it's
            // still working, the way Apple Mail's status area shows "Deleting
            // 2 messages" over its usual idle text.
            let activity = s.activity_summary();
            let status = activity
                .clone()
                .unwrap_or_else(|| s.status.clone().unwrap_or_default());
            let is_error = activity.is_none() && status.starts_with("Sync error");
            div()
                .flex_shrink_0()
                .w_full()
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .pr_1p5()
                .child(div().flex_shrink_0().child(sync_button(state)))
                .child({
                    let open_logs = state.clone();
                    div()
                        .id("sync-status")
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .text_color(theme::color(if is_error {
                            theme::DANGER
                        } else {
                            theme::TEXT_MUTED
                        }))
                        .text_size(px(11.0))
                        .cursor_pointer()
                        .hover(|el| el.text_color(theme::color(theme::TEXT_PRIMARY)))
                        .on_click(move |_, _, cx| {
                            open_logs.update(cx, |state, cx| {
                                let open = !state.logs_open;
                                state.set_logs(open, cx);
                            });
                        })
                        // The dot sits outside the truncating box and does
                        // not shrink. Inside it, `justify_center` pushed the
                        // overflow out of both edges and the clip took the dot
                        // with it, so the one element saying "still working"
                        // disappeared exactly when the text got long enough to
                        // be worth reading.
                        .when(activity.is_some(), |el| {
                            el.child(div().flex_shrink_0().child(spinner()))
                        })
                        .child(div().min_w(px(0.0)).truncate().child(status))
                })
                .child(div().flex_shrink_0().child(settings_button(state)))
        })
}

/// Geometry is passed rather than read from the handle: a `uniform_list`
/// populates neither `bounds()` nor `max_offset()`, so the message list derives
/// its own from the row count and a measured viewport.
fn scrollbar_drag_targets(s: &AppState) -> [ScrollbarDrag; 2] {
    let list_viewport = s.list_viewport_height.get();
    let list_content = s.visible_messages().len() as f32 * s.appearance.message_row.height();

    let folder_handle = s.sidebar_scroll_handle.clone();
    let folder_viewport = f32::from(folder_handle.bounds().size.height);
    let folder_content = folder_viewport + f32::from(folder_handle.max_offset().height);

    [
        ScrollbarDrag {
            handle: s.list_scroll_handle.0.borrow().base_handle.clone(),
            dragging: s.list_scrollbar_dragging.clone(),
            drag_start: s.list_scrollbar_drag_start.clone(),
            viewport_height: list_viewport,
            content_height: list_content,
        },
        ScrollbarDrag {
            handle: folder_handle,
            dragging: s.sidebar_scrollbar_dragging.clone(),
            drag_start: s.sidebar_scrollbar_drag_start.clone(),
            viewport_height: folder_viewport,
            content_height: folder_content,
        },
    ]
}

struct ScrollbarDrag {
    handle: gpui::ScrollHandle,
    dragging: Rc<Cell<bool>>,
    drag_start: Rc<Cell<(f32, f32)>>,
    viewport_height: f32,
    content_height: f32,
}

fn drive_scrollbar_drag(target: &ScrollbarDrag, mouse_y: f32, window: &mut Window) {
    if !target.dragging.get() {
        return;
    }
    let overflow_y = (target.content_height - target.viewport_height).max(0.0);
    if overflow_y <= 0.0 || target.viewport_height <= 0.0 {
        return;
    }
    let thumb_height = (target.viewport_height * target.viewport_height / target.content_height)
        .max(SCROLLBAR_MIN_THUMB_HEIGHT);
    let max_thumb_top = (target.viewport_height - thumb_height).max(0.0);
    if max_thumb_top <= 0.0 {
        return;
    }
    // Content pixels per pixel the thumb travels along its shorter track.
    let content_per_track_px = overflow_y / max_thumb_top;
    let (start_mouse_y, start_offset_y) = target.drag_start.get();
    // Dragging down (positive delta) makes the offset more negative.
    let delta = (mouse_y - start_mouse_y) * content_per_track_px;
    target.handle.set_offset(point(
        px(0.0),
        px((start_offset_y - delta).clamp(-overflow_y, 0.0)),
    ));
    window.refresh();
}

fn folder_row(
    s: &AppState,
    state: &Entity<AppState>,
    folder: &birdman_store::Folder,
) -> impl IntoElement {
    let label = crate::state::sidebar_folder_name(folder);
    nested_folder_row(s, state, folder, label, false, None)
}

/// `group` is `Some((key, expanded))` when this folder parents nested ones. The
/// chevron calls `stop_propagation` so expanding does not also select.
fn nested_folder_row(
    s: &AppState,
    state: &Entity<AppState>,
    folder: &birdman_store::Folder,
    label: String,
    indented: bool,
    group: Option<(String, bool)>,
) -> impl IntoElement {
    let id = folder.id;
    let selected = s.selected_folder == Some(id);
    let icon_path = crate::state::sidebar_folder_icon(folder);
    let group_key = group.as_ref().map(|(key, _)| key.clone());
    let state = state.clone();
    div()
        .id(("folder", id.0 as u64))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .when(indented, |el| el.pl_5())
        .py_1()
        .rounded_md()
        // Always in the layout, merely transparent when unselected: adding the
        // border only on selection shunts the row 2px sideways on every click.
        .border_l_2()
        .border_color(gpui::transparent_black())
        .when(selected, |el| {
            el.bg(theme::color(theme::BG_SELECTED))
                .border_color(theme::color(theme::ACCENT))
        })
        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| {
                state.select_folder(id, cx);
                if let Some(key) = &group_key {
                    state.toggle_sidebar_group(key, cx);
                }
            });
        })
        .child(
            sized_icon(icon_path, INLINE_ICON_SIZE).text_color(theme::color(if selected {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_MUTED
            })),
        )
        // On the label, not the row: on the row it clips the icon instead.
        .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
        // `flex_shrink_0` so a long folder name truncates rather than squashing
        // the number.
        .children(s.unread_badge(folder).map(|count| {
            div()
                .flex_shrink_0()
                .px_1p5()
                .rounded_full()
                .text_size(px(10.0))
                .bg(theme::color(if selected {
                    theme::ACCENT
                } else {
                    theme::BG_HOVER
                }))
                .text_color(theme::color(if selected {
                    theme::BG_APP
                } else {
                    theme::TEXT_SECONDARY
                }))
                .child(count.to_string())
        }))
        .children(group.map(|(_, expanded)| disclosure_icon(expanded)))
}

fn disclosure_icon(expanded: bool) -> impl IntoElement {
    sized_icon(
        if expanded {
            "icons/chevron-down.svg"
        } else {
            "icons/chevron-right.svg"
        },
        INLINE_ICON_SIZE,
    )
}

/// One level of nesting. A group whose parent folder is not itself in the list
/// still gets a header, or its children have nowhere to appear.
///
/// `account` scopes both the expansion state and the element ids: two accounts
/// routinely have identically-named groups, which would otherwise expand
/// together and collide as gpui ids.
fn grouped_folder_rows(
    s: &AppState,
    state: &Entity<AppState>,
    others: &[&birdman_store::Folder],
    account: birdman_store::AccountId,
) -> Vec<gpui::AnyElement> {
    let mut children: Vec<(&str, Vec<&birdman_store::Folder>)> = Vec::new();
    for folder in others {
        let Some(key) = crate::state::sidebar_folder_group(folder) else {
            continue;
        };
        match children.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, group)) => group.push(folder),
            None => children.push((key, vec![folder])),
        }
    }

    let mut rows: Vec<gpui::AnyElement> = Vec::new();
    let mut rendered_groups: Vec<&str> = Vec::new();
    for folder in others {
        if crate::state::sidebar_folder_group(folder).is_some() {
            continue; // drawn under its parent below
        }
        let key = folder.imap_path.as_str();
        let scoped = group_key(account, key);
        let group = children.iter().find(|(existing, _)| *existing == key);
        let expanded = s.sidebar_expanded_groups.contains(&scoped);
        rows.push(
            nested_folder_row(
                s,
                state,
                folder,
                crate::state::sidebar_folder_name(folder),
                false,
                group.map(|_| (scoped.clone(), expanded)),
            )
            .into_any_element(),
        );
        if let Some((_, group)) = group {
            rendered_groups.push(key);
            if expanded {
                rows.extend(group.iter().map(|child| child_row(s, state, child)));
            }
        }
    }

    for (key, group) in &children {
        if rendered_groups.contains(key) {
            continue;
        }
        let scoped = group_key(account, key);
        let expanded = s.sidebar_expanded_groups.contains(&scoped);
        rows.push(
            orphan_group_header(state, key, &scoped, group.len(), expanded).into_any_element(),
        );
        if expanded {
            rows.extend(group.iter().map(|child| child_row(s, state, child)));
        }
    }
    rows
}

fn group_key(account: birdman_store::AccountId, key: &str) -> String {
    format!("{}:{key}", account.0)
}

fn child_row(
    s: &AppState,
    state: &Entity<AppState>,
    folder: &birdman_store::Folder,
) -> gpui::AnyElement {
    nested_folder_row(
        s,
        state,
        folder,
        crate::state::sidebar_folder_leaf(folder).to_string(),
        true,
        None,
    )
    .into_any_element()
}

/// For a group whose parent folder does not exist on the server.
fn orphan_group_header(
    state: &Entity<AppState>,
    label: &str,
    scoped_key: &str,
    count: usize,
    expanded: bool,
) -> impl IntoElement {
    let state = state.clone();
    let key = scoped_key.to_string();
    let label = label.to_string();
    div()
        // gpui ids must be unique within a frame, and a fixed one collided as
        // soon as a second orphan group existed.
        .id(gpui::SharedString::from(format!(
            "folder-group-{scoped_key}"
        )))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(theme::color(theme::TEXT_MUTED))
        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.toggle_sidebar_group(&key, cx));
        })
        .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
        .child(div().text_size(px(11.0)).child(count.to_string()))
        .child(disclosure_icon(expanded))
}

fn scope_label(s: &AppState, scope: crate::state::AccountScope) -> String {
    match scope {
        crate::state::AccountScope::All => "All accounts".to_string(),
        crate::state::AccountScope::One(id) => s
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.display_name.clone())
            .unwrap_or_default(),
        crate::state::AccountScope::Unset => s
            .accounts
            .first()
            .map(|a| a.display_name.clone())
            .unwrap_or_default(),
    }
}

const ACCOUNT_SWITCHER_HEIGHT: f32 = 24.0;

/// The list floats: `absolute` inside a `relative` wrapper so it does not push
/// the folder list down, and `deferred` so it paints after its later siblings.
/// gpui has no z-index -- paint order is the ordering.
///
/// It must stay inside the sidebar's width: anything overlapping the reading
/// pane renders *behind* the native webview.
fn account_header(
    s: &AppState,
    state: &Entity<AppState>,
    label: &str,
    multi_account: bool,
) -> impl IntoElement {
    if !multi_account {
        return div().flex_shrink_0().child(
            div()
                .text_color(theme::color(theme::TEXT_MUTED))
                .text_size(px(11.0))
                .p_1()
                .overflow_hidden()
                .child(label.to_string()),
        );
    }

    let open = s.account_picker_open;
    let toggle = state.clone();
    let options = s.account_picker_options();
    let highlighted = s.account_picker.index;

    div()
        .flex_shrink_0()
        .relative()
        .child(
            div()
                .id("account-switcher")
                .h(px(ACCOUNT_SWITCHER_HEIGHT))
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .rounded_md()
                .text_size(px(11.0))
                .text_color(theme::color(theme::TEXT_SECONDARY))
                .hover(|el| {
                    el.bg(theme::color(theme::BG_HOVER))
                        .text_color(theme::color(theme::TEXT_PRIMARY))
                })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |state, cx| state.toggle_account_picker(cx));
                })
                .child(div().flex_1().truncate().child(label.to_string()))
                .child(disclosure_icon(open)),
        )
        .when(open && !options.is_empty(), |el| {
            el.child(gpui::deferred(
                div()
                    // `deferred` changes paint order, not hit-testing: without
                    // `occlude` the popup draws on top but clicks fall through
                    // to the folder rows it covers.
                    .occlude()
                    .absolute()
                    .top(px(ACCOUNT_SWITCHER_HEIGHT + 2.0))
                    .left(px(0.0))
                    .right(px(0.0))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::color(theme::BORDER))
                    .bg(theme::color(theme::BG_APP))
                    .children(options.into_iter().enumerate().map(
                        |(ix, (scope, option_label))| {
                            let pick = state.clone();
                            let active = ix == highlighted;
                            div()
                                .id(("account-option", ix as u64))
                                .when(active, |el| el.bg(theme::color(theme::BG_SELECTED)))
                                .flex()
                                .items_center()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_size(px(12.0))
                                .text_color(theme::color(theme::TEXT_SECONDARY))
                                .hover(|el| {
                                    el.bg(theme::color(theme::BG_HOVER))
                                        .text_color(theme::color(theme::TEXT_PRIMARY))
                                })
                                .cursor_pointer()
                                .on_click(move |_, _, cx| {
                                    pick.update(cx, |state, cx| state.select_account(scope, cx));
                                })
                                .child(div().flex_1().truncate().child(option_label))
                        },
                    )),
            ))
        })
}

fn logs_overlay(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let dismiss = state.clone();
    let lines = s.log_lines.clone();
    div()
        .id("logs-overlay")
        .occlude()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme::color_alpha(theme::BG_APP, BACKDROP_OPACITY))
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |state, cx| state.set_logs(false, cx));
        })
        .child(
            div()
                .id("logs-panel")
                .occlude()
                .my_8()
                .w(px(760.0))
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme::color(theme::BORDER))
                .bg(theme::color(theme::BG_LIST))
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme::color(theme::BORDER))
                        .text_size(px(11.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .child(format!(
                                    "Log \u{2014} newest {} lines first \u{2014} R to restart, Esc to close",
                                    lines.len()
                                )),
                        )
                        .child({
                            let restart = state.clone();
                            div()
                                .id("logs-restart-daemon")
                                .flex_shrink_0()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(theme::color(theme::BG_HOVER))
                                .hover(|el| el.bg(theme::color(theme::BG_SELECTED)))
                                .cursor_pointer()
                                .child("Restart daemon (R)")
                                .on_click(move |_, _, cx| {
                                    restart.update(cx, |state, cx| state.restart_daemon(cx));
                                })
                        }),
                )
                .child(
                    div()
                        .id("logs-scroll")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .p_3()
                        .font_family("Menlo")
                        .text_size(px(11.0))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .children(lines.into_iter().map(|line| {
                            let colour = if line.contains("ERROR") {
                                theme::DANGER
                            } else if line.contains("WARN") {
                                theme::TEXT_PRIMARY
                            } else {
                                theme::TEXT_MUTED
                            };
                            div().w_full().text_color(theme::color(colour)).child(line)
                        })),
                ),
        )
}

const BACKDROP_OPACITY: f32 = 0.82;

fn palette_overlay(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let dismiss = state.clone();
    let matches: Vec<_> = s
        .palette_matches()
        .into_iter()
        .map(|c| {
            (
                c.label(s),
                crate::palette::shortcut_label(c.shortcut),
                c.group,
            )
        })
        .collect();
    let highlighted = s.palette.index;
    let query = s.palette.query.clone();

    div()
        .id("palette-overlay")
        .occlude()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme::color_alpha(theme::BG_APP, BACKDROP_OPACITY))
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |state, cx| state.set_palette(false, cx));
        })
        .child(
            div()
                .id("palette")
                .occlude()
                .mt_8()
                .w(px(420.0))
                .max_h(px(420.0))
                .flex()
                .flex_col()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::color(theme::BORDER))
                .bg(theme::color(theme::BG_LIST))
                .child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .pb_1()
                        .text_size(px(11.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child(if query.is_empty() {
                            "Type a command \u{2014} \u{2191}\u{2193} choose, \u{21e5} section, \u{23ce} run"
                                .to_string()
                        } else {
                            query.clone()
                        }),
                )
                // Outside the scrolling box, which also makes the scroll
                // handle's child indices line up with the command indices that
                // `scroll_to_item` is given.
                .child(
                    div()
                        .id("palette-list")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .track_scroll(&s.palette_scroll)
                        .flex()
                        .flex_col()
                        .gap_0p5()
                .children(matches.is_empty().then(|| {
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(13.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child("No matching command")
                }))
                .children({
                    // `COMMANDS` keeps sections contiguous (a test pins that),
                    // so this cannot print the same heading twice.
                    let mut previous: Option<crate::palette::Group> = None;
                    let subject = s.selected_message.map(|_| s.selected_subject());
                    matches
                        .into_iter()
                        .enumerate()
                        .map(move |(ix, (name, shortcut, group))| {
                            let new_group = previous != Some(group);
                            let heading = previous
                                .map(|p| p.section() != group.section())
                                .unwrap_or(true)
                                .then(|| group.section().title())
                                .flatten();
                            previous = Some(group);
                            let pick = state.clone();
                            let active = ix == highlighted;
                            div()
                                .flex()
                                .flex_col()
                                .children(
                                    (new_group && ix > 0 && heading.is_none()).then(|| {
                                        div().h(px(1.0)).my_1().mx_2().bg(theme::color(theme::BORDER))
                                    }),
                                )
                                .when(heading.is_some() && ix > 0, |el| el.mt_3())
                                .children(heading.map(|title| {
                                    div()
                                        .px_2()
                                        .pt_1()
                                        .pb_1()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(px(10.0))
                                        .text_color(theme::color(theme::TEXT_MUTED))
                                        .child(title)
                                        // `flex_1` + `min_w(0)` + truncate is
                                        // what ellipsises a long subject rather
                                        // than pushing the heading off the row.
                                        .when_some(subject.clone(), |el, subject| {
                                            el.child(
                                                div()
                                                    .w(px(1.0))
                                                    .h(px(9.0))
                                                    .flex_shrink_0()
                                                    .bg(theme::color(theme::BORDER)),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .truncate()
                                                    .child(subject),
                                            )
                                        })
                                }))
                                .child(
                                    div()
                                        .id(("palette-item", ix as u64))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .text_size(px(13.0))
                                        .when(active, |el| el.bg(theme::color(theme::BG_SELECTED)))
                                        .text_color(theme::color(if active {
                                            theme::TEXT_PRIMARY
                                        } else {
                                            theme::TEXT_SECONDARY
                                        }))
                                        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
                                        .cursor_pointer()
                                        .on_click(move |_, window, cx| {
                                            pick.update(cx, |state, cx| {
                                                state.palette.index = ix;
                                                let chosen =
                                                    state.palette_matches().get(ix).copied();
                                                state.set_palette(false, cx);
                                                if let Some(command) = chosen {
                                                    (command.run)(state, window, cx);
                                                }
                                            });
                                        })
                                        .child(div().flex_1().min_w(px(0.0)).truncate().child(name))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(px(11.0))
                                                .text_color(theme::color(theme::TEXT_MUTED))
                                                .child(shortcut.into_owned()),
                                        ),
                                )
                        })
                        .collect::<Vec<_>>()
                        }),
                ),
        )
}

/// A pane-filling overlay rather than a dropdown, because a dropdown would hang
/// over the reading pane and render behind the native webview. The webview is
/// hidden while this is open (see `Root::render`).
fn move_picker_overlay(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let dismiss = state.clone();
    let targets: Vec<_> = s
        .filtered_move_targets()
        .into_iter()
        .map(|f| {
            (
                f.id,
                crate::state::sidebar_folder_name(f),
                crate::state::sidebar_folder_icon(f),
            )
        })
        .collect();
    let highlighted = s.move_picker.index;
    let query = s.move_picker.query.clone();

    div()
        .id("move-picker-overlay")
        .occlude()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .bg(theme::color_alpha(theme::BG_APP, BACKDROP_OPACITY))
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |state, cx| state.set_move_picker(false, cx));
        })
        .child(
            div()
                .id("move-picker")
                .occlude()
                .mt_8()
                .w(px(360.0))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_0p5()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::color(theme::BORDER))
                .bg(theme::color(theme::BG_LIST))
                .child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .pb_1()
                        .text_size(px(11.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child(if query.is_empty() {
                            "Move to — type to filter, ↑↓ to choose, ⏎ to move".to_string()
                        } else {
                            format!("Move to  {query}")
                        }),
                )
                .children(targets.is_empty().then(|| {
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(13.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child("No matching folder")
                }))
                .children(
                    targets
                        .into_iter()
                        .enumerate()
                        .map(|(ix, (id, label, icon_path))| {
                            let pick = state.clone();
                            let active = ix == highlighted;
                            div()
                                .id(("move-target", id.0 as u64))
                                .when(active, |el| el.bg(theme::color(theme::BG_SELECTED)))
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_size(px(13.0))
                                .text_color(theme::color(if active {
                                    theme::TEXT_PRIMARY
                                } else {
                                    theme::TEXT_SECONDARY
                                }))
                                .hover(|el| {
                                    el.bg(theme::color(theme::BG_HOVER))
                                        .text_color(theme::color(theme::TEXT_PRIMARY))
                                })
                                .cursor_pointer()
                                .on_click(move |_, _, cx| {
                                    pick.update(cx, |state, cx| {
                                        state.move_selected_to_folder(id, cx)
                                    });
                                })
                                .child(
                                    sized_icon(icon_path, INLINE_ICON_SIZE)
                                        .text_color(theme::color(theme::TEXT_MUTED)),
                                )
                                .child(div().flex_1().min_w(px(0.0)).truncate().child(label))
                        }),
                ),
        )
}

fn more_folders_header(s: &AppState, state: &Entity<AppState>, key: u64) -> impl IntoElement {
    let expanded = s.sidebar_more_expanded;
    let state = state.clone();
    div()
        .id(("more-folders", key))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .mt_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .text_color(theme::color(theme::TEXT_MUTED))
        .hover(|el| {
            el.bg(theme::color(theme::BG_HOVER))
                .text_color(theme::color(theme::TEXT_SECONDARY))
        })
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.toggle_sidebar_more(cx));
        })
        .child(
            div()
                .flex_1()
                .truncate()
                .child(if expanded { "Show less" } else { "Show more" }),
        )
        .child(disclosure_icon(expanded))
}

fn sync_button(state: &Entity<AppState>) -> impl IntoElement {
    let state = state.clone();
    div()
        .id("sync-now")
        .group(ICON_BUTTON_GROUP)
        .size(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        .child(icon("icons/refresh.svg"))
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.sync_now(cx));
        })
}

/// Rendered in the sidebar while shown and in the message list's header once
/// hidden, since a button that hides its own only route would have no way back.
fn sidebar_toggle_button(state: &Entity<AppState>, currently_visible: bool) -> impl IntoElement {
    let state = state.clone();
    div()
        .id("toggle-sidebar")
        .group(ICON_BUTTON_GROUP)
        .size(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        .child(icon(if currently_visible {
            "icons/sidebar-hide.svg"
        } else {
            "icons/sidebar-show.svg"
        }))
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.toggle_sidebar(cx));
        })
}

fn new_message_button(state: &Entity<AppState>) -> impl IntoElement {
    let state = state.clone();
    div()
        .id("compose-new")
        .size(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(theme::color(theme::BG_HOVER))
        .hover(|el| el.bg(theme::color(theme::BG_SELECTED)))
        .cursor_pointer()
        .group(ICON_BUTTON_GROUP)
        .child(icon("icons/compose.svg"))
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.compose_new(cx));
        })
}

fn settings_button(state: &Entity<AppState>) -> impl IntoElement {
    let state = state.clone();
    div()
        .id("open-settings")
        .group(ICON_BUTTON_GROUP)
        .size(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        .child(icon("icons/settings.svg"))
        .on_click(move |_, _, cx| {
            let path = crate::config::config_path();
            let open_task = cx.background_spawn(async move { crate::config::open_editor(&path) });
            let state = state.clone();
            cx.spawn(async move |cx| {
                if let Err(err) = open_task.await {
                    // The window may be gone by the time the editor answers.
                    let _ = state.update(cx, |state, cx| {
                        state.status = Some(format!("couldn't open editor: {err}"));
                        cx.notify();
                    });
                }
            })
            .detach();
        })
}

fn search_box(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let show_placeholder = s.search_query.is_empty() && !s.search_active;
    let handle_focus = state.clone();

    let box_el = div()
        .id("search-box")
        .track_focus(&s.search_focus_handle)
        .on_key_down(cx_listener_search(state))
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme::color(theme::BG_LIST))
        .border_1()
        .when(s.search_active, |el| {
            el.border_color(theme::color(theme::ACCENT))
        })
        .when(!s.search_active, |el| {
            el.border_color(theme::color(theme::BORDER))
        })
        .text_color(theme::color(theme::TEXT_PRIMARY))
        .cursor_text()
        .on_click(move |_, window, cx| {
            handle_focus.update(cx, |state, cx| {
                if !state.search_active {
                    state.search_cursor = state.search_query.len();
                }
                state.search_active = true;
                state.search_focus_handle.focus(window);
                cx.notify();
            });
        });

    // compose.rs's line-renderer works unmodified for single-line content.
    if show_placeholder {
        box_el.child(
            div()
                .text_color(theme::color(theme::TEXT_MUTED))
                .child("Search..."),
        )
    } else {
        box_el.child(crate::compose::render_field_content(
            &s.search_query,
            s.search_cursor,
            s.search_anchor,
            s.search_active,
        ))
    }
}

/// `on_key_down` needs a plain `Fn`, so this cannot be `cx.listener`; it routes
/// straight to the `AppState` entity instead.
fn cx_listener_search(
    state: &Entity<AppState>,
) -> impl Fn(&gpui::KeyDownEvent, &mut Window, &mut gpui::App) + 'static {
    let state = state.clone();
    move |event, window, cx| {
        state.update(cx, |state, cx| {
            if state.search_active {
                state.search_key_down(event, window, cx);
            }
        });
    }
}

/// Every slot must be single-line and ellipsised: `uniform_list` measures the
/// *first* row and lays the rest out from that one measurement, so a slot that
/// could wrap breaks the layout for every row, not just its own.
///
/// `None`, not an empty element, when a slot has nothing to say -- the line is
/// a `gap`-ed flex row, so a zero-width child still pushes its neighbours apart.
fn message_slot(
    slot: MessageSlot,
    msg: &birdman_store::MessageSummary,
    row: &crate::config::MessageRow,
    unseen: bool,
) -> Option<gpui::AnyElement> {
    let style = row.style(slot);
    let color = theme::color(style.color_for(unseen));

    // `min_w(0)` is what lets a growing slot shrink to the column rather than
    // setting the column's width.
    let text = |value: String| {
        Some(
            div()
                .when(slot.grows(), |el| el.flex_1().min_w(px(0.0)).truncate())
                .when(!slot.grows(), |el| el.flex_shrink_0())
                .text_size(px(style.size))
                .font_weight(style.weight)
                .text_color(color)
                .child(value)
                .into_any_element(),
        )
    };
    let inline_icon = |path: &'static str| {
        Some(
            div()
                .flex_shrink_0()
                .child(sized_icon(path, style.size).text_color(color))
                .into_any_element(),
        )
    };

    match slot {
        MessageSlot::UnreadDot => Some(
            div()
                .flex_shrink_0()
                .w(px(style.size))
                .h(px(style.size))
                .rounded_full()
                .when(unseen, |el| el.bg(color))
                .into_any_element(),
        ),
        MessageSlot::Sender => text(
            msg.from_name
                .clone()
                .unwrap_or_else(|| msg.from_addr.clone().unwrap_or_default()),
        ),
        MessageSlot::Recipients => text(msg.to_addrs.clone().unwrap_or_default()),
        MessageSlot::Subject => text(
            msg.subject
                .clone()
                .unwrap_or_else(|| "(no subject)".to_string()),
        ),
        // Blank rather than absent: the row height is uniform, so a message
        // with no preview still has to leave the line where it is.
        MessageSlot::Preview => text(msg.preview.clone().unwrap_or_default()),
        MessageSlot::Date => text(msg.date.map(relative_timestamp).unwrap_or_default()),
        MessageSlot::Flag => msg
            .flags
            .flagged
            .then(|| inline_icon("icons/flag.svg"))
            .flatten(),
        MessageSlot::Attachment => msg
            .has_attachments
            .then(|| inline_icon("icons/paperclip.svg"))
            .flatten(),
        MessageSlot::Spacer => Some(div().ml_auto().into_any_element()),
    }
}

/// Paired with `message_list_scrollbar`: gpui ships no scrollbar widget, so
/// `uniform_list`'s scrolling has no visible indicator otherwise.
fn message_list(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let visible = s.visible_messages().to_vec();
    let count = visible.len();
    let selected = s.selected_message;
    let state_for_rows = state.clone();
    let scroll_handle = s.list_scroll_handle.clone();
    let dragging = s.list_scrollbar_dragging.clone();

    // Derived by hand: `uniform_list` populates neither `max_offset()` nor
    // `bounds()`, because skipping the content-size machinery is exactly how it
    // avoids laying out off-screen rows. The viewport comes from the `canvas()`.
    let viewport_height_cell = s.list_viewport_height.clone();
    let measured_height = viewport_height_cell.clone();
    let base_handle = scroll_handle.0.borrow().base_handle.clone();
    let viewport_height = viewport_height_cell.get();
    let load_more_handle = base_handle.clone();
    let load_more_state = state.clone();
    // Every piece of geometry below multiplies by this, so it must be the same
    // number the rows are actually drawn at.
    let row = s.appearance.message_row.clone();
    let row_height = row.height();
    let content_height = count as f32 * row_height;
    let overflow_y = (content_height - viewport_height).max(0.0);
    let max_offset_y = -overflow_y;
    let show_scrollbar =
        s.appearance.show.scrollbars && count > 0 && overflow_y > 0.0 && viewport_height > 0.0;

    let current_offset_y = f32::from(base_handle.offset().y);
    let scroll_fraction = if max_offset_y < 0.0 {
        (current_offset_y / max_offset_y).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_height = if content_height > 0.0 {
        (viewport_height * viewport_height / content_height).max(SCROLLBAR_MIN_THUMB_HEIGHT)
    } else {
        SCROLLBAR_MIN_THUMB_HEIGHT
    };
    let max_thumb_top = (viewport_height - thumb_height).max(0.0);
    let thumb_top = scroll_fraction * max_thumb_top;

    // (mouse Y at drag start, scroll offset.y at drag start).
    let drag_start = s.list_scrollbar_drag_start.clone();

    let down_handle = base_handle.clone();
    let down_dragging = dragging;
    let down_drag_start = drag_start.clone();
    let on_down = move |event: &MouseDownEvent, _window: &mut Window, _cx: &mut App| {
        down_drag_start.set((
            f32::from(event.position.y),
            f32::from(down_handle.offset().y),
        ));
        down_dragging.set(true);
    };

    div()
        .id("message-list")
        .flex()
        .flex_col()
        .w(px(320.0))
        .h_full()
        .flex_shrink_0()
        .bg(theme::color(theme::BG_LIST))
        .border_r_1()
        .border_color(theme::color(theme::BORDER))
        .when(s.appearance.show.message_list_header, |el| {
            el.child(message_list_header(s, state, &visible))
        })
        .child(
            // A plain `.relative()` wrapper, *not* `UniformListDecoration`:
            // that hook positions fine but never delivers mouse-down/move/up to
            // what it renders, so click-to-jump and drag never fired.
            //
            // move/up live on this WIDE wrapper, not the 10px track -- gpui
            // fires them only while the cursor is over the handler's own
            // element, so a diagonal drag stalls. mouse-down stays on the track,
            // so a drag cannot be started by clicking a message row.
            div()
                .relative()
                .flex_1()
                // The same `min-height: auto` trap as the sidebar's list.
                .min_h(px(0.0))
                .child(
                    uniform_list(
                        "message-rows",
                        count,
                        move |range: std::ops::Range<usize>, _window, _cx| {
                            range
                                .map(|ix| {
                                    let msg = visible[ix].clone();
                                    {
                                        let id = msg.id;
                                        let is_selected = selected == Some(id);
                                        let unseen = !msg.flags.seen;
                                        let state = state_for_rows.clone();
                                        div()
                                            .id(("message", id.0 as u64))
                                            .w_full()
                                            .h(px(row_height))
                                            .overflow_hidden()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .py_2()
                                            .border_b_1()
                                            .border_color(theme::color(theme::BORDER))
                                            .when(is_selected, |el| {
                                                el.bg(theme::color(theme::BG_SELECTED))
                                            })
                                            .when(unseen && !is_selected, |el| {
                                                el.bg(theme::color(theme::BG_UNREAD))
                                            })
                                            .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
                                            .cursor_pointer()
                                            .on_click(move |_, window, cx| {
                                                state.update(cx, |state, cx| {
                                                    state.select_message(id, cx);
                                                    state.focus_main(window, cx);
                                                });
                                            })
                                            .children(row.gutter.iter().filter_map(|slot| {
                                                message_slot(*slot, &msg, &row, unseen)
                                            }))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .children(row.lines.iter().map(|line| {
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .w_full()
                                                            .min_w(px(0.0))
                                                            .gap_2()
                                                            .children(line.iter().filter_map(
                                                                |slot| {
                                                                    message_slot(
                                                                        *slot, &msg, &row, unseen,
                                                                    )
                                                                },
                                                            ))
                                                    })),
                                            )
                                    }
                                })
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(scroll_handle.clone())
                    .size_full(),
                )
                .child(
                    // Measurement probe, and where infinite scroll is driven
                    // from: the one place both the viewport height and the
                    // current offset are known.
                    canvas(
                        move |bounds, _window, cx| {
                            let viewport = f32::from(bounds.size.height);
                            measured_height.set(viewport);

                            // `load_more_messages` no-ops when a load is in
                            // flight or the folder is exhausted, so calling it
                            // every frame is safe.
                            let scrolled = -f32::from(load_more_handle.offset().y);
                            let remaining = content_height - (scrolled + viewport);
                            if viewport > 0.0 && remaining <= viewport {
                                load_more_state.update(cx, |state, cx| {
                                    if state.has_more_messages() {
                                        state.load_more_messages(cx);
                                    }
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full(),
                )
                .when(show_scrollbar, |el| {
                    el.child(scrollbar(
                        "message-list-scrollbar",
                        thumb_top,
                        thumb_height,
                        on_down,
                    ))
                }),
        )
}

fn message_list_header(
    s: &AppState,
    state: &Entity<AppState>,
    visible: &[birdman_store::MessageSummary],
) -> impl IntoElement {
    let searching = s.search_active || s.search_results.is_some();
    let folder_label = if searching {
        "Search Results".to_string()
    } else {
        s.selected_folder
            .and_then(|id| s.folders.iter().find(|f| f.id == id))
            .map(crate::state::sidebar_folder_name)
            .unwrap_or_default()
    };
    // From the store for a folder, from the results themselves for a search.
    // Keyed on whether there are *results*, not on whether the box is open: an
    // open empty box still shows the folder, and reporting "0 messages" the
    // instant it opens says the mailbox emptied.
    let (count, unread) = match (s.search_results.is_some(), s.selected_folder_counts) {
        (false, Some((total, unread))) => (total as usize, unread as usize),
        _ => (
            visible.len(),
            visible.iter().filter(|m| !m.flags.seen).count(),
        ),
    };

    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_1()
        .p_2()
        .border_b_1()
        .border_color(theme::color(theme::BORDER))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .mb_1()
                .when(!s.sidebar_visible, |el| {
                    el.child(sidebar_toggle_button(state, false))
                })
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_size(px(14.0))
                        .child(folder_label),
                )
                .child(attachment_filter_button(s, state))
                .child(new_message_button(state)),
        )
        .child({
            let toggle = state.clone();
            let filtered = s.filter.unread;
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(theme::color(theme::TEXT_MUTED))
                .text_size(px(11.0))
                .child(format!(
                    "{count} message{}",
                    if count == 1 { "" } else { "s" }
                ))
                .child(
                    div()
                        .id("unread-filter")
                        .ml_auto()
                        .when(filtered, |el| {
                            el.px_1p5()
                                .rounded_full()
                                .bg(theme::color(theme::ACCENT))
                                .text_color(theme::color(theme::BG_APP))
                        })
                        .cursor_pointer()
                        .when(filtered, |el| {
                            el.hover(|el| el.bg(theme::color(theme::SCROLLBAR_THUMB_HOVER)))
                        })
                        .when(!filtered, |el| {
                            el.hover(|el| el.text_color(theme::color(theme::TEXT_PRIMARY)))
                        })
                        .on_click(move |_, window, cx| {
                            toggle.update(cx, |state, cx| {
                                state.toggle_unread_only(cx);
                                state.keep_search_focus(window, cx);
                            });
                        })
                        .child(format!("{unread} unread")),
                )
        })
        .when(s.search_expanded, |el| el.child(search_box(s, state)))
}

/// The confirmation is laid *over* the address, which stays in the layout with
/// its colour turned off -- that is what holds the width, so the header does
/// not twitch as the message arrives and leaves.
///
/// The overlay uses explicit edges, **not** `size_full`: a percentage resolves
/// against an indefinite parent (the bubble is sized by its own text) and
/// collapses to nothing.
fn address_chip(
    s: &AppState,
    state: &Entity<AppState>,
    who: &birdman_mime::Mailbox,
) -> impl IntoElement {
    let act = state.clone();
    let address = who.address.clone();
    let name = who.name.clone();
    let copied = s.copied_address.as_deref() == Some(address.as_str());
    let hidden = theme::color_alpha(theme::TEXT_PRIMARY, 0.0);

    div()
        .flex()
        .items_center()
        .gap_1p5()
        .min_w(px(0.0))
        .when_some(name.clone(), |el, name| {
            el.child(div().flex_shrink_0().truncate().child(name))
        })
        .child(
            div()
                .id(gpui::SharedString::from(format!("chip-{address}")))
                .flex_shrink_0()
                .max_w(px(280.0))
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(theme::color(if copied {
                    theme::ACCENT
                } else {
                    theme::BG_HOVER
                }))
                .text_size(px(11.0))
                .text_color(theme::color(theme::TEXT_MUTED))
                .cursor_pointer()
                .hover(|el| {
                    el.bg(theme::color(if copied {
                        theme::ACCENT
                    } else {
                        theme::BG_SELECTED
                    }))
                })
                .child(
                    div()
                        .relative()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .truncate()
                                .when(copied, |el| el.text_color(hidden))
                                .child(address.clone()),
                        )
                        .when(copied, |el| {
                            el.child(
                                div()
                                    .absolute()
                                    .top(px(0.0))
                                    .left(px(0.0))
                                    .right(px(0.0))
                                    .bottom(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme::color(theme::BG_APP))
                                    .child("Copied address"),
                            )
                        }),
                )
                .on_click(move |event, _, cx| {
                    let double = event.click_count() >= 2;
                    let address = address.clone();
                    let name = name.clone();
                    act.update(cx, |state, cx| {
                        if double {
                            state.compose_to(address, name, cx);
                        } else {
                            state.copy_address(address, cx);
                        }
                    });
                }),
        )
}

/// One line, expandable for the thread with five people on it.
fn address_block(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let toggle = state.clone();
    let rows = s.address_rows();
    let sender = rows
        .iter()
        .find(|(label, _)| *label == "From")
        .and_then(|(_, addresses)| addresses.first().cloned());
    let expanded = s.header_expanded;
    // Nothing to expand into when `From` is the only header there is.
    let expandable = rows
        .iter()
        .any(|(label, addresses)| *label != "From" && !addresses.is_empty());

    div()
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w(px(0.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .w_full()
                .min_w(px(0.0))
                .text_color(theme::color(theme::TEXT_SECONDARY))
                .when_some(sender.clone(), |el, who| {
                    el.child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(address_chip(s, state, &who)),
                    )
                })
                .when(expandable, |el| {
                    el.child(
                        div()
                            .id("header-expand")
                            .ml_auto()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(18.0))
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
                            .child(
                                sized_icon(
                                    if expanded {
                                        "icons/chevron-down.svg"
                                    } else {
                                        "icons/chevron-right.svg"
                                    },
                                    12.0,
                                )
                                .text_color(theme::color(theme::TEXT_MUTED)),
                            )
                            .on_click(move |_, _, cx| {
                                toggle.update(cx, |state, cx| state.toggle_header_expanded(cx));
                            }),
                    )
                }),
        )
        .when(expanded, |el| {
            // `From` is already the chip on the line above.
            el.children(rows.iter().filter(|(label, _)| *label != "From").map(
                |(label, addresses)| {
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .w_full()
                        .min_w(px(0.0))
                        .child(
                            // Fixed, so the chips line up down the block.
                            div()
                                .w(px(56.0))
                                .flex_shrink_0()
                                .pt_0p5()
                                .text_size(px(10.0))
                                .text_color(theme::color(theme::TEXT_MUTED))
                                .child(*label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .children(addresses.iter().map(|who| address_chip(s, state, who))),
                        )
                },
            ))
        })
}

fn subject_menu(state: &Entity<AppState>, at: gpui::Point<gpui::Pixels>) -> impl IntoElement {
    let copy = state.clone();
    let dismiss = state.clone();
    gpui::deferred(
        div()
            .id("subject-menu")
            .occlude()
            .absolute()
            .left(at.x)
            .top(at.y)
            .min_w(px(120.0))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::color(theme::BORDER))
            .bg(theme::color(theme::BG_LIST))
            .text_size(px(12.0))
            .text_color(theme::color(theme::TEXT_PRIMARY))
            .cursor_pointer()
            .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
            .child("Copy")
            .on_click(move |_, _, cx| {
                copy.update(cx, |state, cx| {
                    state.copy_header_selection(cx);
                });
            })
            .on_mouse_down_out(move |_, _, cx| {
                dismiss.update(cx, |state, cx| state.close_subject_menu(cx));
            }),
    )
}

/// Newest at the bottom, so a message does not jump as the one above expires.
fn notifications(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    gpui::deferred(
        div()
            .absolute()
            .bottom(px(10.0))
            .right(px(10.0))
            .flex()
            .flex_col()
            .gap_1()
            .items_end()
            .children(s.notifications.iter().map(|notification| {
                let dismiss = state.clone();
                let id = notification.id;
                div()
                    .id(gpui::ElementId::Integer(id))
                    .occlude()
                    .flex()
                    .items_center()
                    .gap_2()
                    .max_w(px(360.0))
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::color(if notification.failed {
                        theme::DANGER
                    } else {
                        theme::BORDER
                    }))
                    .bg(theme::color(theme::BG_LIST))
                    .text_size(px(12.0))
                    .text_color(theme::color(if notification.failed {
                        theme::DANGER
                    } else {
                        theme::TEXT_PRIMARY
                    }))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme::color(theme::BG_HOVER)))
                    .child(notification.text.clone())
                    .on_click(move |_, _, cx| {
                        dismiss.update(cx, |state, cx| state.dismiss_notification(id, cx));
                    })
            })),
    )
}

/// Clicking hands the file to the OS default handler, which means running
/// whatever is associated with its extension. That is why the materialised copy
/// carries `com.apple.quarantine`.
fn attachment_pill(attachment: &birdman_store::Attachment) -> gpui::AnyElement {
    let base = div()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(theme::color(theme::BG_HOVER))
        .text_size(px(11.0))
        .child(sized_icon("icons/paperclip.svg", 11.0).text_color(theme::color(theme::TEXT_MUTED)))
        .child(attachment.filename.clone());

    let Some(path) = attachment.path.clone().map(std::path::PathBuf::from) else {
        return base
            .text_color(theme::color(theme::TEXT_MUTED))
            .child(spinner())
            .into_any_element();
    };

    base.id(gpui::SharedString::from(
        path.to_string_lossy().into_owned(),
    ))
    .text_color(theme::color(theme::TEXT_SECONDARY))
    .child(
        div()
            .text_color(theme::color(theme::TEXT_MUTED))
            .child(human_size(attachment.size)),
    )
    .cursor_pointer()
    .hover(|el| el.bg(theme::color(theme::BG_SELECTED)))
    .on_click({
        let path = path.clone();
        move |_, _, _| {
            if let Err(err) = open::that_detached(&path) {
                log::warn!("could not open {}: {err}", path.display());
            }
        }
    })
    // No drag-out. It needs `external_drag_payload`, which exists only on
    // Zed's `main` and not in the gpui release this depends on -- see the
    // note on the dependency in Cargo.toml. Clicking opens the file in the
    // system handler, and `birdman attachments <id> --save DIR` writes it
    // somewhere a file manager can reach.
    .into_any_element()
}

/// A pulse rather than a turning ring: gpui's `Div` has no rotation.
fn spinner() -> impl IntoElement {
    use gpui::AnimationExt as _;
    div().size(px(6.0)).rounded_full().with_animation(
        "attachment-spinner",
        gpui::Animation::new(std::time::Duration::from_millis(1_100)).repeat(),
        |el, delta| {
            let alpha = 0.25 + 0.75 * (delta * std::f32::consts::TAU).sin().abs();
            el.bg(theme::color_alpha(theme::ACCENT, alpha))
        },
    )
}

fn human_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes.max(0) as f64;
    if bytes < KB {
        format!("{bytes:.0} B")
    } else if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / (KB * KB))
    }
}

fn attachment_filter_button(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let state = state.clone();
    let on = s.filter.attachments;
    div()
        .id("attachment-filter")
        .group(ICON_BUTTON_GROUP)
        .size(px(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .when(on, |el| el.bg(theme::color(theme::ACCENT)))
        .when(!on, |el| {
            el.hover(|el| el.bg(theme::color(theme::BG_HOVER)))
        })
        // `sized_icon`, not `icon`: the latter's group-hover recolour fights
        // the filled state.
        .child(
            sized_icon("icons/paperclip.svg", ICON_SIZE).text_color(theme::color(if on {
                theme::BG_APP
            } else {
                theme::TEXT_MUTED
            })),
        )
        .on_click(move |_, window, cx| {
            state.update(cx, |state, cx| {
                state.toggle_attachments_only(cx);
                state.keep_search_focus(window, cx);
            });
        })
}

/// `appears_transparent` extends the macOS content view up under the titlebar,
/// so the traffic lights overlap the content and the system draws no title.
/// Change this and `traffic_light_position` needs rechecking.
pub const TITLEBAR_HEIGHT: f32 = 32.0;

/// Clears the macOS traffic lights, which run to roughly 70px.
#[cfg(target_os = "macos")]
const TITLEBAR_LEADING_INSET: f32 = 84.0;
#[cfg(not(target_os = "macos"))]
const TITLEBAR_LEADING_INSET: f32 = 12.0;

/// macOS only: `appears_transparent` suppresses the system's title text, and
/// the strip has to exist anyway to hold the traffic lights. It is also the
/// window's drag region as far as the platform is concerned.
///
/// **Nothing is drawn elsewhere** -- none of that applies to a Linux tiler. The
/// window's own title (`TitlebarOptions::title`, set in `main`) is separate and
/// unaffected.
fn titlebar() -> Option<impl IntoElement> {
    // `cfg!` rather than `#[cfg]`: one body, folded away just the same.
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(
        div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .pl(px(TITLEBAR_LEADING_INSET))
            .text_size(px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::color(theme::TEXT_SECONDARY))
            .child("Birdman"),
    )
}

const ICON_BUTTON_GROUP: &str = "icon-button";

fn icon(path: &'static str) -> impl IntoElement {
    sized_icon(path, ICON_SIZE).group_hover(ICON_BUTTON_GROUP, |el| {
        el.text_color(theme::color(theme::TEXT_PRIMARY))
    })
}

/// The colour must be set **on the svg element**, never inherited: gpui skips
/// painting an svg whose own computed `text.color` is `None`, so an icon
/// relying on the surrounding text colour draws nothing at all.
fn sized_icon(path: &'static str, size: f32) -> gpui::Svg {
    gpui::svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(theme::color(theme::TEXT_MUTED))
}

/// Explicit rather than `py_1` + line height: an icon button has no text to
/// give it a height, so the two kinds drift apart without a common number.
pub const CONTROL_HEIGHT: f32 = 26.0;

const HEADER_TIMESTAMP_TEXT_SIZE: f32 = 11.0;

const ICON_BUTTON_INSET: f32 = 5.0;

const ICON_SIZE: f32 = CONTROL_HEIGHT - 2.0 * ICON_BUTTON_INSET;

const INLINE_ICON_SIZE: f32 = 13.0;

const SCROLLBAR_WIDTH: f32 = 12.0;
/// Inset within `SCROLLBAR_WIDTH`'s hit area, so the track stays wider than the
/// thumb and there is somewhere forgiving to grab.
const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
pub(crate) const SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 24.0;

/// The affordance without the drag machinery, for panes that scroll by wheel.
pub(crate) fn scrollbar_thumb(thumb_top: f32, thumb_height: f32) -> impl IntoElement {
    div()
        .absolute()
        .top(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .w(px(SCROLLBAR_WIDTH))
        .child(
            div()
                .absolute()
                .top(px(thumb_top))
                .right(px(3.0))
                .w(px(SCROLLBAR_THUMB_WIDTH))
                .h(px(thumb_height))
                .rounded_full()
                .bg(theme::color(theme::SCROLLBAR_THUMB)),
        )
}

/// Presentational only: `thumb_top`/`thumb_height` are computed by the caller,
/// and `on_down` is the only handler it owns (see `message_list` for why
/// move/up live on a wider wrapper). Shared by both lists, hence the
/// caller-supplied `id` -- two elements with the same id in one frame collide.
fn scrollbar(
    id: &'static str,
    thumb_top: f32,
    thumb_height: f32,
    on_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .top(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .w(px(SCROLLBAR_WIDTH))
        .on_mouse_down(MouseButton::Left, on_down)
        .child(
            div()
                .absolute()
                .top(px(thumb_top))
                .right(px(3.0))
                .w(px(SCROLLBAR_THUMB_WIDTH))
                .h(px(thumb_height))
                .rounded_full()
                .bg(theme::color(theme::SCROLLBAR_THUMB))
                .hover(|el| el.bg(theme::color(theme::SCROLLBAR_THUMB_HOVER))),
        )
}

fn reading_pane(s: &AppState, state: &Entity<AppState>) -> impl IntoElement {
    let toolbar_button =
        |icon_path: &'static str,
         id: &'static str,
         action: fn(&mut AppState, &mut Context<AppState>)| {
            let state = state.clone();
            div()
                .id(id)
                .group(ICON_BUTTON_GROUP)
                .size(px(CONTROL_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(theme::color(theme::BG_HOVER))
                .hover(|el| el.bg(theme::color(theme::BG_SELECTED)))
                .cursor_pointer()
                .child(icon(icon_path))
                .on_click(move |_, _, cx| {
                    state.update(cx, action);
                })
        };
    let Some(message_id) = s.selected_message else {
        return div()
            .id("reading-pane")
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(theme::color(theme::TEXT_MUTED))
                    .child("Select a message"),
            );
    };
    let msg = s.visible_messages().iter().find(|m| m.id == message_id);

    let render_toolbar_action = |action: &ToolbarAction| -> gpui::AnyElement {
        match action {
            // Split out into groups before rendering -- see below.
            ToolbarAction::Spacer => div().into_any_element(),
            ToolbarAction::Reply => {
                toolbar_button("icons/reply.svg", "compose-reply", |state, cx| {
                    state.reply(false, cx)
                })
                .into_any_element()
            }
            ToolbarAction::ReplyAll => {
                toolbar_button("icons/reply-all.svg", "compose-reply-all", |state, cx| {
                    state.reply(true, cx)
                })
                .into_any_element()
            }
            ToolbarAction::Forward => {
                toolbar_button("icons/forward.svg", "compose-forward", |state, cx| {
                    state.forward(cx)
                })
                .into_any_element()
            }
            ToolbarAction::Move => {
                toolbar_button("icons/folder.svg", "message-move", |state, cx| {
                    let open = !state.move_picker_open;
                    state.set_move_picker(open, cx)
                })
                .into_any_element()
            }
            ToolbarAction::Flag => toolbar_button("icons/flag.svg", "message-flag", |state, cx| {
                state.toggle_flag_selected(cx)
            })
            .into_any_element(),
            // The icon shows what the click does, not what is currently on.
            ToolbarAction::DarkMode => {
                let (icon, id) = if s.selected_is_darkened() {
                    ("icons/sun.svg", "message-undarken")
                } else {
                    ("icons/moon.svg", "message-darken")
                };
                toolbar_button(icon, id, |state, cx| state.toggle_dark_mode(cx)).into_any_element()
            }
            ToolbarAction::Divider => div()
                .w(px(1.0))
                .h(px(CONTROL_HEIGHT - 8.0))
                .mx_1()
                .flex_shrink_0()
                .bg(theme::color(theme::BORDER))
                .into_any_element(),
            ToolbarAction::Archive => {
                toolbar_button("icons/archive.svg", "message-archive", |state, cx| {
                    state.archive_selected(cx)
                })
                .into_any_element()
            }
            ToolbarAction::Delete => {
                toolbar_button("icons/trash.svg", "message-delete", |state, cx| {
                    state.delete_selected(cx)
                })
                .into_any_element()
            }
        }
    };

    // Grouped at each `Spacer` and joined with `justify_between` rather than
    // a `margin-left: auto` spacer div: taffy only fully hands a flex row's
    // free space to an auto margin when it is the row's *sole* auto margin,
    // and split across groups like this one it visibly under-pushed the
    // trailing icons short of the row's right edge.
    let toolbar_groups = s
        .appearance
        .toolbar_actions
        .split(|a| matches!(a, ToolbarAction::Spacer))
        .map(|group| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(group.iter().map(&render_toolbar_action))
                .into_any_element()
        });

    let toolbar = div()
        .flex()
        .flex_shrink_0()
        .w_full()
        .items_center()
        .justify_between()
        .p_2()
        .border_b_1()
        .border_color(theme::color(theme::BORDER))
        // Contents and order come from `config::ToolbarAction`.
        .children(toolbar_groups);

    let mut header = div()
        .id("reading-pane-header")
        .flex()
        .flex_shrink_0()
        .flex_col()
        .p_4()
        .gap_2();
    if let Some(msg) = msg {
        header = header
            .child(
                div()
                    .flex()
                    .w_full()
                    // Top-aligned: the subject can wrap, and the date should
                    // stay level with its first line.
                    .items_start()
                    .gap_4()
                    .child(
                        // `flex_1` + `min_w(0)` is what lets a long subject
                        // wrap: with no bounded width the text lays out on one
                        // unbounded line and runs off the pane.
                        {
                            let menu = state.clone();
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(18.0))
                                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                                    let at = event.position;
                                    menu.update(cx, |state, cx| state.open_subject_menu(at, cx));
                                })
                                .child(crate::selectable::selectable_text(
                                    "reading-pane-subject",
                                    s.selected_subject(),
                                    &s.subject_selection,
                                ))
                        },
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(HEADER_TIMESTAMP_TEXT_SIZE))
                            .text_color(theme::color(theme::TEXT_MUTED))
                            .child(msg.date.map(full_timestamp).unwrap_or_default()),
                    ),
            )
            .child(address_block(s, state))
            // Must be in the header, not at the top of the message: the pane's
            // content area is covered by the webview, so a pill there would be
            // invisible the moment an HTML body loaded.
            .when(
                !s.selected_attachments.is_empty() || s.selected_attachments_loading,
                |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .pt_1()
                            .children(s.selected_attachments.iter().map(attachment_pill)),
                    )
                },
            );
    }

    // Plaintext only: HTML is the webview's job, and this is what sits behind
    // it for messages with no HTML part.
    let mut content = div()
        .id("reading-pane-content")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scroll()
        .p_4()
        .gap_2();

    // Nothing rather than the plaintext: while an overlay hides the webview,
    // exposing the fallback makes the message appear to change rendering; and
    // while an HTML body is preparing, the plaintext is what is available
    // first, so painting it is a styleless flash.
    let overlay_up = s.overlay_covers_reading_pane();
    let behind_overlay = overlay_up && s.selected_html_source.is_some();
    // One state from selection to rendered document: split in two it read as a
    // blank frame between them.
    let awaiting = s.selected_body_loading || s.selected_html_pending;

    if behind_overlay {
    } else if awaiting {
        content = content.child(
            div()
                .flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme::color(theme::TEXT_MUTED))
                .child(spinner())
                .child("Loading message…"),
        );
    } else if let Some(body) = &s.selected_body {
        content = content.child(div().w_full().child(body.clone()));
    } else {
        content = content.child(
            div()
                .w_full()
                .text_color(theme::color(theme::TEXT_MUTED))
                .child("(no plaintext body)"),
        );
    }

    // The rect probe must not live inside `content`: that scrolls, so the
    // probe would report bounds sliding up and drag the webview off-screen.
    let pane_rect = s.reading_pane_rect.clone();
    let body = div()
        .relative()
        .flex_1()
        // Painted here as well as in the webview's stylesheet so the two agree:
        // the native view arrives a frame after the layout that sizes it, and a
        // mismatched colour shows through until it does.
        //
        // Asked of the same function the stylesheet uses, never pinned to
        // `bg_message` -- that only matches a force-darkened message, and every
        // other one is on white. While a body is on its way the last one's
        // colour is held rather than reverting to the dark default.
        .bg(gpui::rgb(if awaiting {
            s.last_document_background
                .unwrap_or_else(|| crate::webview::document_background(s.selected_rendering()))
        } else {
            crate::webview::document_background(s.selected_rendering())
        }))
        .min_h(px(0.0))
        .child(content)
        .child(
            canvas(
                move |bounds, window, _cx| {
                    let next = (
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    );
                    if pane_rect.get() == next {
                        return;
                    }
                    pane_rect.set(next);
                    // Storing the rect is not enough: this runs during prepaint,
                    // *after* `Root::render` read the old one, and writing a
                    // `Cell` schedules no frame -- so a layout change that causes
                    // no redraw of its own leaves the webview parked.
                    //
                    // `request_animation_frame`, not `refresh()`: the latter does
                    // nothing while drawing, and prepaint is drawing. Guarded on
                    // an actual change, or this is an unconditional redraw loop.
                    window.request_animation_frame();
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full(),
        );

    div()
        .id("reading-pane")
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        // `show.toolbar = false` is how a config asks for no toolbar; an empty
        // `toolbar_actions` list is treated as a typo.
        .when(s.appearance.show.toolbar, |el| el.child(toolbar))
        .child(header)
        .child(body)
}

fn full_timestamp(timestamp: i64) -> String {
    let Some(when) = chrono::DateTime::from_timestamp(timestamp, 0) else {
        return String::new();
    };
    let when = when.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    if when.date_naive() == now.date_naive() {
        when.format("%H:%M").to_string()
    } else if when.year() == now.year() {
        when.format("%-d %b at %H:%M").to_string()
    } else {
        when.format("%-d %b %Y at %H:%M").to_string()
    }
}

/// Local time, never UTC: `msg.date` is a Unix timestamp, and rendering it in
/// UTC puts mail in the wrong day for anyone far enough from Greenwich.
fn relative_timestamp(timestamp: i64) -> String {
    let Some(when) = chrono::DateTime::from_timestamp(timestamp, 0) else {
        return String::new();
    };
    let when = when.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    if when.date_naive() == now.date_naive() {
        when.format("%H:%M").to_string()
    } else if when.year() == now.year() {
        when.format("%-d %b").to_string()
    } else {
        when.format("%-d %b %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pill_reports_a_size_a_reader_can_take_in() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(9_700 * 1024), "9.5 MB");
        assert_eq!(human_size(-1), "0 B");
    }
}
