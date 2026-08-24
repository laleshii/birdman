use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use birdman_backend::ComposeDraft;
use birdman_store::{Folder, FolderId, MessageId, MessageSummary};
use gpui::{App, AppContext as _, Context};

use crate::compose::ComposeView;
use birdman_proto::sidebar_folder_rank;
pub use birdman_proto::{is_default_folder, OTHER_FOLDER_RANK};

/// Metadata only. The connectors live in the service; a client holding them
/// would be holding a live connection, which is what stops a second client
/// from existing. Routing is by `id`.
#[derive(Clone, Debug)]
pub struct AccountRuntime {
    pub id: birdman_store::AccountId,
    pub display_name: String,
    /// What outgoing mail is signed with. Not `display_name`, which labels the
    /// account in the sidebar.
    pub name: Option<String>,
    pub email: String,
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub text: String,
    pub failed: bool,
}

const SUBJECT_MENU_HEIGHT: f32 = 30.0;

/// Shorter than a notification: it confirms something the reader just did
/// deliberately, and the address underneath is what they will want back.
const COPY_CONFIRMATION: std::time::Duration = std::time::Duration::from_millis(1_200);

/// A failure gets no longer: the detail belongs in the log.
const NOTIFICATION_TTL: std::time::Duration = std::time::Duration::from_millis(2_500);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AccountScope {
    /// Merged. Only the default folders appear: a unified Inbox is meaningful,
    /// a union of two accounts' label trees is not.
    All,
    One(birdman_store::AccountId),
    #[default]
    Unset,
}

impl AccountScope {
    pub fn account(self) -> Option<birdman_store::AccountId> {
        match self {
            AccountScope::One(id) => Some(id),
            _ => None,
        }
    }
}

pub struct AppState {
    /// Mirrors `birdman_service::Service`'s methods, which is what let this move
    /// from an in-process call to a socket round trip without touching a call
    /// site.
    pub service: Arc<birdman_client::Client>,
    /// In config order. Everything routes by `birdman_store::AccountId`.
    pub accounts: Vec<AccountRuntime>,

    pub folders: Vec<Folder>,
    pub selected_folder: Option<FolderId>,
    pub messages: Vec<MessageSummary>,
    /// `messages` accumulates pages rather than being replaced, so this is the
    /// only thing that says whether more exist.
    messages_cursor: Option<birdman_store::PageCursor>,
    /// The scroll trigger runs every frame, so without this one scroll to the
    /// bottom queues a page load per frame.
    loading_more: bool,
    /// Counted in the store across the whole folder: `messages` holds only what
    /// has been paged in, so counting it reports the page size.
    pub selected_folder_counts: Option<(u32, u32)>,
    pub selected_message: Option<MessageId>,
    pub selected_body_loading: bool,
    pub selected_body: Option<String>,
    pub selected_html_source: Option<String>,
    pub selected_attachments: Vec<birdman_store::Attachment>,
    pub subject_selection: crate::selectable::Selection,
    pub copied_address: Option<String>,
    /// Reset on every selection, so one crowded thread does not leave every
    /// subsequent message expanded.
    pub header_expanded: bool,
    pub subject_menu: Option<gpui::Point<gpui::Pixels>>,
    /// A message says it has attachments long before they are on disk, and
    /// without this the header reads as "no attachments" rather than "not yet".
    pub selected_attachments_loading: bool,
    /// Cached: deciding it scans the whole document (~500us on a 100KB
    /// newsletter) and both the pane and the toolbar need it every frame.
    selected_supports_dark: bool,
    /// Held while the next message's body is fetched, so the pane does not
    /// flash a different colour.
    pub last_document_background: Option<u32>,
    /// The plaintext is available synchronously and the HTML is not, so without
    /// this the fallback flashes styleless for a frame before the webview
    /// replaces it.
    pub selected_html_pending: bool,
    /// FIFO, not LRU: at this size the bookkeeping is not worth it. Bounded,
    /// because entries hold their images inlined as base64.
    html_document_cache: std::collections::HashMap<MessageId, String>,
    html_document_cache_order: std::collections::VecDeque<MessageId>,
    pub status: Option<String>,
    /// Separate from `status`, which is a *state*. Sharing one line meant
    /// "Copied" overwrote "Syncing..." and sat there as the mailbox's state.
    pub notifications: Vec<Notification>,
    next_notification: u64,

    pub search_focus_handle: gpui::FocusHandle,
    /// So anything that takes focus can give it back. Without it, closing the
    /// search field leaves focus on a hidden element and **every keyboard
    /// shortcut silently stops working**.
    pub root_focus_handle: Option<gpui::FocusHandle>,
    pub filter: birdman_store::MessageFilter,
    /// Only meaningful with more than one account configured.
    pub account_scope: AccountScope,
    pub account_picker_open: bool,
    pub account_picker: crate::text_input::PickerState,
    /// Folders with nothing unread are absent rather than zero.
    pub folder_unread: std::collections::HashMap<FolderId, u32>,
    pub search_active: bool,
    /// On screen, as distinct from `search_active`, which is about *focus*: the
    /// box stays revealed while results show even after focus moves away.
    pub search_expanded: bool,
    pub search_query: String,
    pub search_cursor: usize,
    pub search_anchor: crate::text_input::Anchor,
    pub search_results: Option<Vec<MessageSummary>>,

    /// Must be stored rather than created fresh each render, or the list loses
    /// its scroll position every frame.
    pub list_scroll_handle: gpui::UniformListScrollHandle,
    /// Measured every frame, because `uniform_list` populates neither
    /// `ScrollHandle::max_offset()` nor `bounds()` -- bypassing the content-size
    /// machinery is the whole point of virtualizing.
    pub list_viewport_height: Rc<Cell<f32>>,
    /// Here rather than a local: `message_list_scrollbar` is rebuilt from
    /// scratch each render, so nothing stored there survives between frames.
    pub list_scrollbar_dragging: Rc<Cell<bool>>,
    /// `(mouse Y, scroll offset Y)` at drag start. Must live here: a drag always
    /// spans frames, and rebuilding it each render zeroes the origin so the next
    /// mouse-move computes an enormous delta and slams the thumb to the bottom.
    pub list_scrollbar_drag_start: Rc<Cell<(f32, f32)>>,
    /// A plain scrollable `Div`, unlike the message list, so `max_offset()` and
    /// `bounds()` are populated and no measuring `canvas()` is needed.
    pub sidebar_scroll_handle: gpui::ScrollHandle,
    /// Same across-frames reason as `list_scrollbar_dragging`.
    pub sidebar_scrollbar_dragging: Rc<Cell<bool>>,
    /// See [`AppState::list_scrollbar_drag_start`] for why this is not a local.
    pub sidebar_scrollbar_drag_start: Rc<Cell<(f32, f32)>>,
    /// `(x, y, width, height)` in window-relative logical points, measured every
    /// frame because a native child view is positioned by hand. Measured on a
    /// **non-scrolling** wrapper: a probe inside the scrolling content would
    /// drag the webview off-screen with it.
    pub reading_pane_rect: Rc<Cell<(f32, f32, f32, f32)>>,

