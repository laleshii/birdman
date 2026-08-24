//! Text a reader can select with the mouse, and copy.
//!
//! gpui ships no selectable text -- `InteractiveText` has clicks and tooltips
//! but no selection, because Zed builds selection on its editor. The pieces it
//! does expose are [`TextLayout::index_for_position`] and
//! [`StyledText::with_highlights`]; everything here is the bookkeeping between.
//!
//! The state is a cloneable handle rather than gpui element state because a
//! selection must survive the re-render that drawing it causes, and the owner
//! needs to reach the selected text to copy it.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    div, HighlightStyle, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StyledText, TextLayout, Window,
};

use crate::theme;

#[derive(Clone, Default)]
pub struct Selection(Rc<RefCell<Inner>>);

#[derive(Default)]
struct Inner {
    /// From the last render, so a mouse event hit-tests against the text as it
    /// was actually laid out.
    layout: Option<TextLayout>,
    text: SharedString,
    anchor: Option<usize>,
    cursor: usize,
    dragging: bool,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn selected_text(&self) -> Option<String> {
        let inner = self.0.borrow();
        let (start, end) = inner.range()?;
        Some(inner.text.get(start..end)?.to_string())
    }

    pub fn clear(&self) {
        let mut inner = self.0.borrow_mut();
        inner.anchor = None;
        inner.dragging = false;
    }

    fn select_all(&self) {
        let mut inner = self.0.borrow_mut();
        inner.anchor = Some(0);
        inner.cursor = inner.text.len();
    }

    /// `index_for_position` returns `Err` carrying the nearest index when the
    /// pointer is past the end of a line, which is what a drag wants.
    fn index_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<usize> {
        let inner = self.0.borrow();
        let layout = inner.layout.as_ref()?;
        match layout.index_for_position(position) {
            Ok(index) | Err(index) => Some(index.min(inner.text.len())),
        }
    }
}

impl Inner {
    fn range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }
}

pub fn selectable_text(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
    selection: &Selection,
) -> impl IntoElement {
    let text = text.into();
    {
        // Offsets into the old string mean nothing once the text changes.
        let mut inner = selection.0.borrow_mut();
        if inner.text != text {
            inner.text = text.clone();
            inner.anchor = None;
            inner.cursor = 0;
            inner.dragging = false;
        }
    }

    let highlights = selection.0.borrow().range().map(|(start, end)| {
        (
            start..end,
            HighlightStyle {
                background_color: Some(theme::color(theme::BG_SELECTED).into()),
                ..Default::default()
            },
        )
    });
    let styled = StyledText::new(text).with_highlights(highlights);
    selection.0.borrow_mut().layout = Some(styled.layout().clone());

    let down = selection.clone();
    let drag = selection.clone();
    let up = selection.clone();
    div()
        .id(id)
        .child(styled)
        .on_mouse_down(MouseButton::Left, move |event, window: &mut Window, _| {
            if event.click_count >= 2 {
                down.select_all();
            } else if let Some(index) = down.index_at(event.position) {
                let mut inner = down.0.borrow_mut();
                inner.anchor = Some(index);
                inner.cursor = index;
                inner.dragging = true;
            }
            // Nothing observes the handle, so the redraw must be asked for.
            // Safe from an event handler; `refresh` is a no-op mid-draw.
            window.refresh();
        })
        .on_mouse_move(move |event, window: &mut Window, _| {
            if !event.dragging() {
                drag.0.borrow_mut().dragging = false;
                return;
            }
            if !drag.0.borrow().dragging {
                return;
            }
            if let Some(index) = drag.index_at(event.position) {
                drag.0.borrow_mut().cursor = index;
                window.refresh();
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, window: &mut Window, _| {
            up.0.borrow_mut().dragging = false;
            window.refresh();
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_text(text: &str) -> Selection {
        let selection = Selection::new();
        selection.0.borrow_mut().text = text.to_string().into();
        selection
    }

    #[test]
    fn a_selection_is_the_span_between_anchor_and_cursor_either_way_round() {
        let selection = with_text("Quarterly report");
        {
            let mut inner = selection.0.borrow_mut();
            inner.anchor = Some(10);
            inner.cursor = 0;
        }
        assert_eq!(selection.selected_text().as_deref(), Some("Quarterly "));
    }

    #[test]
    fn an_empty_span_is_not_a_selection() {
        let selection = with_text("Quarterly report");
        selection.0.borrow_mut().anchor = Some(0);
        assert_eq!(selection.selected_text(), None);
    }

    #[test]
    fn select_all_takes_the_whole_string() {
        let selection = with_text("Quarterly report");
        selection.select_all();
        assert_eq!(
            selection.selected_text().as_deref(),
            Some("Quarterly report")
        );
    }

    #[test]
    fn a_multibyte_selection_never_splits_a_character() {
        let text = "café ☕ report";
        let selection = with_text(text);

        let boundary = text.char_indices().map(|(at, _)| at).nth(5).unwrap();
        {
            let mut inner = selection.0.borrow_mut();
            inner.anchor = Some(0);
            inner.cursor = boundary;
        }
        assert_eq!(
            selection.selected_text().as_deref(),
            Some(&text[..boundary])
        );

        // `get` rather than indexing, so a stale offset inside a character
        // yields `None` instead of taking the window down.
        let split = (1..text.len())
            .find(|at| !text.is_char_boundary(*at))
            .unwrap();
        selection.0.borrow_mut().cursor = split;
        assert_eq!(
            selection.selected_text(),
            None,
            "a split offset must not panic"
        );
    }
}
