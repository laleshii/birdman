use birdman_backend::{ComposeDraft, OutgoingMessage, Recipient};
use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, Context, FocusHandle, KeyDownEvent, Render,
    Window, WindowBounds, WindowOptions,
};

use crate::text_input;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

/// Bounded because the whole list is held in the window and filtered on every
/// keystroke. Ranked by how often you have corresponded.
const CONTACT_LIMIT: u32 = 2_000;

const SUGGESTION_LIMIT: usize = 6;

pub struct ComposeView {
    focus_handle: FocusHandle,
    body_scroll: gpui::ScrollHandle,
    field: Field,
    /// Byte offset into the *current* field. Reset to that field's end
    /// whenever focus moves; fields remember no earlier position.
    cursor: usize,
    /// The other end is `cursor`. Cleared whenever focus moves.
    anchor: crate::text_input::Anchor,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    /// Hidden by default, but opened automatically when a draft arrives with
    /// something in them: hiding a field that already has content is how a
    /// recipient goes missing.
    show_copies: bool,
    contacts: Vec<birdman_store::Contact>,
    suggestions: Vec<birdman_store::Contact>,
    suggestion: usize,
    accounts: Vec<SendAs>,
    from_index: usize,
    service: std::sync::Arc<birdman_client::Client>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    sending: bool,
    sent: bool,
    status: Option<String>,
}

/// Names an account rather than carrying its transport, so the compose window
/// never holds a connection.
#[derive(Clone)]
pub struct SendAs {
    pub account: birdman_store::AccountId,
    pub display_name: String,
    pub name: Option<String>,
    pub email: String,
}

impl ComposeView {
    pub fn new(
        cx: &mut App,
        draft: ComposeDraft,
        accounts: Vec<SendAs>,
        from_index: usize,
        service: std::sync::Arc<birdman_client::Client>,
    ) -> Self {
        let field = if draft.to.is_empty() {
            Field::To
        } else {
            Field::Body
        };
        let to = recipients_to_string(&draft.to);
        let body = draft.body;
        let cursor = match field {
            Field::To => to.len(),
            Field::Body => body.len(),
            _ => 0,
        };
        Self {
            focus_handle: cx.focus_handle(),
            field,
            cursor,
            anchor: None,
            body_scroll: gpui::ScrollHandle::new(),
            to,
            cc: recipients_to_string(&draft.cc),
            bcc: String::new(),
            show_copies: !draft.cc.is_empty(),
            contacts: Vec::new(),
            suggestions: Vec::new(),
            suggestion: 0,
            subject: draft.subject,
            body,
            from_index: from_index.min(accounts.len().saturating_sub(1)),
            accounts,
            service,
            in_reply_to: draft.in_reply_to,
            references: draft.references,
            sending: false,
            sent: false,
            status: None,
        }
    }