    /// When each folder was last synced. In memory rather than persisted:
    /// losing it on restart is wanted, since a fresh process should check a
    /// folder the first time it is opened.
    folder_last_synced: std::collections::HashMap<FolderId, std::time::Instant>,
    pub appearance: crate::config::Appearance,
    pub sidebar_more_expanded: bool,
    pub move_picker_open: bool,
    pub palette_open: bool,
    /// Read on open rather than followed: two processes write this file, and
    /// tailing it is a lot of machinery for something opened after the fact.
    pub logs_open: bool,
    pub log_lines: Vec<String>,
    /// If this climbs while the reader moves, the backlog is real.
    pub opens_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Two moves within a few milliseconds means the event reached more than
    /// one handler -- the shape of "the selection jumps three messages".
    last_arrow: Option<std::time::Instant>,
    /// The folder whose next `MessagesChanged` is our own echo. Each such change
    /// is applied locally when issued, so acting on the announcement re-runs the
    /// folder query -- destructive in the unread-only view, where everything
    /// read since it opened vanishes and one delete looks like several.
    ///
    /// One slot, not a counter: each replaces the previous expectation, so a
    /// missed event is corrected by the next action rather than leaking.
    pub self_changed_folder: Option<FolderId>,
    /// `true` forces adaptation on, `false` off, absent means whatever the
    /// config and the message imply. It must push **both** ways: under `Auto` a
    /// message can already be un-adapted because the sender declares dark
    /// support, and a "turned off" set does nothing there.
    ///
    /// Per message and session-scoped: the reason to reach for it is that *this*
    /// email renders badly.
    pub dark_override: std::collections::HashMap<birdman_store::MessageId, bool>,
    /// Moving an index does not move a viewport, so the handle has to be asked.
    pub palette_scroll: gpui::ScrollHandle,
    pub palette: crate::text_input::PickerState,
    pub move_picker: crate::text_input::PickerState,
    /// Keyed by top-level segment (`Websites`, `[Google Mail]`).
    pub sidebar_expanded_groups: std::collections::HashSet<String>,
    /// Toggled by a button that moves with it: in the sidebar when visible, in
    /// the message list's header when hidden.
    pub sidebar_visible: bool,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: &mut Context<Self>,
        service: Arc<birdman_client::Client>,
        accounts: Vec<AccountRuntime>,
    ) -> Self {
        Self {
            service,
            accounts,
            folders: Vec::new(),
            selected_folder: None,
            messages: Vec::new(),
            messages_cursor: None,
            loading_more: false,
            selected_folder_counts: None,
            selected_message: None,
            selected_body_loading: false,
            selected_body: None,
            selected_html_source: None,
            selected_attachments: Vec::new(),
            subject_selection: crate::selectable::Selection::new(),
            copied_address: None,
            header_expanded: false,
            subject_menu: None,
            selected_attachments_loading: false,
            selected_supports_dark: false,
            last_document_background: None,
            selected_html_pending: false,
            html_document_cache: std::collections::HashMap::new(),
            html_document_cache_order: std::collections::VecDeque::new(),
            search_focus_handle: cx.focus_handle(),
            root_focus_handle: None,
            filter: birdman_store::MessageFilter::default(),
            account_scope: AccountScope::default(),
            account_picker_open: false,
            account_picker: crate::text_input::PickerState::default(),
            folder_unread: std::collections::HashMap::new(),
            search_active: false,
            search_expanded: false,
            search_query: String::new(),
            search_cursor: 0,
            search_anchor: None,
            search_results: None,
            // Deliberately empty: the daemon may have been running for hours,
            // and events are deltas that are never replayed, so assuming
            // "Syncing..." is how a status line gets stuck on it.
            status: None,
            notifications: Vec::new(),
            next_notification: 1,
            list_scroll_handle: gpui::UniformListScrollHandle::new(),
            list_viewport_height: Rc::new(Cell::new(0.0)),
            list_scrollbar_dragging: Rc::new(Cell::new(false)),
            list_scrollbar_drag_start: Rc::new(Cell::new((0.0, 0.0))),
            sidebar_scroll_handle: gpui::ScrollHandle::new(),
            sidebar_scrollbar_dragging: Rc::new(Cell::new(false)),
            sidebar_scrollbar_drag_start: Rc::new(Cell::new((0.0, 0.0))),
            reading_pane_rect: Rc::new(Cell::new((0.0, 0.0, 0.0, 0.0))),
            sidebar_visible: true,
            folder_last_synced: std::collections::HashMap::new(),
            appearance: crate::config::Appearance::default(),
            sidebar_more_expanded: false,
            move_picker_open: false,
            palette_open: false,
            logs_open: false,
            log_lines: Vec::new(),
            opens_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            last_arrow: None,
            self_changed_folder: None,
            dark_override: std::collections::HashMap::new(),
            palette_scroll: gpui::ScrollHandle::new(),
            palette: crate::text_input::PickerState::default(),
            move_picker: crate::text_input::PickerState::default(),
            sidebar_expanded_groups: std::collections::HashSet::new(),
        }
    }

    pub fn toggle_sidebar_group(&mut self, group: &str, cx: &mut Context<Self>) {
        if !self.sidebar_expanded_groups.remove(group) {
            self.sidebar_expanded_groups.insert(group.to_string());
        }
        cx.notify();
    }

    pub fn toggle_sidebar_more(&mut self, cx: &mut Context<Self>) {
        self.sidebar_more_expanded = !self.sidebar_more_expanded;
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    /// Accounts come from the store, not from a config type: which accounts
    /// exist is a fact about the local data.
    pub fn refresh_folders(&mut self, cx: &mut Context<Self>) {
        // Sidebar order is the service's promise, not this client's.
        self.folders = self.service.folders(None).unwrap_or_default();
        self.folder_unread = self
            .service
            .unread_counts()
            .unwrap_or_default()
            .into_iter()
            .collect();

        // Merged by default, except with one account, where `All` is the same
        // inbox with the custom folders hidden -- strictly worse, for no gain.
        if self.account_scope == AccountScope::Unset && !self.accounts.is_empty() {
            self.account_scope = match self.accounts.as_slice() {
                [only] => AccountScope::One(only.id),
                _ => AccountScope::All,
            };
        }

        // A folder can vanish under the selection when the sync prunes it.
        if self
            .selected_folder
            .is_some_and(|id| !self.folders.iter().any(|f| f.id == id))
        {
            self.selected_folder = None;
            self.selected_message = None;
            self.selected_body = None;
            self.set_selected_html(None);
        }

        if self.selected_folder.is_none() {
            if let Some(inbox_id) = self.inbox_in_scope() {
                self.select_folder(inbox_id, cx);
                return;
            }
        }
        cx.notify();
    }

    /// With a single account this is every folder.
    pub fn visible_folders(&self) -> Vec<&Folder> {
        if self.accounts.len() <= 1 {
            return self.folders.iter().collect();
        }
        match self.account_scope {
            AccountScope::One(account) => self
                .folders
                .iter()
                .filter(|f| f.account_id == account)
                .collect(),
            // One row per default folder, standing for every account's copy.
            // The first account's is the representative; selecting it expands to
            // all of them (see `selected_folder_ids`).
            AccountScope::All => {
                let mut seen = Vec::new();
                let mut rows = Vec::new();
                for folder in self.folders.iter().filter(|f| is_default_folder(f)) {
                    let rank = sidebar_folder_rank(folder);
                    if !seen.contains(&rank) {
                        seen.push(rank);
                        rows.push(folder);
                    }
                }
                rows
            }
            AccountScope::Unset => self.folders.iter().collect(),
        }
    }

    /// Only the default folders get one. In the merged view a default row
    /// stands for that folder on every account, so its bubble sums them -- the
    /// same expansion `selected_folder_ids` does, so the two cannot disagree.
    pub fn unread_badge(&self, folder: &Folder) -> Option<u32> {
        if !is_default_folder(folder) {
            return None;
        }
        let total = if self.is_merged_view() {
            let rank = sidebar_folder_rank(folder);
            self.folders
                .iter()
                .filter(|f| sidebar_folder_rank(f) == rank)
                .filter_map(|f| self.folder_unread.get(&f.id))
                .sum()
        } else {
            self.folder_unread.get(&folder.id).copied().unwrap_or(0)
        };
        (total > 0).then_some(total)
    }

    /// Custom folders are hidden in it, so "show more" has nothing to reveal.
    pub fn is_merged_view(&self) -> bool {
        self.accounts.len() > 1 && self.account_scope == AccountScope::All
    }

    fn inbox_in_scope(&self) -> Option<FolderId> {
        let account = self.account_scope.account();
        self.folders
            .iter()
            .find(|f| {
                f.imap_path.eq_ignore_ascii_case("INBOX")
                    && account.is_none_or(|id| f.account_id == id)
            })
            .map(|f| f.id)
    }

    /// Lands on the inbox: the previously selected folder belongs to the
    /// account being left, so keeping it shows one account's name above
    /// another's mail.
    pub fn select_account(&mut self, scope: AccountScope, cx: &mut Context<Self>) {
        self.account_picker_open = false;
        self.account_picker.reset();
        if self.account_scope == scope {
            cx.notify();
            return;
        }
        self.account_scope = scope;
        self.selected_folder = None;
        // `visible_messages` prefers results, which would otherwise keep
        // showing over the new account's inbox.
        self.search_results = None;
        match self.inbox_in_scope() {
            Some(inbox) => self.select_folder(inbox, cx),
            None => {
                self.messages.clear();
                self.messages_cursor = None;
                self.selected_message = None;
                self.selected_body = None;
                self.set_selected_html(None);
                cx.notify();
            }
        }
    }

    /// Once at startup; the event stream keeps it current after that.
    pub fn refresh_sync_status(&mut self, cx: &mut Context<Self>) {
        use birdman_proto::SyncState;
        let Ok(state) = self.service.sync_status() else {
            return;
        };
        // Worst news wins, which is the only answer one line can honestly give.
        self.status = state
            .iter()
            .find_map(|(_, s)| match s {
                SyncState::Failed { message } => {
                    Some(format!("Sync error: {}", short_error(message)))
                }
                _ => None,
            })
            .or_else(|| {
                state.iter().find_map(|(_, s)| match s {
                    SyncState::Syncing { folder: Some(name) } => Some(format!("Syncing {name}...")),
                    SyncState::Syncing { folder: None } => Some("Syncing...".to_string()),
                    _ => None,
                })
            })
            .or(Some("Synced".to_string()));
        cx.notify();
    }

    pub fn toggle_account_picker(&mut self, cx: &mut Context<Self>) {
        self.account_picker_open = !self.account_picker_open;
        self.account_picker.reset();
        cx.notify();
    }

    /// The current scope is excluded: it is named in the row above.
    pub fn account_picker_options(&self) -> Vec<(AccountScope, String)> {
        std::iter::once(AccountScope::All)
            .chain(self.accounts.iter().map(|a| AccountScope::One(a.id)))
            .filter(|scope| *scope != self.account_scope)
            .map(|scope| {
                let label = match scope {
                    AccountScope::All => "All accounts".to_string(),
                    AccountScope::One(id) => self
                        .account(id)
                        .map(|a| a.display_name.clone())
                        .unwrap_or_default(),
                    AccountScope::Unset => String::new(),
                };
                (scope, label)
            })
            .filter(|(_, label)| self.account_picker.matches([label.as_str()]))
            .collect()
    }

    /// Same contract as the other pickers.
    pub fn account_picker_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        use crate::text_input::PickerKey;
        let key = crate::text_input::classify_picker_key(event);
        match key {
            PickerKey::Dismiss => {
                self.account_picker_open = false;
                self.account_picker.reset();
                cx.notify();
            }
            PickerKey::Previous | PickerKey::Next => {
                let delta = if matches!(key, PickerKey::Previous) {
                    -1
                } else {
                    1
                };
                let len = self.account_picker_options().len();
                self.account_picker.step(delta, len);
                cx.notify();
            }
            PickerKey::Confirm => {
                let chosen = self
                    .account_picker_options()
                    .get(self.account_picker.index)
                    .map(|(scope, _)| *scope);
                if let Some(scope) = chosen {
                    self.select_account(scope, cx);
                }
            }
            PickerKey::Insert(_) | PickerKey::Backspace => {
                self.account_picker.edit(&key);
                cx.notify();
            }
            PickerKey::Ignored => {}
        }
    }

    /// Only INBOX is kept live by the supervisor -- IMAP allows one selected
    /// mailbox per connection -- so everything else refreshes when opened.
    /// [`FOLDER_SYNC_TTL`] keeps repeated clicking from re-syncing; the Sync
    /// button bypasses it.
    fn sync_folders_if_stale(&mut self, folder_ids: Vec<FolderId>, cx: &mut Context<Self>) {
        let stale: Vec<_> = folder_ids
            .into_iter()
            .filter(|id| {
                self.folder_last_synced
                    .get(id)
                    .is_none_or(|at| at.elapsed() >= FOLDER_SYNC_TTL)
            })
            .collect();
        if !stale.is_empty() {
            self.resync_folders(stale, cx);
        }
    }

    pub fn select_folder(&mut self, folder_id: FolderId, cx: &mut Context<Self>) {
        // The filter belongs to the folder it was turned on in: carried across,
        // it shows an empty list indistinguishable from an empty folder.
        self.filter = birdman_store::MessageFilter::default();
        self.selected_folder = Some(folder_id);
        self.selected_message = None;
        self.selected_body = None;
        self.selected_body_loading = false;
        self.set_selected_html(None);
        self.refresh_messages(cx);
        // The whole tree: selecting a parent shows its children's mail too.
        self.sync_folders_if_stale(self.selected_folder_ids(folder_id), cx);
    }

    /// Every folder the selection stands for: in the merged view a default row
    /// covers that folder on every account, everywhere else it is `folder_id`
    /// plus the tree nested under it.
    ///
    /// Nesting is by path prefix plus the server's delimiter -- IMAP has no
    /// parent link, the hierarchy is only in the names -- and the delimiter
    /// guard is what stops `Websites` also claiming `WebsitesArchive`.
    fn selected_folder_ids(&self, folder_id: FolderId) -> Vec<FolderId> {
        if self.is_merged_view() {
            if let Some(selected) = self.folders.iter().find(|f| f.id == folder_id) {
                let rank = sidebar_folder_rank(selected);
                if rank < OTHER_FOLDER_RANK {
                    return self
                        .folders
                        .iter()
                        .filter(|f| sidebar_folder_rank(f) == rank)
                        .map(|f| f.id)
                        .collect();
                }
            }
        }
        self.selected_folder_tree(folder_id)
    }

    fn selected_folder_tree(&self, folder_id: FolderId) -> Vec<FolderId> {
        let Some(selected) = self.folders.iter().find(|f| f.id == folder_id) else {
            return vec![folder_id];
        };
        let prefix = format!(
            "{}{}",
            selected.imap_path,
            selected.delimiter.as_deref().unwrap_or("/")
        );
        std::iter::once(folder_id)
            .chain(
                self.folders
                    .iter()
                    .filter(|f| f.id != folder_id && f.imap_path.starts_with(&prefix))
                    .map(|f| f.id),
            )
            .collect()
    }

    pub fn refresh_messages(&mut self, cx: &mut Context<Self>) {
        let Some(folder_id) = self.selected_folder else {
            self.messages.clear();
            cx.notify();
            return;
        };
        // The selection plus its descendants, shown as one stream.
        let folder_ids = self.selected_folder_ids(folder_id);
        // As many as are currently shown, not just the first page: a refresh
        // fires on every sync event, and resetting to page one would yank the
        // viewport back. Capped, so a deep scroll is still a bounded query.
        let want = self
            .messages
            .len()
            .max(MESSAGE_PAGE_LIMIT as usize)
            .min(MESSAGE_REFRESH_CAP as usize) as u32;
        // Still synchronous: this fires on sync events rather than keystrokes,
        // so it is the next candidate rather than the current problem.
        let messages = on_main("message list query", || {
            self.service
                .messages(folder_ids.clone(), None, want, self.filter)
                .unwrap_or_default()
        });
        let counts = on_main("message count query", || {
            self.service.message_counts(folder_ids.clone()).ok()
        });
        // The refetch may exceed one page, so the cursor comes from what was
        // actually read rather than from a page-size assumption.
        self.messages_cursor = next_cursor_after(&messages, want);
        let mut messages = messages;
        if self.filter.unread {
            // The unread filter selects what the list is *built* from, not what
            // it keeps showing: a message read while you look at it stays,
            // greyed, until the view is rebuilt. Filtering live is what made one
            // delete look like several, since the refresh re-ran the unread
            // query and everything read since the view opened vanished with it.
            //
            // Scoped to this folder set, or `select_folder`'s carried-over rows
            // would drag a whole other mailbox into this one.
            let carried: Vec<_> = self
                .messages
                .iter()
                .filter(|row| folder_ids.contains(&row.folder_id))
                .filter(|row| !messages.iter().any(|m| m.id == row.id))
                .cloned()
                .collect();
            for row in carried {
                let at = messages
                    .iter()
                    .position(|m| (m.date, m.id) < (row.date, row.id))
                    .unwrap_or(messages.len());
                messages.insert(at, row);
            }
        }
        self.messages = messages;
        self.selected_folder_counts = counts;
        self.loading_more = false;
        cx.notify();
    }

    /// No-op at the end of the folder, while a page is loading, or during a
    /// search. Foreground: an indexed keyset query over local SQLite, and a
    /// background task would mean holding a cursor across frames for no gain.
    pub fn load_more_messages(&mut self, cx: &mut Context<Self>) {
        if self.loading_more || self.search_results.is_some() {
            return;
        }
        let (Some(folder_id), Some(cursor)) = (self.selected_folder, self.messages_cursor) else {
            return;
        };
        self.loading_more = true;
        let folder_ids = self.selected_folder_ids(folder_id);
        let page = self
            .service
            .messages(folder_ids, Some(cursor), MESSAGE_PAGE_LIMIT, self.filter)
            .unwrap_or_default();
        self.messages_cursor = next_cursor(&page);
        self.messages.extend(page);
        self.loading_more = false;
        cx.notify();
    }

    fn cache_html_document(&mut self, message_id: MessageId, document: String) {
        if !self.html_document_cache.contains_key(&message_id) {
            self.html_document_cache_order.push_back(message_id);
            if self.html_document_cache_order.len() > HTML_DOCUMENT_CACHE_CAP {
                if let Some(oldest) = self.html_document_cache_order.pop_front() {
                    self.html_document_cache.remove(&oldest);
                }
            }
        }
        self.html_document_cache.insert(message_id, document);
    }

    /// Clicking a filter is a refinement of the search being typed, not a
    /// departure from it.
    pub fn keep_search_focus(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.search_active {
            self.search_focus_handle.focus(window);
            cx.notify();
        }
    }

    pub fn toggle_unread_only(&mut self, cx: &mut Context<Self>) {
        self.filter.unread = !self.filter.unread;
        self.reset_for_filter_change(cx);
    }

    /// Shows only messages carrying an attachment.
    ///
    pub fn toggle_attachments_only(&mut self, cx: &mut Context<Self>) {
        self.filter.attachments = !self.filter.attachments;
        self.reset_for_filter_change(cx);
    }

    /// Restarts whichever list is on screen, search included: the page it was
    /// showing came from a different result set and its cursor points into that
    /// one.
    fn reset_for_filter_change(&mut self, cx: &mut Context<Self>) {
        self.list_scroll_handle
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        if self.search_results.is_some() {
            self.run_search(cx);
            return;
        }
        self.messages.clear();
        self.messages_cursor = None;
        self.refresh_messages(cx);
    }

    pub fn has_more_messages(&self) -> bool {
        self.messages_cursor.is_some() && self.search_results.is_none()
    }

    /// From `self.folders`, so routing a command costs no store lookup.
    pub fn account_of_folder(&self, folder_id: FolderId) -> Option<birdman_store::AccountId> {
        self.folders
            .iter()
            .find(|f| f.id == folder_id)
            .map(|f| f.account_id)
    }

    fn account_of_message(&self, message_id: MessageId) -> Option<birdman_store::AccountId> {
        let folder = self
            .visible_messages()
            .iter()
            .find(|m| m.id == message_id)
            .map(|m| m.folder_id)?;
        self.account_of_folder(folder)
    }

    pub fn account(&self, id: birdman_store::AccountId) -> Option<&AccountRuntime> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Falls back to the first configured account, so compose works before any
    /// selection.
    pub fn active_account(&self) -> Option<&AccountRuntime> {
        self.selected_folder
            .and_then(|f| self.account_of_folder(f))
            .and_then(|id| self.account(id))
            .or_else(|| self.accounts.first())
    }

    pub fn self_address(&self) -> String {
        self.active_account()
            .map(|a| a.email.clone())
            .unwrap_or_default()
    }

    /// The one place the UI invokes a backend. `on_done` runs only on success,
    /// with `&mut AppState`, so an action can follow up without threading state
    /// through the async block.
    fn dispatch(
        &mut self,
        account: birdman_store::AccountId,
        command: birdman_backend::Command,
        cx: &mut Context<Self>,
        on_done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        // These all apply to the visible list immediately, so the folder's
        // announcement tells this window nothing. See `self_changed_folder`.
        self.self_changed_folder = self.selected_folder;
        let label = command.describe();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let result = service.execute(account, command).await;
            let _ = this.update(cx, |state, cx| match result {
                Ok(_) => on_done(state, cx),
                Err(err) => {
                    log::warn!("{label} failed: {err}");
                    state.notify_failure(
                        format!("{label} failed: {}", short_error(&err.to_string())),
                        cx,
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Resyncs the *other end* of an action: archiving moves a message into All
    /// Mail, deleting into Trash, flagging into Starred, and nothing syncs those
    /// folders on its own once the supervisor is down to IDLEing on INBOX.
    /// Without this the message vanishes from where it was and never appears
    /// where it went.
    ///
    /// Deliberately quiet -- the action itself already reported its outcome.
    pub fn resync_folders(&self, folder_ids: Vec<FolderId>, cx: &mut Context<Self>) {
        if folder_ids.is_empty() {
            return;
        }
        // Before spawning: `self.folders` is only readable here.
        let targets: Vec<_> = folder_ids
            .into_iter()
            .filter_map(|folder| Some((folder, self.account_of_folder(folder)?)))
            .collect();
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            for (folder, account) in targets {
                // Sequential: folders on one account share a connection, so
                // concurrency only makes the mailbox thrash between them.
                match service
                    .execute(account, birdman_backend::Command::SyncFolder { folder })
                    .await
                {
                    Ok(_) => {
                        let _ = this.update(cx, |state, _| {
                            // A failed sync must not start the TTL, or it
                            // suppresses its own retry.
                            state
                                .folder_last_synced
                                .insert(folder, std::time::Instant::now());
                        });
                    }
                    // Quiet by design: nobody asked for this.
                    Err(err) => log::warn!("folder resync failed: {err}"),
                }
            }
            let _ = this.update(cx, |state, cx| {
                state.refresh_folders(cx);
                state.refresh_messages(cx);
            });
        })
        .detach();
    }

    /// **Scoped to one account.** With several configured, `self.folders` holds
    /// every account's Trash, and archiving a work message into a personal one
    /// would be real data loss.
    fn special_folder(
        &self,
        account: birdman_store::AccountId,
        special_use: birdman_store::SpecialUse,
    ) -> Option<&birdman_store::Folder> {
        self.folders
            .iter()
            .find(|f| f.account_id == account && f.special_use == Some(special_use))
    }

    /// Polls modification times rather than using inotify/FSEvents: the thing
    /// watched is often a **symlink** an external tool retargets, and those
    /// watches follow the resolved file, so they go silent exactly when the link
    /// is repointed. Polling re-resolves the link every time.
    pub fn watch_appearance(&mut self, cx: &mut Context<Self>) {
        self.apply_appearance(cx);
        cx.spawn(async move |this, cx| {
            let mut seen = appearance_fingerprint();
            loop {
                cx.background_executor()
                    .timer(APPEARANCE_POLL_INTERVAL)
                    .await;
                let current = appearance_fingerprint();
                if current == seen {
                    continue;
                }
                seen = current;
                if this
                    .update(cx, |state, cx| state.apply_appearance(cx))
                    .is_err()
                {
                    break; // window gone
                }
            }
        })
        .detach();
    }

    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        let appearance = crate::config::load_appearance();
        let changed = appearance.palette != self.appearance.palette;
        let remote_images_changed = appearance.remote_images != self.appearance.remote_images;
        // Only on a *change*: the config sets the default, not the current
        // state, so a reload must not stomp a hand-toggled sidebar.
        if appearance.show.sidebar != self.appearance.show.sidebar {
            self.sidebar_visible = appearance.show.sidebar;
        }
        crate::theme::set_palette(appearance.palette);
        self.appearance = appearance;
        if remote_images_changed {
            self.html_document_cache.clear();
            self.html_document_cache_order.clear();
            if let Some(message_id) = self.selected_message {
                self.select_message(message_id, cx);
            }
        }
        if changed {
            log::info!("appearance reloaded from config");
        }
        cx.notify();
    }

    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        let Some(folder_id) = self.selected_folder else {
            return;
        };
        let Some(folder) = self.folders.iter().find(|f| f.id == folder_id).cloned() else {
            return;
        };

        // Every account's copy in the merged view: syncing one inbox while
        // showing three leaves the view silently stale.
        let targets: Vec<(FolderId, birdman_store::AccountId)> = self
            .selected_folder_ids(folder_id)
            .into_iter()
            .filter_map(|id| Some((id, self.account_of_folder(id)?)))
            .collect();
        let service = self.service.clone();
        if targets.is_empty() {
            return;
        }

        self.status = Some(format!("Syncing {}...", folder.name));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut failure = None;
            let mut listed: Vec<birdman_store::AccountId> = Vec::new();
            for (id, account) in targets {
                // An explicit sync is also how a folder created or renamed on
                // the server shows up without a restart.
                if !listed.contains(&account) {
                    listed.push(account);
                    if let Err(err) = service
                        .execute(account, birdman_backend::Command::ListFolders)
                        .await
                    {
                        failure.get_or_insert(err.to_string());
                        continue;
                    }
                }
                match service
                    .execute(account, birdman_backend::Command::SyncFolder { folder: id })
                    .await
                {
                    Ok(_) => {
                        let _ = this.update(cx, |state, _| {
                            // Resets the freshness clock, so opening the folder
                            // straight after does not sync twice.
                            state
                                .folder_last_synced
                                .insert(id, std::time::Instant::now());
                        });
                    }
                    Err(err) => {
                        failure.get_or_insert(err.to_string());
                    }
                }
            }

            let _ = this.update(cx, |state, cx| {
                state.status = match &failure {
                    None => Some("Synced".to_string()),
                    Some(err) => {
                        log::error!("sync now failed: {err}");
                        Some(format!("Sync error: {}", short_error(err)))
                    }
                };
                state.refresh_folders(cx);
                state.refresh_messages(cx);
            });
        })
        .detach();
    }

    pub fn select_adjacent(&mut self, delta: i32, cx: &mut Context<Self>) {
        let visible = self.visible_messages();
        if visible.is_empty() {
            return;
        }
        let current_index = self
            .selected_message
            .and_then(|id| visible.iter().position(|m| m.id == id));
        let next_index = match current_index {
            Some(i) => (i as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize,
            None => 0,
        };
        let visible_len = visible.len();
        let next_id = visible[next_index].id;
        log::debug!("ARROWPROBE {current_index:?} -> {next_index} (len {visible_len})");
        // Default level, because the symptom is intermittent and asking for a
        // repro with logging turned up is asking for it twice.
        let now = std::time::Instant::now();
        if let Some(previous) = self.last_arrow.replace(now) {
            let apart = now.duration_since(previous);
            if apart < REPEAT_ARROW_WINDOW {
                log::warn!(
                    "arrow handled again {}ms after the last one ({current_index:?} -> {next_index}) \
                     -- one keypress reaching more than one handler?",
                    apart.as_millis()
                );
            }
        }
        // Selected first, because that notifies: a scroll requested before it
        // competes with the render it triggers.
        self.select_message(next_id, cx);
        self.scroll_row_into_view(next_index);
        cx.notify();
    }

    /// Scrolls only far enough to put `index` on screen, and not at all when it
    /// already is.
    ///
    /// gpui 0.2.2's `ScrollStrategy` offers only `Top`, `Center` and `Bottom`.
    /// `Center` re-centres on every keypress, so the list lurches half a screen
    /// while the selection moves one row; `Top` is the same in the other
    /// direction. This is the `Nearest` the newer gpui has and this one does
    /// not -- everything it needs is already tracked for the scrollbar.
    fn scroll_row_into_view(&self, index: usize) {
        let row = self.appearance.message_row.height();
        let viewport = self.list_viewport_height.get();
        if row <= 0.0 || viewport <= 0.0 {
            return;
        }
        // Offsets run negative as the list scrolls down.
        let top = -f32::from(self.list_scroll_handle.0.borrow().base_handle.offset().y);
        let row_top = index as f32 * row;
        let row_bottom = row_top + row;
        if row_top < top {
            self.list_scroll_handle
                .scroll_to_item(index, gpui::ScrollStrategy::Top);
        } else if row_bottom > top + viewport {
            self.list_scroll_handle
                .scroll_to_item(index, gpui::ScrollStrategy::Bottom);
        }
    }

    /// Asks for the body of the row *below* the one just selected and throws
    /// the answer away. The point is the daemon's side effect: fetching stores
    /// the body, so the next row needs no IMAP round trip.
    ///
    /// **One row only.** The daemon answers a client one request at a time, so a
    /// deeper read-ahead queues in front of whatever the reader does next and
    /// starts costing exactly what it is meant to save.
    fn prefetch_next_body(&self, message_id: MessageId, cx: &mut Context<Self>) {
        let Some(index) = self.messages.iter().position(|m| m.id == message_id) else {
            return;
        };
        let Some(next) = self.messages.get(index + 1) else {
            return;
        };
        if next.body_fetched {
            return;
        }
        let next_id = next.id;
        let service = self.service.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = service.body(next_id);
            })
            .detach();
    }

    pub fn select_message(&mut self, message_id: MessageId, cx: &mut Context<Self>) {
        self.selected_message = Some(message_id);
        self.set_selected_html(None);
        self.selected_html_pending = false;
        let Some(msg) = self
            .visible_messages()
            .iter()
            .find(|m| m.id == message_id)
            .cloned()
        else {
            return;
        };

        let needs_mark_read = !msg.flags.seen;

        // Everything below is off the main thread. Awaited inline, these socket
        // round trips blocked the window on the daemon, and since the client
        // serialises queries on one connection a reader moving faster than the
        // daemon answers queued the whole backlog onto the frame loop.
        self.selected_attachments.clear();
        self.selected_attachments_loading = msg.has_attachments;
        self.subject_selection.clear();
        self.copied_address = None;
        self.header_expanded = false;
        self.subject_menu = None;
        // `has_attachments` is derived when the body is stored, so an unfetched
        // message reports `false` however many files it carries. The open below
        // re-runs this once the body lands.
        if msg.has_attachments {
            self.load_attachments(message_id, cx);
        }

        let cached = self.html_document_cache.get(&message_id).cloned();
        if let Some(document) = cached.clone() {
            // The document is already prepared, sanitized and embedded. Going
            // through the store buys a "Loading message..." frame and nothing.
            self.selected_body = None;
            self.selected_body_loading = false;
            self.selected_html_pending = false;
            self.set_selected_html(Some(document));
            cx.notify();
        } else {
            self.selected_body = None;
            self.selected_body_loading = true;
            // From here, not from when the body arrives: the pane paints nothing
            // while this is set, so the plaintext never gets a frame of its own.
            self.selected_html_pending = true;
            self.set_selected_html(None);
            cx.notify();
        }

        let service = self.service.clone();
        if cached.is_none() {
            cx.spawn(async move |this, cx| {
                let body = cx
                    .background_spawn(async move { service.body(message_id).ok().flatten() })
                    .await;
                let _ = this.update(cx, |state, cx| {
                    // The reader has moved on; this answer is about a message no
                    // longer on screen.
                    if state.selected_message != Some(message_id) {
                        return;
                    }
                    let have_body = body
                        .as_ref()
                        .is_some_and(|b| b.text.is_some() || b.html.is_some());
                    state.selected_body = body.as_ref().and_then(|b| b.text.clone());
                    state.selected_body_loading = !have_body;
                    if let Some(birdman_proto::MessageBody {
                        html: Some(html), ..
                    }) = body
                    {
                        // Before preparing, so the pane waits rather than painting
                        // the plaintext underneath.
                        state.selected_html_pending = true;
                        state.prepare_html_body(message_id, html, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        // Decided from the row, not a store read: a body only in a sibling copy
        // costs one wasted fetch, and the alternative is waiting on the socket
        // before deciding, which is the thing being removed.
        let needs_body = !msg.body_fetched && cached.is_none();
        cx.notify();
        self.prefetch_next_body(message_id, cx);

        if !needs_body && !needs_mark_read {
            return;
        }

        if needs_mark_read {
            // Applied now, not when the daemon answers: waiting a round trip to
            // clear the unread dot reads as the keypress not registering.
            // Reverted below if the command fails.
            self.set_seen_locally(message_id, true);
            // The daemon will announce this; see `absorbed_own_change`.
            self.self_changed_folder = self.folder_of_message(message_id);
        }
        let Some(account) = self.account_of_message(message_id) else {
            return;
        };
        let service = self.service.clone();
        let in_flight = self.opens_in_flight.clone();
        let queued = in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let issued = std::time::Instant::now();
        log::debug!(
            "open {} (body {needs_body}, mark_read {needs_mark_read}), {queued} in flight",
            message_id.0
        );
        cx.spawn(async move |this, cx| {
            // Bounded: the daemon answers well under a second even off the
            // server, so anything past the timeout is stuck rather than slow.
            let open = service.execute(
                account,
                birdman_backend::Command::OpenMessage {
                    message: message_id,
                    fetch_body: needs_body,
                    mark_read: needs_mark_read,
                },
            );
            let result = match with_timeout(open, OPEN_TIMEOUT, cx).await {
                Some(result) => result,
                None => Err(birdman_client::ClientError::Transport(format!(
                    "no answer from birdmand within {}s",
                    OPEN_TIMEOUT.as_secs()
                ))),
            };

            // From the store, not the command's return: the backend's job is to
            // make the store correct, and everything else reads from there.
            let body = match (&result, needs_body) {
                (Ok(_), true) => service
                    .body(message_id)
                    .ok()
                    .flatten()
                    .map(|body| (body.text, body.html)),
                _ => None,
            };

            let waited = issued.elapsed();
            let left = in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) - 1;
            // Same rule as `Timed`: debug only, `warn` so it stands out.
            if birdman_config::logging::instrumented() {
                if waited >= SLOW_OPEN {
                    log::warn!(
                        "open {} took {}ms ({left} still in flight)",
                        message_id.0,
                        waited.as_millis()
                    );
                } else {
                    log::debug!(
                        "open {} took {}ms ({left} left)",
                        message_id.0,
                        waited.as_millis()
                    );
                }
            }
            let _ = this.update(cx, |state, cx| {
                if state.selected_message != Some(message_id) {
                    return;
                }
                state.selected_body_loading = false;
                if let Err(err) = &result {
                    // Otherwise indistinguishable from a genuinely empty message.
                    if needs_mark_read {
                        // Put the dot back rather than leave the list claiming
                        // something the server never agreed to.
                        state.set_seen_locally(message_id, false);
                    }
                    state.notify_failure(
                        format!("Open failed: {}", short_error(&err.to_string())),
                        cx,
                    );
                    cx.notify();
                    return;
                }
                if needs_body {
                    // The body is stored, so `has_attachments` is now meaningful.
                    state.load_attachments(message_id, cx);
                }
                if !needs_body && state.selected_attachments.is_empty() {
                    state.selected_attachments_loading = false;
                }
                if let Some((text, html)) = body {
                    state.selected_body = text;
                    if let Some(html) = html {
                        state.selected_html_pending = true;
                        state.prepare_html_body(message_id, html, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn reply(&self, reply_all: bool, cx: &mut Context<Self>) {
        let Some(msg) = self.selected_message().cloned() else {
            return;
        };
        let parsed = birdman_backend::parsed_from_summary(&msg, self.selected_body.clone());
        let draft = birdman_backend::reply_draft(&parsed, &self.self_address(), reply_all);
        self.open_compose(draft, cx);
    }

    pub fn forward(&self, cx: &mut Context<Self>) {
        let Some(msg) = self.selected_message().cloned() else {
            return;
        };
        let parsed = birdman_backend::parsed_from_summary(&msg, self.selected_body.clone());
        let draft = birdman_backend::forward_draft(&parsed);
        self.open_compose(draft, cx);
    }

    pub fn compose_new(&self, cx: &mut Context<Self>) {
        self.open_compose(ComposeDraft::default(), cx);
    }

    fn selected_message(&self) -> Option<&MessageSummary> {
        let id = self.selected_message?;
        self.visible_messages().iter().find(|m| m.id == id)
    }

    fn open_compose(&self, draft: ComposeDraft, cx: &mut App) {
        let options: Vec<_> = self
            .accounts
            .iter()
            .map(|a| crate::compose::SendAs {
                account: a.id,
                display_name: a.display_name.clone(),
                name: a.name.clone(),
                email: a.email.clone(),
            })
            .collect();
        // Replying from the mailbox you were reading is almost always meant.
        let from_index = self
            .active_account()
            .and_then(|active| self.accounts.iter().position(|a| a.id == active.id))
            .unwrap_or(0);
        ComposeView::open(cx, draft, options, from_index, self.service.clone());
    }

    /// Resolves inline `cid:` images to `data:` URIs, which must happen before
    /// the HTML reaches the webview: it resolves a `cid:` against nothing, so an
    /// unrewritten reference simply does not render.
    fn prepare_html_body(&mut self, message_id: MessageId, html: String, cx: &mut Context<Self>) {
        // From the cache when already prepared: the webview gets it next frame.
        if let Some(document) = self.html_document_cache.get(&message_id) {
            self.set_selected_html(Some(document.clone()));
            self.selected_html_pending = false;
            cx.notify();
            return;
        }

        let service = self.service.clone();
        let load_remote_images =
            self.appearance.remote_images == crate::config::RemoteImages::Always;
        // Off-thread: embedding base64-encodes every inline image and
        // sanitizing parses the whole document.
        let prepared = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            // Here, not in the caller: another socket round trip, and the point
            // of this task is that the window does not wait on one.
            let inline_images = service
                .inline_attachments(message_id)
                .unwrap_or_default()
                .into_iter()
                .map(|a| crate::webview::InlineImage {
                    content_id: a.content_id,
                    content_type: a.content_type,
                    cached_path: a.cached_path.into(),
                })
                .collect::<Vec<_>>();
            let document =
                crate::webview::prepare_document(&html, &inline_images, load_remote_images);
            (document, started.elapsed())
        });
        cx.spawn(async move |this, cx| {
            let (document, took) = prepared.await;
            let _ = this.update(cx, |state, cx| {
                // Applying it now would show one message's body under another's
                // header.
                if state.selected_message != Some(message_id) {
                    return;
                }
                if (state.appearance.remote_images == crate::config::RemoteImages::Always)
                    != load_remote_images
                {
                    return;
                }
                if took > SLOW_BODY_PREPARE {
                    log::info!(
                        "slow body prepare: {}ms for {} bytes",
                        took.as_millis(),
                        document.len()
                    );
                }
                state.cache_html_document(message_id, document.clone());
                state.set_selected_html(Some(document));
                state.selected_html_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn visible_messages(&self) -> &[MessageSummary] {
        self.search_results.as_deref().unwrap_or(&self.messages)
    }

    pub fn search_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key.as_str() == "escape" {
            self.close_search(window, cx);
            return;
        }
        let clipboard_text = if crate::text_input::is_paste_keystroke(event) {
            cx.read_from_clipboard().and_then(|item| item.text())
        } else {
            None
        };
        let before = self.search_query.len();
        let outcome = crate::text_input::try_common_edit_key(
            &mut self.search_query,
            &mut self.search_cursor,
            &mut self.search_anchor,
            event,
            clipboard_text.as_deref(),
        );
        if let crate::text_input::Edit::Copied(text) = &outcome {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        }
        if outcome.handled() {
            // **Lengths only, never characters**: this file goes to disk, and a
            // probe logging what someone types into a mail search is a
            // keylogger. Length still separates the two causes of a doubled
            // character: two log lines means two dispatches, one line growing by
            // two means one event inserting twice.
            log::debug!(
                "SEARCHKEY len {before} -> {} held={} key_len={}",
                self.search_query.len(),
                event.is_held,
                event.keystroke.key.len()
            );
            self.run_search(cx);
        }
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        if self.search_query.trim().is_empty() {
            self.search_results = None;
        } else {
            let service = self.service.clone();
            // FTS5 syntax errors degrade to "no results" rather than surfacing a
            // query-language error.
            self.search_results = service
                .search(self.search_query.clone(), self.filter, 100)
                .ok();
        }
        cx.notify();
    }

    /// Opens search, or closes it if already open. Escape closes it too.
    pub fn toggle_search(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.search_expanded {
            self.close_search(window, cx);
        } else {
            self.open_search(window, cx);
        }
    }

    pub fn open_search(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.search_expanded = true;
        self.search_active = true;
        self.search_cursor = self.search_query.len();
        self.search_anchor = None;
        self.search_focus_handle.focus(window);
        cx.notify();
    }

    /// Prefer this over bare [`AppState::clear_search`] anywhere the user can
    /// trigger it, or focus is left on a hidden element -- see
    /// `root_focus_handle`.
    pub fn close_search(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.clear_search(cx);
        self.focus_main(window, cx);
    }

    pub fn focus_main(&self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(handle) = &self.root_focus_handle {
            handle.focus(window);
            cx.notify();
        }
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search_expanded = false;
        self.search_active = false;
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_anchor = None;
        self.search_results = None;
        cx.notify();
    }

    pub fn toggle_flag_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(message_id) = self.selected_message {
            self.toggle_flag(message_id, cx);
        }
    }

    pub fn toggle_flag(&mut self, message_id: MessageId, cx: &mut Context<Self>) {
        let Some(msg) = self
            .visible_messages()
            .iter()
            .find(|m| m.id == message_id)
            .cloned()
        else {
            return;
        };
        let mut target_flags = msg.flags;
        target_flags.flagged = !target_flags.flagged;

        // Optimistic: a failure surfaces on the status line and the next sync
        // corrects the flag, which beats a visibly laggy toggle.
        for list in [Some(&mut self.messages), self.search_results.as_mut()]
            .into_iter()
            .flatten()
        {
            if let Some(m) = list.iter_mut().find(|m| m.id == message_id) {
                m.flags.flagged = target_flags.flagged;
            }
        }
        cx.notify();

        let Some(account) = self.account_of_message(message_id) else {
            return;
        };
        self.dispatch(
            account,
            birdman_backend::Command::SetFlags {
                message: message_id,
                flags: target_flags,
            },
            cx,
            move |state, cx| {
                // A membership change for Flagged: on Gmail `\Flagged` *is* the
                // Starred label.
                let flagged = state
                    .special_folder(account, birdman_store::SpecialUse::Flagged)
                    .map(|f| f.id);
                state.resync_folders(flagged.into_iter().collect(), cx);
            },
        );
    }

    /// Scoped to the *message's* account, not the sidebar's: in the merged view
    /// the list spans accounts, and moving mail between two servers would be a
    /// copy and a delete, which this does not do.
    pub fn move_targets(&self) -> Vec<&Folder> {
        let Some(message) = self.selected_message else {
            return Vec::new();
        };
        let Some(current) = self
            .visible_messages()
            .iter()
            .find(|m| m.id == message)
            .map(|m| m.folder_id)
        else {
            return Vec::new();
        };
        let Some(account) = self.account_of_folder(current) else {
            return Vec::new();
        };
        self.folders
            .iter()
            .filter(|f| f.account_id == account && f.id != current)
            .collect()
    }

    /// Matches the display name *and* the full path, so both "Trash" and
    /// "[Gmail]/Tr" find the same folder.
    pub fn filtered_move_targets(&self) -> Vec<&Folder> {
        self.move_targets()
            .into_iter()
            .filter(|f| {
                self.move_picker
                    .matches([sidebar_folder_name(f).as_str(), f.imap_path.as_str()])
            })
            .collect()
    }

    pub fn move_picker_target(&self) -> Option<FolderId> {
        self.filtered_move_targets()
            .get(self.move_picker.index)
            .map(|f| f.id)
    }

    pub fn set_move_picker(&mut self, open: bool, cx: &mut Context<Self>) {
        self.move_picker_open = open && self.selected_message.is_some();
        // Reopened clean: last message's filter is never what this one wants.
        self.move_picker.reset();
        cx.notify();
    }

    /// Both the app and the daemon write this file, which is what makes it
    /// worth showing: a sync failure happens in the daemon.
    pub fn set_logs(&mut self, open: bool, cx: &mut Context<Self>) {
        self.logs_open = open;
        if open {
            self.log_lines = read_log_tail(LOG_TAIL_LINES);
        } else {
            // A stale copy of a log is worse than no copy.
            self.log_lines.clear();
        }
        cx.notify();
    }

    /// The daemon builds each account's auth adapter at startup, so a credential
    /// added afterwards is never picked up and sync fails forever against a
    /// password sitting right there. Restarting is the only fix from the window.
    pub fn restart_daemon(&mut self, cx: &mut Context<Self>) {
        self.notify_user("Restarting birdmand...", cx);
        cx.notify();

        let client = self.service.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { client.restart_daemon() })
                .await;
            let _ = this.update(cx, |state, cx| {
                match outcome {
                    Ok(()) => {
                        log::info!("birdmand restarted");
                        state.notify_user("Restarted birdmand", cx);
                        // The event pump resubscribes itself, but only once the
                        // old stream ends.
                        state.refresh_folders(cx);
                        state.refresh_messages(cx);
                        state.refresh_sync_status(cx);
                    }
                    Err(err) => {
                        log::error!("could not restart birdmand: {err}");
                        state.notify_failure(format!("Restart failed: {err}"), cx);
                    }
                }
                // The interesting lines are the daemon's, going down and up.
                if state.logs_open {
                    state.log_lines = read_log_tail(LOG_TAIL_LINES);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Lists the attachments, then makes the copies -- two stages, because the
    /// names and sizes are one cheap read and the copies are a file each. In one
    /// request the header stayed empty for the whole of it, and the *body* read
    /// queued behind the copying on the shared connection.
    fn load_attachments(&mut self, message_id: MessageId, cx: &mut Context<Self>) {
        let service = self.service.clone();
        cx.spawn(async move |this, cx| {
            let listed = {
                let service = service.clone();
                cx.background_spawn(
                    async move { service.attachments(message_id).unwrap_or_default() },
                )
                .await
            };
            if listed.is_empty() {
                let _ = this.update(cx, |state, cx| {
                    if state.selected_message == Some(message_id) {
                        state.selected_attachments_loading = false;
                        cx.notify();
                    }
                });
                return;
            }
            let already_there = listed.iter().all(|a| a.path.is_some());
            let _ = this.update(cx, |state, cx| {
                if state.selected_message == Some(message_id) {
                    state.selected_attachments = listed;
                    state.selected_attachments_loading = false;
                    cx.notify();
                }
            });
            if already_there {
                return;
            }

            let ready = service.materialise_attachments(message_id).await;
            let _ = this.update(cx, |state, cx| {
                if state.selected_message == Some(message_id) {
                    match ready {
                        Ok(ready) => state.selected_attachments = ready,
                        Err(err) => {
                            log::warn!("could not prepare attachments: {err}");
                            state.notify_failure(
                                format!(
                                    "Attachments unavailable: {}",
                                    short_error(&err.to_string())
                                ),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The pane's body is a native webview composited **above** every gpui
    /// layer, so anything drawn there is invisible until it is hidden.
    ///
    /// One function, asked by both the code that hides the webview and the code
    /// that decides what to paint underneath -- kept as two hand-maintained
    /// lists it drifted every time an overlay was added, each shipping invisible.
    pub fn overlay_covers_reading_pane(&self) -> bool {
        self.palette_open || self.move_picker_open || self.logs_open
    }

    pub fn selected_subject(&self) -> String {
        self.selected_message
            .and_then(|id| self.visible_messages().iter().find(|m| m.id == id).cloned())
            .and_then(|m| m.subject)
            .unwrap_or_else(|| "(no subject)".to_string())
    }

    pub fn notify_user(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.push_notification(text.into(), false, cx);
    }

    pub fn notify_failure(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.push_notification(text.into(), true, cx);
    }

    fn push_notification(&mut self, text: String, failed: bool, cx: &mut Context<Self>) {
        let id = self.next_notification;
        self.next_notification += 1;
        self.notifications.push(Notification { id, text, failed });
        cx.notify();

        // Each dismisses itself: one timer for the stack would cut a
        // notification short whenever another arrived behind it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTIFICATION_TTL).await;
            let _ = this.update(cx, |state, cx| state.dismiss_notification(id, cx));
        })
        .detach();
    }

    pub fn dismiss_notification(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.notifications.len();
        self.notifications.retain(|n| n.id != id);
        if self.notifications.len() != before {
            cx.notify();
        }
    }

    /// Clamped above the reading pane's body: a menu dropping into it would be
    /// behind the native webview and simply not there.
    pub fn open_subject_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.subject_selection.selected_text().is_none() {
            return;
        }
        let body_top = self.reading_pane_rect.get().1;
        let highest = (body_top - SUBJECT_MENU_HEIGHT - 4.0).max(0.0);
        let mut position = position;
        position.y = gpui::px(f32::from(position.y).min(highest));
        self.subject_menu = Some(position);
        cx.notify();
    }

    pub fn close_subject_menu(&mut self, cx: &mut Context<Self>) {
        if self.subject_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Returns whether it took the keystroke, so `Cmd+C` falls through to its
    /// other meanings when nothing is selected.
    pub fn copy_header_selection(&mut self, cx: &mut Context<Self>) -> bool {
        // Only the subject is selectable; the address copies itself on click.
        let Some(text) = self.subject_selection.selected_text() else {
            return false;
        };
        self.subject_menu = None;
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.notify_user("Copied", cx);
        cx.notify();
        true
    }

    pub fn copy_address(&mut self, address: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(address.clone()));

        // On the chip rather than in a notification: the confirmation belongs
        // where the action was.
        self.copied_address = Some(address.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPY_CONFIRMATION).await;
            let _ = this.update(cx, |state, cx| {
                // Or a second copy is cut short by the first one's timer.
                if state.copied_address.as_deref() == Some(address.as_str()) {
                    state.copied_address = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn toggle_header_expanded(&mut self, cx: &mut Context<Self>) {
        self.header_expanded = !self.header_expanded;
        cx.notify();
    }

    /// `Reply-To` only when it differs from `From`, which is exactly when it
    /// matters: it is where Reply will go.
    pub fn address_rows(&self) -> Vec<(&'static str, Vec<birdman_mime::Mailbox>)> {
        let Some(msg) = self.selected_message().cloned() else {
            return Vec::new();
        };
        let from: Vec<birdman_mime::Mailbox> = msg
            .from_addr
            .clone()
            .map(|address| birdman_mime::Mailbox {
                name: msg.from_name.clone(),
                address,
            })
            .into_iter()
            .collect();
        let reply_to = birdman_backend::split_addrs(msg.reply_to_addrs.as_deref());
        let differs = reply_to.iter().any(|r| {
            !from
                .iter()
                .any(|f| f.address.eq_ignore_ascii_case(&r.address))
        });

        [
            ("From", from),
            ("Reply-To", if differs { reply_to } else { Vec::new() }),
            ("To", birdman_backend::split_addrs(msg.to_addrs.as_deref())),
            ("Cc", birdman_backend::split_addrs(msg.cc_addrs.as_deref())),
            (
                "Bcc",
                birdman_backend::split_addrs(msg.bcc_addrs.as_deref()),
            ),
        ]
        .into_iter()
        .filter(|(_, addresses)| !addresses.is_empty())
        .collect()
    }

    pub fn compose_to(&mut self, address: String, name: Option<String>, cx: &mut Context<Self>) {
        self.open_compose(
            birdman_backend::ComposeDraft {
                to: vec![birdman_backend::Recipient::new(name, address)],
                ..Default::default()
            },
            cx,
        );
        cx.notify();
    }

    /// Both lists: a message can be on screen through the folder list and
    /// through search results at once, and updating one leaves the other lying.
    fn set_seen_locally(&mut self, message_id: MessageId, seen: bool) {
        let mut changed_in = None;
        for list in [Some(&mut self.messages), self.search_results.as_mut()]
            .into_iter()
            .flatten()
        {
            if let Some(m) = list.iter_mut().find(|m| m.id == message_id) {
                // Re-marking a read message read must not decrement anything.
                if m.flags.seen != seen {
                    changed_in = Some(m.folder_id);
                }
                m.flags.seen = seen;
            }
        }

        // The counts move with the flag. Once the daemon's announcement was
        // suppressed as our own echo, nothing else updated them: the dot cleared
        // while the header still said "766 unread".
        let Some(folder) = changed_in else { return };
        let delta: i64 = if seen { -1 } else { 1 };
        if let Some(count) = self.folder_unread.get_mut(&folder) {
            *count = count.saturating_add_signed(delta as i32);
        } else if !seen {
            self.folder_unread.insert(folder, 1);
        }
        let in_view = self.selected_folder_ids_contain(folder);
        if let Some((_, unread)) = self.selected_folder_counts.as_mut() {
            if in_view {
                *unread = unread.saturating_add_signed(delta as i32);
            }
        }
    }

    /// The header's counts cover the selected folder *and its descendants*.
    fn selected_folder_ids_contain(&self, folder: FolderId) -> bool {
        self.selected_folder
            .map(|selected| self.selected_folder_ids(selected).contains(&folder))
            .unwrap_or(false)
    }

    fn folder_of_message(&self, message_id: MessageId) -> Option<FolderId> {
        self.visible_messages()
            .iter()
            .find(|m| m.id == message_id)
            .map(|m| m.folder_id)
    }

    /// Acting on our own echo is what made arrowing through unread mail lurch:
    /// every keypress marked a message read, every mark published an event, and
    /// every event rebuilt the whole list to learn a flag already set.
    ///
    /// Consumed on read, so only the one expected event is skipped.
    pub fn absorbed_own_change(&mut self, folder: FolderId) -> bool {
        if self.self_changed_folder == Some(folder) {
            self.self_changed_folder = None;
            return true;
        }
        false
    }

    pub fn dark_mode_for_selected(&self) -> crate::config::EmailDarkMode {
        match self
            .selected_message
            .and_then(|id| self.dark_override.get(&id))
        {
            Some(true) => crate::config::EmailDarkMode::Always,
            Some(false) => crate::config::EmailDarkMode::Never,
            None => self.appearance.email_dark_mode,
        }
    }

    /// One setter rather than two assignments: the dark flag is only correct if
    /// recomputed every time the source changes, and a bare field makes
    /// forgetting easy and silent.
    fn set_selected_html(&mut self, document: Option<String>) {
        let showing = document.is_some();
        self.selected_supports_dark = document
            .as_deref()
            .is_some_and(crate::webview::supports_dark_mode);
        self.selected_html_source = document;
        if showing {
            // Only from a document actually being shown: clearing the source for
            // a load resets `selected_supports_dark`, so asking mid-wait answers
            // about no document at all.
            self.last_document_background = Some(crate::webview::document_background(
                self.selected_rendering(),
            ));
        }
    }

    pub fn selected_rendering(&self) -> crate::webview::Rendering {
        crate::webview::rendering_from(self.dark_mode_for_selected(), self.selected_supports_dark)
    }

    /// Asks the same function the reading pane does rather than re-deriving it:
    /// under `Auto` the answer depends on the document, not just the config.
    pub fn selected_is_darkened(&self) -> bool {
        self.selected_rendering() == crate::webview::Rendering::ForceDark
    }

    /// Keyed off what is *currently on screen*, not off whether an override
    /// exists, so the button always changes something.
    pub fn toggle_dark_mode(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_message else {
            return;
        };
        let wanted = !self.selected_is_darkened();
        self.dark_override.insert(id, wanted);
        cx.notify();
    }

    pub fn set_palette(&mut self, open: bool, cx: &mut Context<Self>) {
        self.palette_open = open;
        self.palette.reset();
        cx.notify();
    }

    pub fn palette_matches(&self) -> Vec<&'static crate::palette::PaletteCommand> {
        // These all act on *the* message, so with none selected they are a list
        // of things that will not happen.
        let has_message = self.selected_message.is_some();
        crate::palette::COMMANDS
            .iter()
            .filter(|c| has_message || c.group.section() != crate::palette::Section::Message)
            .filter(|c| self.palette.matches([c.name, c.aliases]))
            .collect()
    }

    /// Same classification as every other picker; only `Confirm` differs.
    pub fn palette_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        use crate::text_input::PickerKey;
        let key = crate::text_input::classify_picker_key(event);
        match key {
            PickerKey::Dismiss => self.set_palette(false, cx),
            PickerKey::Previous | PickerKey::Next => {
                let delta = if matches!(key, PickerKey::Previous) {
                    -1
                } else {
                    1
                };
                let len = self.palette_matches().len();
                self.palette.step(delta, len);
                self.palette_scroll.scroll_to_item(self.palette.index);
                cx.notify();
            }
            PickerKey::Confirm => {
                let chosen = self.palette_matches().get(self.palette.index).copied();
                // Closed *before* running: several commands open an overlay.
                self.set_palette(false, cx);
                if let Some(command) = chosen {
                    (command.run)(self, window, cx);
                }
            }
            PickerKey::Insert(_) | PickerKey::Backspace => {
                self.palette.edit(&key);
                // Filtering resets the highlight, so the viewport must follow.
                self.palette_scroll.scroll_to_item(0);
                cx.notify();
            }
            // Tab jumps to the next *section*, not the next item.
            PickerKey::Ignored if event.keystroke.key.as_str() == "tab" => {
                self.palette_next_section(cx);
            }
            PickerKey::Ignored => {}
        }
    }

    fn palette_next_section(&mut self, cx: &mut Context<Self>) {
        let matches = self.palette_matches();
        let Some(current) = matches.get(self.palette.index).map(|c| c.group.section()) else {
            return;
        };
        let next = matches
            .iter()
            .position(|c| c.group.section() != current)
            .or_else(|| (!matches.is_empty()).then_some(0));
        if let Some(next) = next {
            self.palette.index = next;
            self.palette_scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// The classification is shared; only the effects are specific.
    pub fn move_picker_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        use crate::text_input::PickerKey;
        let key = crate::text_input::classify_picker_key(event);
        match key {
            PickerKey::Dismiss => self.set_move_picker(false, cx),
            PickerKey::Previous | PickerKey::Next => {
                let delta = if matches!(key, PickerKey::Previous) {
                    -1
                } else {
                    1
                };
                let len = self.filtered_move_targets().len();
                self.move_picker.step(delta, len);
                cx.notify();
            }
            PickerKey::Confirm => {
                if let Some(target) = self.move_picker_target() {
                    self.move_selected_to_folder(target, cx);
                }
            }
            PickerKey::Insert(_) | PickerKey::Backspace => {
                self.move_picker.edit(&key);
                cx.notify();
            }
            PickerKey::Ignored => {}
        }
    }

    pub fn move_selected_to_folder(&mut self, target: FolderId, cx: &mut Context<Self>) {
        self.move_picker_open = false;
        self.move_picker.reset();
        self.move_selected_to(target, cx);
    }

    /// `\Archive` when the server has one, else `\All` -- Gmail exposes no
    /// `\Archive`, and archiving there is a move into All Mail.
    pub fn archive_selected(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self
            .selected_message
            .and_then(|id| self.account_of_message(id))
        else {
            return;
        };
        let Some(target) = self.archive_folder(account).map(|f| f.id) else {
            self.notify_failure("No archive folder on this account", cx);
            cx.notify();
            return;
        };
        self.move_selected_to(target, cx);
    }

    fn archive_folder(&self, account: birdman_store::AccountId) -> Option<&birdman_store::Folder> {
        self.special_folder(account, birdman_store::SpecialUse::Archive)
            .or_else(|| self.special_folder(account, birdman_store::SpecialUse::All))
    }

    /// Drops the message from the visible list immediately, then does the IMAP
    /// work in the background and surfaces only a failure.
    fn move_selected_to(&mut self, target: FolderId, cx: &mut Context<Self>) {
        let Some(message_id) = self.selected_message else {
            return;
        };
        let Some(msg) = self
            .visible_messages()
            .iter()
            .find(|m| m.id == message_id)
            .cloned()
        else {
            return;
        };
        if msg.folder_id == target {
            self.notify_user("Already there", cx);
            cx.notify();
            return;
        }
        let source = msg.folder_id;
        let Some(account) = self.account_of_folder(source) else {
            return;
        };
        self.forget_selected_message(cx);

        self.dispatch(
            account,
            birdman_backend::Command::MoveMessage {
                message: message_id,
                to_folder: target,
            },
            cx,
            move |state, cx| {
                // Both ends: neither folder learns this on its own.
                state.resync_folders(vec![source, target], cx);
            },
        );
    }

    /// Shared by delete, archive and move, so all three keep the list's place.
    /// The next message *down* inherits the selection, falling back to the one
    /// above on the last row.
    fn forget_selected_message(&mut self, cx: &mut Context<Self>) {
        let Some(message_id) = self.selected_message else {
            return;
        };

        // Before the removal, while the neighbours are still there.
        let successor = {
            let visible = self.visible_messages();
            visible
                .iter()
                .position(|m| m.id == message_id)
                .and_then(|at| {
                    visible
                        .get(at + 1)
                        .or_else(|| at.checked_sub(1).and_then(|p| visible.get(p)))
                })
                .map(|m| m.id)
        };

        self.messages.retain(|m| m.id != message_id);
        if let Some(results) = &mut self.search_results {
            results.retain(|m| m.id != message_id);
        }
        self.selected_message = None;
        self.selected_body = None;
        self.set_selected_html(None);
        self.selected_html_pending = false;

        match successor {
            Some(next) => self.select_message(next, cx),
            None => cx.notify(),
        }
    }

    pub fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(message_id) = self.selected_message else {
            return;
        };
        let Some(account) = self.account_of_message(message_id) else {
            return;
        };
        self.forget_selected_message(cx);
        self.dispatch(
            account,
            birdman_backend::Command::DeleteMessage {
                message: message_id,
            },
            cx,
            move |state, cx| {
                // The message lands in Trash, which needs to hear about it.
                let trash = state
                    .special_folder(account, birdman_store::SpecialUse::Trash)
                    .map(|f| f.id);
                state.resync_folders(trash.into_iter().collect(), cx);
            },
        );
    }
}

/// Enough for a sync attempt and its failure, without reading 2MB into memory.
const LOG_TAIL_LINES: usize = 400;

/// Well under the ~33ms of a held key's repeat rate, so holding an arrow down
/// does not trip it.
const REPEAT_ARROW_WINDOW: std::time::Duration = std::time::Duration::from_millis(20);

const SLOW_OPEN: std::time::Duration = std::time::Duration::from_millis(400);

/// `Root::render`'s guard catches the stall either way; this attributes it.
fn on_main<T>(what: &'static str, work: impl FnOnce() -> T) -> T {
    let _timed = birdman_config::logging::Timed::new(what, birdman_config::logging::Timed::FRAME);
    work()
}

const OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// gpui's executor has a timer but no combinator for this.
async fn with_timeout<T>(
    future: impl std::future::Future<Output = T>,
    limit: std::time::Duration,
    cx: &mut gpui::AsyncApp,
) -> Option<T> {
    let timer = cx.background_executor().timer(limit);
    futures::pin_mut!(future);
    match futures::future::select(future, timer).await {
        futures::future::Either::Left((value, _)) => Some(value),
        futures::future::Either::Right(_) => None,
    }
}

fn read_log_tail(count: usize) -> Vec<String> {
    let path = birdman_config::data_dir().join("birdman.log");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return vec![format!("could not read {}", path.display())];
    };
    let lines: Vec<&str> = contents.lines().collect();
    // Newest first: file order puts the line worth reading at the bottom of a
    // container that opens at the top.
    lines[lines.len().saturating_sub(count)..]
        .iter()
        .rev()
        .map(|l| l.to_string())
        .collect()
}

/// Rows fetched per page. A **UI** limit, not a store one: keyset pagination
/// makes page 2 cost the same as page 1 however deep the mailbox goes, and
/// `load_more_messages` appends pages as the list scrolls.
const MESSAGE_PAGE_LIMIT: u32 = 200;

/// A short page means the end. An exactly-full one might still be the last, and
/// the next fetch settles it -- one wasted indexed query, against a `COUNT` on
/// every page to avoid it.
fn next_cursor(page: &[MessageSummary]) -> Option<birdman_store::PageCursor> {
    next_cursor_after(page, MESSAGE_PAGE_LIMIT)
}

fn next_cursor_after(page: &[MessageSummary], requested: u32) -> Option<birdman_store::PageCursor> {
    if page.len() < requested as usize {
        return None;
    }
    page.last().map(|last| birdman_store::PageCursor {
        // `date` is nullable on the row but not in the cursor. Undated mail
        // sorts last under `ORDER BY date DESC`, and 0 keeps the keyset walking
        // in the same direction.
        date: last.date.unwrap_or(0),
        id: last.id,
    })
}

/// Ceiling on how much a refresh re-reads, so a deep scroll still gets a
/// bounded query per sync event.
const MESSAGE_REFRESH_CAP: u32 = 2_000;

/// How often the config and theme files are checked for changes.
const APPEARANCE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// A missing file contributes `None` rather than being skipped, so *deleting* a
/// theme file is itself a change.
fn appearance_fingerprint() -> Vec<Option<std::time::SystemTime>> {
    crate::config::watched_paths()
        .into_iter()
        .map(|path| {
            std::fs::metadata(&path)
                .ok()
                .and_then(|meta| meta.modified().ok())
        })
        .collect()
}

/// Each entry holds its inline images as base64, so this bounds worst-case
/// memory rather than fitting a browsing pattern.
const HTML_DOCUMENT_CACHE_CAP: usize = 16;

/// Not an error -- a megabyte of inline images takes a moment -- but the first
/// thing to check if opening mail starts feeling slow.
const SLOW_BODY_PREPARE: std::time::Duration = std::time::Duration::from_millis(250);

/// How long a folder stays fresh enough not to re-sync when opened.
const FOLDER_SYNC_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Special-use folders get a canonical name rather than the server's: Gmail
/// serves "[Gmail]/Sent Mail" and a localized account serves localized names
/// entirely, and the sidebar should read the same against any server.
/// User-created folders keep the server's name, which is the only one they have.
///
/// `INBOX` is matched by its reserved path: RFC 6154 defines no `\Inbox`
/// attribute, since servers need not tag the one folder every account has.
pub fn sidebar_folder_name(folder: &birdman_store::Folder) -> String {
    if folder.imap_path.eq_ignore_ascii_case("INBOX") {
        return "Inbox".to_string();
    }
    match folder.special_use {
        Some(birdman_store::SpecialUse::Drafts) => "Drafts".to_string(),
        Some(birdman_store::SpecialUse::Sent) => "Sent".to_string(),
        Some(birdman_store::SpecialUse::Flagged) => "Flagged".to_string(),
        Some(birdman_store::SpecialUse::Junk) => "Junk".to_string(),
        Some(birdman_store::SpecialUse::Trash) => "Trash".to_string(),
        Some(birdman_store::SpecialUse::Archive) => "Archive".to_string(),
        Some(birdman_store::SpecialUse::All) => "All Mail".to_string(),
        None => folder.name.clone(),
    }
}

/// `Websites/paypal` -> `Websites`. By the **first** segment, not the last: a
/// deeply nested `a/b/c` under `a/b` is only a real group if something renders
/// that level too, and otherwise the folder vanishes into a header nothing draws.
///
/// The delimiter comes from the server's `LIST`; `/` is the fallback.
pub fn sidebar_folder_group(folder: &birdman_store::Folder) -> Option<&str> {
    let delimiter = folder.delimiter.as_deref().unwrap_or("/");
    let (head, rest) = folder.imap_path.split_once(delimiter)?;
    (!head.is_empty() && !rest.is_empty()).then_some(head)
}

pub fn sidebar_folder_leaf(folder: &birdman_store::Folder) -> &str {
    let delimiter = folder.delimiter.as_deref().unwrap_or("/");
    folder
        .imap_path
        .split_once(delimiter)
        .map(|(_, rest)| rest)
        .unwrap_or(&folder.imap_path)
}

/// From the same SPECIAL-USE attribute that drives the name and position, so
/// icon and label cannot disagree.
pub fn sidebar_folder_icon(folder: &birdman_store::Folder) -> &'static str {
    if folder.imap_path.eq_ignore_ascii_case("INBOX") {
        return "icons/inbox.svg";
    }
    match folder.special_use {
        Some(birdman_store::SpecialUse::Flagged) => "icons/flag.svg",
        Some(birdman_store::SpecialUse::Drafts) => "icons/drafts.svg",
        Some(birdman_store::SpecialUse::Sent) => "icons/sent.svg",
        Some(birdman_store::SpecialUse::Trash) => "icons/trash.svg",
        _ => "icons/folder.svg",
    }
}

/// IMAP failures routinely carry the server's whole response. The full text
/// goes to the log; this is what fits on screen.
pub fn short_error(message: &str) -> String {
    birdman_config::logging::truncate(
        message.lines().next().unwrap_or(message).trim(),
        STATUS_ERROR_CHARS,
    )
}

const STATUS_ERROR_CHARS: usize = 120;
