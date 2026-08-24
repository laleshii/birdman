//! Shared plain-text editing primitives for every hand-rolled text field in
//! this app. gpui ships no text input widget -- its `examples/input.rs` is an
//! ~800-line reference you are expected to adapt -- so compose, the password
//! prompt and the search box all build on this rather than each drifting.
//!
//! Multi-line Home/End, what Enter does, and rendering genuinely differ per
//! field: callers check their own keys first and fall through to
//! [`try_common_edit_key`].

use gpui::KeyDownEvent;

pub fn prev_char_boundary(s: &str, mut i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn next_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub fn insert_str(content: &mut String, cursor: &mut usize, text: &str) {
    content.insert_str(*cursor, text);
    *cursor += text.len();
}

pub fn backspace(content: &mut String, cursor: &mut usize) {
    let start = prev_char_boundary(content, *cursor);
    if start < *cursor {
        let end = *cursor;
        content.replace_range(start..end, "");
        *cursor = start;
    }
}

pub fn delete_forward(content: &mut String, cursor: &mut usize) {
    let end = next_char_boundary(content, *cursor);
    if end > *cursor {
        let start = *cursor;
        content.replace_range(start..end, "");
    }
}

/// Callers check this before paying for `cx.read_from_clipboard()`;
/// [`try_common_edit_key`] checks it again, so the two cannot drift.
pub fn is_paste_keystroke(event: &KeyDownEvent) -> bool {
    event.keystroke.modifiers.secondary() && event.keystroke.key.as_str() == "v"
}

/// The offset a selection started from. The other end is always the cursor, so
/// `None` means "no selection".
pub type Anchor = Option<usize>;

pub fn selection_range(cursor: usize, anchor: Anchor) -> Option<(usize, usize)> {
    let anchor = anchor?;
    if anchor == cursor {
        return None;
    }
    Some((anchor.min(cursor), anchor.max(cursor)))
}

fn delete_selection(content: &mut String, cursor: &mut usize, anchor: &mut Anchor) -> bool {
    let Some((start, end)) = selection_range(*cursor, *anchor) else {
        *anchor = None;
        return false;
    };
    content.replace_range(start..end, "");
    *cursor = start;
    *anchor = None;
    true
}

/// Whitespace-delimited, which is close enough to Option+Left for an address.
fn prev_word_boundary(content: &str, at: usize) -> usize {
    let head = &content[..at];
    let trimmed = head.trim_end_matches(char::is_whitespace);
    trimmed
        .rfind(char::is_whitespace)
        .map(|i| i + content[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0)
}

fn next_word_boundary(content: &str, at: usize) -> usize {
    let tail = &content[at..];
    let skipped = tail.len() - tail.trim_start_matches(char::is_whitespace).len();
    let rest = &tail[skipped..];
    at + skipped + rest.find(char::is_whitespace).unwrap_or(rest.len())
}

/// Copy and cut return their text rather than acting: the clipboard belongs to
/// the window, not to a text buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    Ignored,
    Handled,
    Copied(String),
}

impl Edit {
    pub fn handled(&self) -> bool {
        *self != Edit::Ignored
    }
}

/// Shift is what distinguishes "move" from "extend". Clearing `anchor`
/// unconditionally here is what once made every selection die on the next
/// arrow press.
fn anchor_before_move(cursor: usize, anchor: &mut Anchor, extending: bool) {
    if extending {
        anchor.get_or_insert(cursor);
    } else {
        *anchor = None;
    }
}

pub fn try_common_edit_key(
    content: &mut String,
    cursor: &mut usize,
    anchor: &mut Anchor,
    event: &KeyDownEvent,
    clipboard_text: Option<&str>,
) -> Edit {
    if is_paste_keystroke(event) {
        return match clipboard_text {
            Some(text) if !text.is_empty() => {
                delete_selection(content, cursor, anchor);
                insert_str(content, cursor, text);
                Edit::Handled
            }
            _ => Edit::Ignored,
        };
    }

    let m = &event.keystroke.modifiers;
    let key = event.keystroke.key.as_str();
    let extending = m.shift;

    // `secondary` is Cmd on macOS, Ctrl elsewhere.
    if m.secondary() && !m.alt {
        match key {
            "a" => {
                *anchor = Some(0);
                *cursor = content.len();
            }
            "c" => {
                return match selection_range(*cursor, *anchor) {
                    Some((start, end)) => Edit::Copied(content[start..end].to_string()),
                    // Must not overwrite the clipboard with an empty string.
                    None => Edit::Handled,
                };
            }
            "x" => {
                return match selection_range(*cursor, *anchor) {
                    Some((start, end)) => {
                        let taken = content[start..end].to_string();
                        delete_selection(content, cursor, anchor);
                        Edit::Copied(taken)
                    }
                    None => Edit::Handled,
                };
            }
            "left" => {
                anchor_before_move(*cursor, anchor, extending);
                *cursor = 0;
            }
            "right" => {
                anchor_before_move(*cursor, anchor, extending);
                *cursor = content.len();
            }
            "backspace" => {
                if !delete_selection(content, cursor, anchor) {
                    content.replace_range(..*cursor, "");
                    *cursor = 0;
                }
            }
            _ => return Edit::Ignored,
        }
        return Edit::Handled;
    }

    if m.alt && !m.secondary() {
        match key {
            "left" => {
                anchor_before_move(*cursor, anchor, extending);
                *cursor = prev_word_boundary(content, *cursor);
            }
            "right" => {
                anchor_before_move(*cursor, anchor, extending);
                *cursor = next_word_boundary(content, *cursor);
            }
            "backspace" => {
                if !delete_selection(content, cursor, anchor) {
                    let start = prev_word_boundary(content, *cursor);
                    content.replace_range(start..*cursor, "");
                    *cursor = start;
                }
            }
            _ => return Edit::Ignored,
        }
        return Edit::Handled;
    }

    if m.control || m.platform {
        return Edit::Ignored;
    }

    match key {
        "left" if extending => {
            anchor_before_move(*cursor, anchor, true);
            *cursor = prev_char_boundary(content, *cursor);
        }
        "right" if extending => {
            anchor_before_move(*cursor, anchor, true);
            *cursor = next_char_boundary(content, *cursor);
        }
        "left" => {
            *cursor = selection_range(*cursor, *anchor).map_or_else(
                // Collapse to the edge rather than moving a character.
                || prev_char_boundary(content, *cursor),
                |(start, _)| start,
            );
            *anchor = None;
        }
        "right" => {
            *cursor = selection_range(*cursor, *anchor)
                .map_or_else(|| next_char_boundary(content, *cursor), |(_, end)| end);
            *anchor = None;
        }
        "home" => {
            anchor_before_move(*cursor, anchor, extending);
            *cursor = 0;
        }
        "end" => {
            anchor_before_move(*cursor, anchor, extending);
            *cursor = content.len();
        }
        "backspace" => {
            if !delete_selection(content, cursor, anchor) {
                backspace(content, cursor);
            }
        }
        "delete" => {
            if !delete_selection(content, cursor, anchor) {
                delete_forward(content, cursor);
            }
        }
        "space" => {
            delete_selection(content, cursor, anchor);
            insert_str(content, cursor, " ");
        }
        _ => match &event.keystroke.key_char {
            Some(ch) => {
                delete_selection(content, cursor, anchor);
                insert_str(content, cursor, ch);
            }
            None => return Edit::Ignored,
        },
    }
    Edit::Handled
}

/// A picker's filter has no cursor and no selection -- you type at the end and
/// backspace from it -- so this shares no state with [`try_common_edit_key`].
#[derive(Debug, PartialEq, Eq)]
pub enum PickerKey {
    Dismiss,
    Previous,
    Next,
    Confirm,
    Insert(String),
    Backspace,
    /// Deliberately *not* passed through: a picker is modal, and letting
    /// Delete reach the message list would act on mail behind it.
    Ignored,
}

pub fn classify_picker_key(event: &KeyDownEvent) -> PickerKey {
    // A modifier chord is a command, never filter text.
    if event.keystroke.modifiers.secondary() || event.keystroke.modifiers.control {
        return PickerKey::Ignored;
    }
    match event.keystroke.key.as_str() {
        "escape" => PickerKey::Dismiss,
        "up" => PickerKey::Previous,
        "down" => PickerKey::Next,
        "enter" => PickerKey::Confirm,
        "backspace" => PickerKey::Backspace,
        _ => match &event.keystroke.key_char {
            // `key_char` is what the layout produced; `key` is the physical
            // name, which types wrongly on a non-US keyboard.
            Some(text) if !text.is_empty() && !text.chars().any(|c| c.is_control()) => {
                PickerKey::Insert(text.clone())
            }
            _ => PickerKey::Ignored,
        },
    }
}

#[derive(Default)]
pub struct PickerState {
    pub query: String,
    /// Into the *filtered* list, not the full one.
    pub index: usize,
}

impl PickerState {
    pub fn reset(&mut self) {
        self.query.clear();
        self.index = 0;
    }

    /// Clamped rather than wrapping: against a list that shrinks as you type,
    /// wrapping past the end lands somewhere unpredictable.
    pub fn step(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.index = 0;
            return;
        }
        self.index = (self.index as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    /// Editing the query always resets the highlight: the old index means
    /// nothing against a different list.
    pub fn edit(&mut self, key: &PickerKey) -> bool {
        match key {
            PickerKey::Insert(text) => {
                self.query.push_str(text);
                self.index = 0;
                true
            }
            PickerKey::Backspace => {
                let changed = self.query.pop().is_some();
                self.index = 0;
                changed
            }
            _ => false,
        }
    }

    pub fn matches<'a>(&self, fields: impl IntoIterator<Item = &'a str>) -> bool {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        fields
            .into_iter()
            .any(|field| field.to_lowercase().contains(&query))
    }
}

#[cfg(test)]
mod tests {
    use gpui::Modifiers;

    fn press(key: &str, modifiers: Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers,
                key: key.into(),
                key_char: None,
            },
            is_held: false,
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn shortcut() -> Modifiers {
        let mut modifiers = Modifiers::default();
        if cfg!(target_os = "macos") {
            modifiers.platform = true;
        } else {
            modifiers.control = true;
        }
        modifiers
    }

    #[test]
    fn shift_and_an_arrow_extends_a_selection() {
        let (mut content, mut cursor, mut anchor) = ("hello".to_string(), 0, None);
        for _ in 0..3 {
            try_common_edit_key(
                &mut content,
                &mut cursor,
                &mut anchor,
                &press("right", shift()),
                None,
            );
        }
        assert_eq!(selection_range(cursor, anchor), Some((0, 3)));

        try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("left", shift()),
            None,
        );
        assert_eq!(selection_range(cursor, anchor), Some((0, 2)));
    }

    #[test]
    fn an_arrow_without_shift_still_drops_the_selection() {
        let (mut content, mut cursor, mut anchor) = ("hello".to_string(), 0, None);
        try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("right", shift()),
            None,
        );
        try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("right", Modifiers::default()),
            None,
        );
        assert_eq!(selection_range(cursor, anchor), None);
    }

    #[test]
    fn copy_returns_the_selection_and_leaves_it_alone() {
        let (mut content, mut cursor, mut anchor) = ("hello".to_string(), 5, Some(1));
        let outcome = try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("c", shortcut()),
            None,
        );
        assert_eq!(outcome, Edit::Copied("ello".into()));
        assert_eq!(content, "hello", "copy does not edit");
        assert_eq!(
            selection_range(cursor, anchor),
            Some((1, 5)),
            "and does not deselect"
        );
    }

    #[test]
    fn cut_returns_the_selection_and_removes_it() {
        let (mut content, mut cursor, mut anchor) = ("hello".to_string(), 5, Some(1));
        let outcome = try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("x", shortcut()),
            None,
        );
        assert_eq!(outcome, Edit::Copied("ello".into()));
        assert_eq!(content, "h");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn copy_with_no_selection_puts_nothing_on_the_clipboard() {
        let (mut content, mut cursor, mut anchor) = ("hello".to_string(), 2, None);
        let outcome = try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("c", shortcut()),
            None,
        );
        assert_eq!(outcome, Edit::Handled);
    }

    #[test]
    fn shift_extends_from_word_and_line_movement_too() {
        let (mut content, mut cursor, mut anchor) = ("one two".to_string(), 0, None);
        let alt_shift = Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::default()
        };
        try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("right", alt_shift),
            None,
        );
        assert_eq!(selection_range(cursor, anchor), Some((0, 3)));

        let (mut content, mut cursor, mut anchor) = ("one two".to_string(), 0, None);
        let mut shortcut_shift = shortcut();
        shortcut_shift.shift = true;
        try_common_edit_key(
            &mut content,
            &mut cursor,
            &mut anchor,
            &press("right", shortcut_shift),
            None,
        );
        assert_eq!(selection_range(cursor, anchor), Some((0, 7)));
    }

    fn picker_key(key: &str, key_char: Option<&str>) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: key.to_string(),
                key_char: key_char.map(str::to_string),
            },
            is_held: false,
        }
    }

    #[test]
    fn picker_navigation_keys_classify() {
        assert_eq!(
            classify_picker_key(&picker_key("up", None)),
            PickerKey::Previous
        );
        assert_eq!(
            classify_picker_key(&picker_key("down", None)),
            PickerKey::Next
        );
        assert_eq!(
            classify_picker_key(&picker_key("enter", None)),
            PickerKey::Confirm
        );
        assert_eq!(
            classify_picker_key(&picker_key("escape", None)),
            PickerKey::Dismiss
        );
        assert_eq!(
            classify_picker_key(&picker_key("backspace", None)),
            PickerKey::Backspace
        );
    }

    #[test]
    fn a_printable_key_becomes_filter_text_from_key_char() {
        // `key_char`, not `key`: they differ on a non-US layout.
        assert_eq!(
            classify_picker_key(&picker_key("a", Some("ä"))),
            PickerKey::Insert("ä".into())
        );
    }

    #[test]
    fn a_modifier_chord_is_never_typed_into_the_filter() {
        let mut event = picker_key("a", Some("a"));
        event.keystroke.modifiers = shortcut();
        assert_eq!(
            classify_picker_key(&event),
            PickerKey::Ignored,
            "the platform shortcut for Select All must not type an 'a'"
        );
    }

    #[test]
    fn a_control_character_is_not_filter_text() {
        assert_eq!(
            classify_picker_key(&picker_key("tab", Some("\t"))),
            PickerKey::Ignored
        );
    }

    #[test]
    fn stepping_clamps_instead_of_wrapping() {
        let mut picker = PickerState::default();
        picker.step(-1, 3);
        assert_eq!(picker.index, 0, "already at the top");
        picker.step(1, 3);
        picker.step(1, 3);
        picker.step(1, 3);
        assert_eq!(
            picker.index, 2,
            "clamped to the last row, not wrapped to the first"
        );
    }

    #[test]
    fn stepping_an_empty_list_stays_put() {
        let mut picker = PickerState::default();
        picker.step(1, 0);
        assert_eq!(picker.index, 0);
    }

    #[test]
    fn editing_the_query_resets_the_highlight() {
        let mut picker = PickerState::default();
        picker.step(1, 5);
        assert_eq!(picker.index, 1);
        assert!(picker.edit(&PickerKey::Insert("t".into())));
        assert_eq!(picker.index, 0);
        assert_eq!(picker.query, "t");

        assert!(picker.edit(&PickerKey::Backspace));
        assert_eq!(picker.query, "");
        assert!(!picker.edit(&PickerKey::Backspace));
    }

    #[test]
    fn matching_is_case_insensitive_across_every_field() {
        let mut picker = PickerState {
            query: "TRA".into(),
            ..PickerState::default()
        };
        assert!(picker.matches(["Trash", "[Gmail]/Trash"]));
        assert!(
            picker.matches(["Bin", "[Gmail]/Trash"]),
            "the path counts too"
        );
        assert!(!picker.matches(["Inbox", "INBOX"]));

        picker.query = "  ".into();
        assert!(
            picker.matches(["anything"]),
            "a blank filter matches everything"
        );
    }

    use super::*;

    #[test]
    fn char_boundaries_step_over_multibyte_chars_without_panicking() {
        // 'é' is 2 bytes (U+00E9), so byte offsets 1..3 straddle it.
        let s = "héllo";
        assert_eq!(s.len(), 6); // h(1) + é(2) + l(1) + l(1) + o(1)

        let after_h = next_char_boundary(s, 0);
        assert_eq!(after_h, 1);
        let after_e_acute = next_char_boundary(s, after_h);
        assert_eq!(after_e_acute, 3); // skipped both bytes of 'é' in one step
        assert!(s.is_char_boundary(after_e_acute));

        let back_to_h = prev_char_boundary(s, after_e_acute);
        assert_eq!(back_to_h, 1);
        assert!(s.is_char_boundary(back_to_h));
    }

    #[test]
    fn char_boundaries_handle_emoji_and_clamp_at_string_edges() {
        let s = "a🦀b"; // crab emoji is 4 bytes
        assert_eq!(prev_char_boundary(s, 0), 0); // can't go before start
        assert_eq!(next_char_boundary(s, s.len()), s.len()); // can't go past end

        let after_a = next_char_boundary(s, 0);
        assert_eq!(after_a, 1);
        let after_crab = next_char_boundary(s, after_a);
        assert_eq!(after_crab, 5); // 1 (a) + 4 (crab) bytes
        assert!(s.is_char_boundary(after_crab));
    }

    #[test]
    fn backspace_removes_one_char_before_cursor() {
        let mut s = "héllo".to_string();
        let mut cursor = 3; // just after 'é'
        backspace(&mut s, &mut cursor);
        assert_eq!(s, "hllo");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn delete_forward_removes_one_char_after_cursor() {
        let mut s = "héllo".to_string();
        let mut cursor = 1; // just before 'é'
        delete_forward(&mut s, &mut cursor);
        assert_eq!(s, "hllo");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn insert_str_advances_cursor_by_byte_len() {
        let mut s = "ab".to_string();
        let mut cursor = 1;
        insert_str(&mut s, &mut cursor, "é"); // 2 bytes
        assert_eq!(s, "aéb");
        assert_eq!(cursor, 3);
    }
}