    pub fn open(
        cx: &mut App,
        draft: ComposeDraft,
        accounts: Vec<SendAs>,
        from_index: usize,
        service: std::sync::Arc<birdman_client::Client>,
    ) {
        let bounds = Bounds::centered(None, gpui::size(px(560.0), px(520.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("New Message".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| ComposeView::new(cx, draft, accounts, from_index, service));
                view.update(cx, |view, cx| {
                    view.focus_handle.focus(window);
                    // Not in `new`: the fetch needs a handle to the view to
                    // deliver its answer, and `new` has only an `App`.
                    view.load_contacts(cx);
                });
                view
            },
        )
        .expect("failed to open compose window");
    }

    fn current_field(&self) -> &str {
        match self.field {
            Field::To => &self.to,
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
            Field::Subject => &self.subject,
            Field::Body => &self.body,
        }
    }

    /// Disjoint-field: a single `&mut self` method returning `&mut String`
    /// would borrow all of `self`, `self.cursor` included.
    fn current_field_and_cursor(
        &mut self,
    ) -> (&mut String, &mut usize, &mut crate::text_input::Anchor) {
        let content = match self.field {
            Field::To => &mut self.to,
            Field::Cc => &mut self.cc,
            Field::Bcc => &mut self.bcc,
            Field::Subject => &mut self.subject,
            Field::Body => &mut self.body,
        };
        (content, &mut self.cursor, &mut self.anchor)
    }

    /// Once: filtering then happens in memory, which is the only way it keeps
    /// up with typing. A round trip per character would not.
    fn load_contacts(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let contacts = cx
                .background_spawn(
                    async move { service.contacts(CONTACT_LIMIT).unwrap_or_default() },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.contacts = this.own_accounts_as_contacts();
                // After, so a configured account name wins over whatever a
                // message happened to carry.
                this.contacts.extend(contacts.into_iter().filter(|c| {
                    !this
                        .accounts
                        .iter()
                        .any(|a| a.email.eq_ignore_ascii_case(&c.address))
                }));
                this.refresh_suggestions();
                cx.notify();
            });
        })
        .detach();
    }

    /// At the head of the list: an account that has never *received* mail is
    /// otherwise unreachable by completion. The one being sent from is filtered
    /// out later, in `refresh_suggestions`.
    fn own_accounts_as_contacts(&self) -> Vec<birdman_store::Contact> {
        self.accounts
            .iter()
            .map(|account| birdman_store::Contact {
                name: account
                    .name
                    .clone()
                    .or_else(|| Some(account.display_name.clone())),
                address: account.email.clone(),
                seen: u32::MAX,
                last_seen: i64::MAX,
            })
            .collect()
    }

    fn addressing(&self) -> bool {
        matches!(self.field, Field::To | Field::Cc | Field::Bcc)
    }

    /// Everything after the last comma: completing against the whole field
    /// would stop matching the moment a second recipient was added.
    fn partial_address(&self) -> Option<&str> {
        if !self.addressing() {
            return None;
        }
        let content = self.current_field();
        let start = content[..self.cursor.min(content.len())]
            .rfind(',')
            .map(|at| at + 1)
            .unwrap_or(0);
        let partial = content[start..self.cursor.min(content.len())].trim();
        (!partial.is_empty()).then_some(partial)
    }

    fn refresh_suggestions(&mut self) {
        self.suggestion = 0;
        let Some(partial) = self.partial_address().map(str::to_ascii_lowercase) else {
            self.suggestions.clear();
            return;
        };
        // Offering someone already on the message is offering a duplicate.
        let mut already: Vec<String> = parse_recipients(self.current_field())
            .into_iter()
            .map(|r| r.address.to_ascii_lowercase())
            .collect();
        // And the sending address: it is on almost every message, so it would
        // head every list, which is the one place it is never the answer. Only
        // that one -- mailing another of your accounts is unusual but real.
        if let Some(sender) = self.accounts.get(self.from_index) {
            already.push(sender.email.to_ascii_lowercase());
        }
        self.suggestions = self
            .contacts
            .iter()
            .filter(|c| !already.contains(&c.address.to_ascii_lowercase()))
            .filter(|c| {
                c.address.to_ascii_lowercase().contains(&partial)
                    || c.name
                        .as_deref()
                        .is_some_and(|n| n.to_ascii_lowercase().contains(&partial))
            })
            .take(SUGGESTION_LIMIT)
            .cloned()
            .collect();
    }

    fn accept_suggestion(&mut self) -> bool {
        let Some(contact) = self.suggestions.get(self.suggestion).cloned() else {
            return false;
        };
        let cursor = self.cursor;
        let (content, _, _) = self.current_field_and_cursor();
        let start = content[..cursor.min(content.len())]
            .rfind(',')
            .map(|at| at + 1)
            .unwrap_or(0);
        let replacement = match contact
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            Some(name) => format!("{name} <{}>", contact.address),
            None => contact.address.clone(),
        };
        // Trailing comma and space, so the next recipient types straight away.
        let padding = if start == 0 { "" } else { " " };
        let inserted = format!("{padding}{replacement}, ");
        content.replace_range(start..cursor.min(content.len()), &inserted);
        self.cursor = start + inserted.len();
        self.anchor = None;
        self.suggestions.clear();
        self.suggestion = 0;
        true
    }

    pub fn toggle_copies(&mut self, cx: &mut Context<Self>) {
        self.show_copies = !self.show_copies;
        if !self.show_copies && matches!(self.field, Field::Cc | Field::Bcc) {
            self.set_field(Field::To);
        }
        cx.notify();
    }

    fn set_field(&mut self, field: Field) {
        self.suggestions.clear();
        self.suggestion = 0;
        self.field = field;
        self.cursor = self.current_field().len();
        self.anchor = None;
    }

    fn move_home(&mut self) {
        let content = self.current_field();
        self.cursor = content[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    fn move_end(&mut self) {
        let content = self.current_field();
        self.cursor = content[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(content.len());
    }

    /// Preserves column where possible. On single-line fields this is just
    /// start/end.
    fn move_vertical(&mut self, delta: i32) {
        let content = self.current_field().to_string();
        let (line_idx, col) = line_and_col(&content, self.cursor);
        let lines: Vec<&str> = content.split('\n').collect();
        let new_line_idx = (line_idx as i32 + delta).clamp(0, lines.len() as i32 - 1) as usize;
        let target_col = col.min(lines[new_line_idx].chars().count());
        self.cursor = offset_for(&content, new_line_idx, target_col);
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending || self.sent {
            return;
        }
        let m = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();

        if m.secondary() && key == "enter" {
            self.send(cx);
            return;
        }
        if key == "escape" {
            window.remove_window();
            return;
        }

        // Only while showing, and only the keys that mean something to it, so
        // typing is never interrupted by a list the reader is ignoring.
        if !self.suggestions.is_empty() {
            match key {
                "up" => {
                    self.suggestion = self.suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.suggestion = (self.suggestion + 1).min(self.suggestions.len() - 1);
                    cx.notify();
                    return;
                }
                "tab" | "enter" | "return" => {
                    if self.accept_suggestion() {
                        cx.notify();
                        return;
                    }
                }
                "escape" => {
                    self.suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        match key {
            "home" => {
                self.move_home();
                cx.notify();
                return;
            }
            "end" => {
                self.move_end();
                cx.notify();
                return;
            }
            "up" => {
                self.move_vertical(-1);
                cx.notify();
                return;
            }
            "down" => {
                self.move_vertical(1);
                cx.notify();
                return;
            }
            "enter" | "return" => {
                if self.field == Field::Body {
                    let (content, cursor, _) = self.current_field_and_cursor();
                    text_input::insert_str(content, cursor, "\n");
                } else {
                    self.advance_field();
                }
                cx.notify();
                return;
            }
            "tab" => {
                self.advance_field();
                cx.notify();
                return;
            }
            _ => {}
        }

        let clipboard_text = if text_input::is_paste_keystroke(event) {
            cx.read_from_clipboard().and_then(|item| item.text())
        } else {
            None
        };
        let (content, cursor, anchor) = self.current_field_and_cursor();
        match text_input::try_common_edit_key(
            content,
            cursor,
            anchor,
            event,
            clipboard_text.as_deref(),
        ) {
            text_input::Edit::Ignored => {}
            text_input::Edit::Handled => {
                self.refresh_suggestions();
                cx.notify();
            }
            text_input::Edit::Copied(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                cx.notify();
            }
        }
    }

    fn advance_field(&mut self) {
        let next = match self.field {
            // Tab skips the copy rows while hidden, so the common draft is
            // still To -> Subject -> Body in three presses.
            Field::To if self.show_copies => Field::Cc,
            Field::To => Field::Subject,
            Field::Cc => Field::Bcc,
            Field::Bcc => Field::Subject,
            Field::Subject => Field::Body,
            Field::Body => Field::Body,
        };
        self.set_field(next);
    }

    /// Absolutely positioned: a list that reflowed the form on every keystroke
    /// would move the field out from under the cursor.
    fn suggestion_list(&self) -> Option<impl IntoElement> {
        if self.suggestions.is_empty() || !self.addressing() {
            return None;
        }
        let highlighted = self.suggestion;
        Some(gpui::deferred(
            div()
                // Deferred so it paints above the rows below, occluding so a
                // click lands on it rather than the row behind.
                .occlude()
                .absolute()
                .top(px(30.0))
                .left(px(56.0))
                .right(px(12.0))
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme::color(theme::BORDER))
                .bg(theme::color(theme::BG_LIST))
                .text_size(px(12.0))
                .children(self.suggestions.iter().enumerate().map(|(ix, contact)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .when(ix == highlighted, |el| {
                            el.bg(theme::color(theme::BG_SELECTED))
                        })
                        .when_some(contact.name.clone(), |el, name| {
                            el.child(div().flex_shrink_0().truncate().child(name))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(10.0))
                                .text_color(theme::color(theme::TEXT_MUTED))
                                .child(contact.address.clone()),
                        )
                })),
        ))
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let to = parse_recipients(&self.to);
        if to.is_empty() {
            self.status = Some("Add at least one recipient.".to_string());
            cx.notify();
            return;
        }

        let Some(account) = self.accounts.get(self.from_index).cloned() else {
            self.status = Some("No account configured to send from.".to_string());
            cx.notify();
            return;
        };
        let message = OutgoingMessage {
            // Never `display_name`: that put the sidebar's label in the header
            // and recipients saw `From: Gmail <...>`.
            from: Recipient::new(account.name.clone(), account.email.clone()),
            to,
            cc: parse_recipients(&self.cc),
            bcc: parse_recipients(&self.bcc),
            subject: self.subject.clone(),
            text_body: self.body.clone(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            message_id: None,
            date: None,
        };
        let service = self.service.clone();

        self.sending = true;
        self.status = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = service.send(account.account, message).await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                match result {
                    Ok(_) => this.sent = true,
                    Err(err) => this.status = Some(format!("Send failed: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn line_and_col(s: &str, cursor: usize) -> (usize, usize) {
    let before = &s[..cursor];
    let line_idx = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    (line_idx, col)
}

fn offset_for(s: &str, line_idx: usize, col: usize) -> usize {
    let mut offset = 0;
    for (i, line) in s.split('\n').enumerate() {
        if i == line_idx {
            let byte_col: usize = line.chars().take(col).map(char::len_utf8).sum();
            return offset + byte_col;
        }
        offset += line.len() + 1; // +1 for the '\n' this split ate
    }
    s.len()
}

fn recipients_to_string(recipients: &[Recipient]) -> String {
    recipients
        .iter()
        .map(|r| r.address.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Understands `Name <addr>` as well as a bare address. Autocomplete writes the
/// first form, and taking the whole string as the address sent
/// `To: "Ada Lovelace <ada@example.com>"`, which is not an address at all.
///
/// Splitting on `,` cuts a display name containing one in two. Left alone: the
/// halves still carry the address, and doing it properly needs a real parser.
fn parse_recipients(text: &str) -> Vec<Recipient> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|entry| match (entry.rfind('<'), entry.rfind('>')) {
            (Some(open), Some(close)) if close > open => {
                let address = entry[open + 1..close].trim();
                (!address.is_empty()).then(|| {
                    let name = entry[..open].trim().trim_matches('"').trim();
                    Recipient::new(
                        (!name.is_empty()).then(|| name.to_string()),
                        address.to_string(),
                    )
                })
            }
            _ => Some(Recipient::new(None, entry.to_string())),
        })
        .collect()
}

/// Cancelled by an equal negative margin where it is drawn, so the caret costs
/// no horizontal space.
const CARET_WIDTH: f32 = 1.5;

pub(crate) fn render_field_content(
    content: &str,
    cursor: usize,
    anchor: crate::text_input::Anchor,
    active: bool,
) -> impl IntoElement {
    let (cursor_line, cursor_col) = if active {
        line_and_col(content, cursor)
    } else {
        (usize::MAX, 0)
    };
    let selection = if active {
        crate::text_input::selection_range(cursor, anchor)
    } else {
        None
    };

    let mut line_start = 0usize;
    div()
        .flex()
        .flex_col()
        .children(
            content
                .split('\n')
                .enumerate()
                .map(|(i, line)| -> AnyElement {
                    let start = line_start;
                    line_start += line.len() + 1; // +1 for the '\n' that `split` removed
                    let highlight = selection.and_then(|(from, to)| {
                        let from = from.max(start).saturating_sub(start);
                        let to = to.min(start + line.len()).saturating_sub(start);
                        (from < to).then_some((from, to))
                    });

                    if let Some((from, to)) = highlight {
                        div()
                            .flex()
                            .child(line[..from].to_string())
                            .child(
                                div()
                                    .bg(theme::color(theme::BG_SELECTED))
                                    .child(line[from..to].to_string()),
                            )
                            .child(line[to..].to_string())
                            .into_any_element()
                    } else if i == cursor_line {
                        let byte_col: usize =
                            line.chars().take(cursor_col).map(char::len_utf8).sum();
                        let (before, after) = line.split_at(byte_col);
                        div()
                            .flex()
                            .child(before.to_string())
                            // Negative margin cancels the width: as a plain flex child the
                            // caret pushed everything after it sideways as it moved.
                            //
                            // No height either -- it stretches to the line, which is the
                            // only thing right across the 10px-13px range these fields use.
                            .child(
                                div()
                                    .w(px(CARET_WIDTH))
                                    .mr(px(-CARET_WIDTH))
                                    .bg(theme::color(theme::ACCENT)),
                            )
                            .child(after.to_string())
                            .into_any_element()
                    } else {
                        div().child(line.to_string()).into_any_element()
                    }
                }),
        )
}

impl Render for ComposeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field_row = |label: &'static str,
                         id: &'static str,
                         field: Field,
                         value: &str,
                         active: bool,
                         cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(theme::color(theme::BORDER))
                .when(active, |el| el.border_color(theme::color(theme::ACCENT)))
                .cursor_text()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_field(field);
                    this.focus_handle.focus(window);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(56.0))
                        .text_color(theme::color(theme::TEXT_MUTED))
                        .child(label),
                )
                .child(div().flex_1().child(render_field_content(
                    value,
                    self.cursor,
                    self.anchor,
                    active,
                )))
        };

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event, window, cx| this.handle_key(event, window, cx)))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::color(theme::BG_APP))
            .text_color(theme::color(theme::TEXT_PRIMARY))
            .text_size(px(13.0))
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().min_w(px(0.0)).child(field_row(
                                "To",
                                "compose-to",
                                Field::To,
                                &self.to.clone(),
                                self.field == Field::To,
                                cx,
                            )))
                            .child(
                                div()
                                    .id("compose-copies")
                                    .flex_shrink_0()
                                    .px_2()
                                    .text_size(px(11.0))
                                    .text_color(theme::color(theme::TEXT_MUTED))
                                    .cursor_pointer()
                                    .hover(|el| el.text_color(theme::color(theme::TEXT_PRIMARY)))
                                    .child(if self.show_copies {
                                        "More \u{2304}"
                                    } else {
                                        "More \u{203a}"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_copies(cx))),
                            ),
                    )
                    .children(self.suggestion_list()),
            )
            .when(self.show_copies, |el| {
                el.child(div().relative().child(field_row(
                    "Cc",
                    "compose-cc",
                    Field::Cc,
                    &self.cc.clone(),
                    self.field == Field::Cc,
                    cx,
                )))
                .child(div().relative().child(field_row(
                    "Bcc",
                    "compose-bcc",
                    Field::Bcc,
                    &self.bcc.clone(),
                    self.field == Field::Bcc,
                    cx,
                )))
            })
            .child(field_row(
                "Subject",
                "compose-subject",
                Field::Subject,
                &self.subject.clone(),
                self.field == Field::Subject,
                cx,
            ))
            .child({
                let handle = self.body_scroll.clone();
                let viewport = f32::from(handle.bounds().size.height);
                let overflow = f32::from(handle.max_offset().height);
                let content = viewport + overflow;
                let thumb_height = if content > 0.0 {
                    (viewport * viewport / content).max(crate::root::SCROLLBAR_MIN_THUMB_HEIGHT)
                } else {
                    crate::root::SCROLLBAR_MIN_THUMB_HEIGHT
                };
                let max_thumb_top = (viewport - thumb_height).max(0.0);
                let fraction = if overflow > 0.0 {
                    (-f32::from(handle.offset().y) / overflow).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .id("compose-body")
                            .size_full()
                            .p_2()
                            .cursor_text()
                            .when(self.field == Field::Body, |el| {
                                el.border_1().border_color(theme::color(theme::ACCENT))
                            })
                            .overflow_y_scroll()
                            .track_scroll(&self.body_scroll)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_field(Field::Body);
                                this.focus_handle.focus(window);
                                cx.notify();
                            }))
                            .child(render_field_content(
                                &self.body.clone(),
                                self.cursor,
                                self.anchor,
                                self.field == Field::Body,
                            )),
                    )
                    .when(overflow > 0.0 && viewport > 0.0, |el| {
                        el.child(crate::root::scrollbar_thumb(
                            fraction * max_thumb_top,
                            thumb_height,
                        ))
                    })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(theme::color(theme::BORDER))
                    .child(
                        div()
                            .id("send-button")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme::color(theme::ACCENT))
                            .cursor_pointer()
                            .when(self.sending || self.sent, |el| el.opacity(0.5))
                            .child(if self.sent {
                                "Queued"
                            } else if self.sending {
                                "Queueing..."
                            } else {
                                "Send"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                    )
                    .child(
                        div()
                            .text_color(theme::color(theme::TEXT_MUTED))
                            .text_size(px(11.0))
                            .child("Ctrl/Cmd+Enter to send, Esc to close"),
                    )
                    .when_some(self.status.clone(), |el, status| {
                        el.child(div().text_color(theme::color(theme::DANGER)).child(status))
                    }),
            )
    }
}

#[cfg(test)]
mod cursor_math_tests {
    use super::*;

    #[test]
    fn line_and_col_round_trips_through_offset_for() {
        let s = "first\nsecond line\nthird";
        for cursor in 0..=s.len() {
            if !s.is_char_boundary(cursor) {
                continue;
            }
            let (line, col) = line_and_col(s, cursor);
            assert_eq!(
                offset_for(s, line, col),
                cursor,
                "round trip failed for cursor {cursor}"
            );
        }
    }

    #[test]
    fn line_and_col_finds_correct_line_and_column() {
        let s = "abc\nde\nfghi";
        assert_eq!(line_and_col(s, 0), (0, 0)); // start of "abc"
        assert_eq!(line_and_col(s, 2), (0, 2)); // inside "abc"
        assert_eq!(line_and_col(s, 4), (1, 0)); // start of "de" (just past the \n)
        assert_eq!(line_and_col(s, 6), (1, 2)); // end of "de"
        assert_eq!(line_and_col(s, 7), (2, 0)); // start of "fghi"
        assert_eq!(line_and_col(s, 11), (2, 4)); // end of string
    }

    #[test]
    fn offset_for_clamps_a_short_target_line_to_its_own_length() {
        let s = "a long first line\nshort";
        let offset = offset_for(s, 1, 999);
        assert_eq!(offset, s.len());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_named_recipient_parses_into_name_and_address() {
        let parsed = parse_recipients("Ada Lovelace <ada@example.com>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "Ada Lovelace");
        assert_eq!(parsed[0].address, "ada@example.com");
    }

    #[test]
    fn bare_addresses_and_mixed_lists_still_work() {
        let parsed = parse_recipients("a@example.com, Bob <b@example.com> ,  c@example.com ");
        let pairs: Vec<_> = parsed
            .iter()
            .map(|r| (r.name.as_str(), r.address.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("a@example.com", "a@example.com"),
                ("Bob", "b@example.com"),
                ("c@example.com", "c@example.com"),
            ]
        );
    }

    #[test]
    fn a_quoted_name_loses_its_quotes_and_an_empty_one_is_dropped() {
        let parsed = parse_recipients("\"Ada\" <ada@example.com>,  <bare@example.com>");
        assert_eq!(parsed[0].name, "Ada");
        assert_eq!(parsed[1].address, "bare@example.com");
        assert_eq!(
            parsed[1].name, "bare@example.com",
            "an empty name falls back to the address"
        );
    }

    #[test]
    fn an_empty_bracket_pair_is_not_a_recipient() {
        assert!(parse_recipients("Nobody <>").is_empty());
    }

    use super::*;
}
