use std::fs;
use std::path::{Path, PathBuf};

use gpui::FontWeight;
use serde::Deserialize;

use crate::theme::Token;

#[allow(unused_imports)]
pub use birdman_config::{
    config_path, data_dir, load, open_editor, AuthConfig, AuthKind, Config, ConfiguredAccount,
    ReceiverConfig, ReceiverKind, SenderConfig, SenderKind,
};

#[derive(Deserialize)]
struct TomlFile {
    #[serde(default)]
    appearance: TomlAppearance,
    #[serde(default)]
    theme: TomlTheme,
}

#[derive(Deserialize, Default)]
struct TomlAppearance {
    email_dark_mode: Option<String>,
    load_remote_images: Option<String>,
    toolbar_actions: Option<Vec<String>>,
    #[serde(default)]
    message_row: TomlMessageRow,
    #[serde(default)]
    show: TomlShow,
    /// A file holding a `[theme]` table, so the palette can be swapped by
    /// something else (an Omarchy-style `current/theme` symlink). Relative to
    /// the config file's own directory; overrides the inline `[theme]`.
    theme_file: Option<String>,
    reading_max_width: Option<f32>,
    reading_css_file: Option<String>,
}

/// Every field optional: a theme names the colours it cares about.
#[derive(Deserialize, Default, Clone)]
pub struct TomlTheme {
    pub bg_app: Option<String>,
    pub bg_sidebar: Option<String>,
    pub bg_list: Option<String>,
    pub bg_selected: Option<String>,
    pub bg_hover: Option<String>,
    pub bg_unread: Option<String>,
    pub bg_message: Option<String>,
    pub border: Option<String>,
    pub text_primary: Option<String>,
    pub text_secondary: Option<String>,
    pub text_muted: Option<String>,
    pub accent: Option<String>,
    pub danger: Option<String>,
    pub scrollbar_thumb: Option<String>,
    pub scrollbar_thumb_hover: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlMessageRow {
    gutter: Option<Vec<String>>,
    lines: Option<Vec<Vec<String>>>,
    #[serde(default)]
    style: std::collections::HashMap<String, TomlSlotStyle>,
}

#[derive(Deserialize, Default)]
struct TomlSlotStyle {
    size: Option<f32>,
    weight: Option<String>,
    color: Option<String>,
    color_unread: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlShow {
    sidebar: Option<bool>,
    toolbar: Option<bool>,
    message_list_header: Option<bool>,
    scrollbars: Option<bool>,
}

#[derive(Deserialize, Default)]
struct TomlThemeFile {
    #[serde(default)]
    theme: TomlTheme,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EmailDarkMode {
    /// Recolour unless the message has its own `prefers-color-scheme` query.
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RemoteImages {
    Never,
    #[default]
    Always,
}

impl RemoteImages {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "always" | "on" | "true" => Some(Self::Always),
            "never" | "off" | "false" => Some(Self::Never),
            _ => None,
        }
    }
}

impl EmailDarkMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "always" | "on" | "true" => Some(Self::Always),
            "never" | "off" | "false" => Some(Self::Never),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolbarAction {
    Reply,
    ReplyAll,
    Forward,
    Move,
    Flag,
    Archive,
    Delete,
    DarkMode,
    /// Not a button -- a gap pushing everything after it to the trailing edge.
    Spacer,
    /// Not a button either -- a hairline rule between groups.
    Divider,
}

impl ToolbarAction {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "reply" => Some(Self::Reply),
            "reply_all" => Some(Self::ReplyAll),
            "forward" => Some(Self::Forward),
            "move" => Some(Self::Move),
            "flag" => Some(Self::Flag),
            "archive" => Some(Self::Archive),
            "delete" => Some(Self::Delete),
            "dark_mode" | "dark" | "sun" => Some(Self::DarkMode),
            "spacer" | "gap" => Some(Self::Spacer),
            "divider" | "separator" | "rule" => Some(Self::Divider),
            _ => None,
        }
    }

    pub const DEFAULT: &'static [ToolbarAction] = &[
        Self::Reply,
        Self::ReplyAll,
        Self::Forward,
        Self::Spacer,
        Self::DarkMode,
        Self::Move,
        Self::Flag,
        Self::Divider,
        Self::Archive,
        Self::Delete,
    ];
}

/// Bounded by what `birdman_store::MessageSummary` already carries: a slot needing
/// a column the list query does not select would render blank.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum MessageSlot {
    /// In the gutter rather than on a line: it sits beside the whole row.
    UnreadDot,
    Sender,
    Subject,
    Preview,
    Recipients,
    Date,
    Flag,
    Attachment,
    /// Not a field: a gap, as in [`ToolbarAction::Spacer`]. Only needed on a
    /// line with no growing slot to do the pushing.
    Spacer,
}

/// One [`LINE_LEADING`] rather than a per-slot value, so a line's height
/// follows the tallest thing on it.
const LINE_LEADING: f32 = 7.0;

const ROW_PADDING_Y: f32 = 8.0;

const ROW_LINE_GAP: f32 = 4.0;

impl MessageSlot {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "unread_dot" | "unread" | "dot" => Some(Self::UnreadDot),
            "sender" | "from" => Some(Self::Sender),
            "subject" => Some(Self::Subject),
            "preview" | "snippet" => Some(Self::Preview),
            "recipients" | "to" => Some(Self::Recipients),
            "date" | "timestamp" | "time" => Some(Self::Date),
            "flag" | "flagged" | "star" => Some(Self::Flag),
            "attachment" | "attachments" | "paperclip" => Some(Self::Attachment),
            "spacer" | "gap" => Some(Self::Spacer),
            _ => None,
        }
    }

    /// Takes the leftover width, and so truncates rather than pushing its
    /// neighbours off the row. Text grows, annotations must not.
    pub fn grows(self) -> bool {
        matches!(
            self,
            Self::Sender | Self::Subject | Self::Preview | Self::Recipients
        )
    }

    pub fn default_style(self) -> SlotStyle {
        let text = |size: f32, weight: FontWeight| SlotStyle {
            size,
            weight,
            color: Token::TextSecondary,
            color_unread: Token::TextPrimary,
        };
        match self {
            Self::Sender | Self::Recipients => text(14.0, FontWeight::BOLD),
            Self::Subject => text(12.0, FontWeight::NORMAL),
            Self::Preview => SlotStyle {
                size: 12.0,
                weight: FontWeight::NORMAL,
                color: Token::TextMuted,
                color_unread: Token::TextMuted,
            },
            Self::Date => SlotStyle {
                size: 11.0,
                weight: FontWeight::NORMAL,
                color: Token::TextMuted,
                color_unread: Token::TextMuted,
            },
            Self::Flag => SlotStyle {
                size: INLINE_ICON_SIZE,
                weight: FontWeight::NORMAL,
                color: Token::Accent,
                color_unread: Token::Accent,
            },
            Self::Attachment => SlotStyle {
                size: INLINE_ICON_SIZE,
                weight: FontWeight::NORMAL,
                color: Token::TextSecondary,
                color_unread: Token::TextSecondary,
            },
            Self::UnreadDot => SlotStyle {
                size: 8.0,
                weight: FontWeight::NORMAL,
                color: Token::Accent,
                color_unread: Token::Accent,
            },
            Self::Spacer => SlotStyle {
                size: 0.0,
                weight: FontWeight::NORMAL,
                color: Token::Border,
                color_unread: Token::Border,
            },
        }
    }
}

/// Mirrors `root::INLINE_ICON_SIZE`, where it is consumed.
const INLINE_ICON_SIZE: f32 = 13.0;

/// Colours are [`Token`]s rather than hex, so an override stays inside the
/// theme instead of pinning a value the palette can no longer reach.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SlotStyle {
    pub size: f32,
    pub weight: FontWeight,
    pub color: Token,
    /// Colour on an unread one. A property of the slot, so the row never has
    /// to special-case read state.
    pub color_unread: Token,
}

impl SlotStyle {
    pub fn color_for(&self, unread: bool) -> Token {
        if unread {
            self.color_unread
        } else {
            self.color
        }
    }
}

fn parse_weight(value: &str) -> Option<FontWeight> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "normal" | "regular" => Some(FontWeight::NORMAL),
        "medium" => Some(FontWeight::MEDIUM),
        "semibold" | "semi_bold" => Some(FontWeight::SEMIBOLD),
        "bold" => Some(FontWeight::BOLD),
        _ => None,
    }
}

/// A gutter (full-height, beside everything) plus a stack of lines. A flat list
/// of slots could not express the unread dot sitting alongside both lines.
#[derive(Clone, PartialEq, Debug)]
pub struct MessageRow {
    pub gutter: Vec<MessageSlot>,
    pub lines: Vec<Vec<MessageSlot>>,
    overrides: Vec<(MessageSlot, SlotStyle)>,
}

impl Default for MessageRow {
    fn default() -> Self {
        Self {
            gutter: vec![MessageSlot::UnreadDot],
            lines: vec![
                vec![MessageSlot::Sender, MessageSlot::Flag, MessageSlot::Date],
                vec![MessageSlot::Subject, MessageSlot::Attachment],
            ],
            overrides: Vec::new(),
        }
    }
}

impl MessageRow {
    pub fn style(&self, slot: MessageSlot) -> SlotStyle {
        self.overrides
            .iter()
            .find(|(candidate, _)| *candidate == slot)
            .map(|(_, style)| *style)
            .unwrap_or_else(|| slot.default_style())
    }

    /// Derived, never configured. `uniform_list`, the scrollbar geometry and
    /// the infinite-scroll trigger all multiply this by the row count, so a
    /// height disagreeing with what is drawn makes the list scroll wrong.
    pub fn height(&self) -> f32 {
        let lines: f32 = self.lines.iter().map(|line| self.line_height(line)).sum();
        let gaps = ROW_LINE_GAP * self.lines.len().saturating_sub(1) as f32;
        2.0 * ROW_PADDING_Y + lines + gaps
    }

    fn line_height(&self, line: &[MessageSlot]) -> f32 {
        line.iter()
            .map(|slot| self.style(*slot).size)
            .fold(0.0f32, f32::max)
            + LINE_LEADING
    }
}

/// Whether a component is there at all, as opposed to what it is made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Show {
    pub sidebar: bool,
    pub toolbar: bool,
    pub message_list_header: bool,
    pub scrollbars: bool,
}

impl Default for Show {
    fn default() -> Self {
        Self {
            sidebar: true,
            toolbar: true,
            message_list_header: true,
            scrollbars: true,
        }
    }
}

#[derive(Clone)]
pub struct Appearance {
    pub email_dark_mode: EmailDarkMode,
    pub remote_images: RemoteImages,
    /// `0` means no cap. Fixed-width newsletters are unaffected; fluid ones
    /// otherwise stretch to an unreadable line length on a wide window.
    pub reading_max_width: f32,
    /// Held as text, not a path: the pane asks for it on every frame.
    pub reading_css: String,
    pub toolbar_actions: Vec<ToolbarAction>,
    pub message_row: MessageRow,
    pub show: Show,
    pub palette: crate::theme::Palette,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            email_dark_mode: EmailDarkMode::default(),
            remote_images: RemoteImages::default(),
            reading_max_width: 720.0,
            reading_css: String::new(),
            toolbar_actions: ToolbarAction::DEFAULT.to_vec(),
            message_row: MessageRow::default(),
            show: Show::default(),
            palette: crate::theme::Palette::DEFAULT,
        }
    }
}

/// `#rrggbb`, `rrggbb`, or `0xrrggbb`; anything else keeps the default.
fn parse_hex(value: &str) -> Option<u32> {
    let cleaned = value
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x");
    (cleaned.len() == 6)
        .then(|| u32::from_str_radix(cleaned, 16).ok())
        .flatten()
}

/// Never fails: anything unparseable falls back to that one setting's default,
/// rather than refusing to draw the app over a mistyped colour.
pub fn load_appearance() -> Appearance {
    let path = config_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return Appearance::default();
    };
    let Ok(parsed) = toml::from_str::<TomlFile>(&raw) else {
        return Appearance::default();
    };
    appearance_from(&parsed, path.parent())
}

fn appearance_from(parsed: &TomlFile, config_dir: Option<&Path>) -> Appearance {
    let mut appearance = Appearance::default();

    if let Some(mode) = parsed
        .appearance
        .email_dark_mode
        .as_deref()
        .and_then(EmailDarkMode::parse)
    {
        appearance.email_dark_mode = mode;
    }
    if let Some(mode) = parsed
        .appearance
        .load_remote_images
        .as_deref()
        .and_then(RemoteImages::parse)
    {
        appearance.remote_images = mode;
    }
    if let Some(actions) = &parsed.appearance.toolbar_actions {
        let parsed: Vec<_> = actions
            .iter()
            .filter_map(|a| ToolbarAction::parse(a))
            .collect();
        // Far more likely a typo than a request for no toolbar.
        if !parsed.is_empty() {
            appearance.toolbar_actions = parsed;
        }
    }

    if let Some(width) = parsed.appearance.reading_max_width.filter(|w| *w >= 0.0) {
        appearance.reading_max_width = width;
    }
    if let Some(file) = &parsed.appearance.reading_css_file {
        let path = resolve_path(file, config_dir);
        match fs::read_to_string(&path) {
            Ok(css) => appearance.reading_css = css,
            Err(err) => log::warn!("reading css {} could not be read: {err}", path.display()),
        }
    }
    apply_message_row(&mut appearance.message_row, &parsed.appearance.message_row);
    apply_show(&mut appearance.show, &parsed.appearance.show);

    apply_theme(&mut appearance.palette, &parsed.theme);
    if let Some(theme_file) = &parsed.appearance.theme_file {
        let path = resolve_path(theme_file, config_dir);
        match fs::read_to_string(&path)
            .ok()
            .and_then(|raw| toml::from_str::<TomlThemeFile>(&raw).ok())
        {
            Some(file) => apply_theme(&mut appearance.palette, &file.theme),
            None => log::warn!("theme file {} could not be read", path.display()),
        }
    }
    appearance
}

fn resolve_path(value: &str, config_dir: Option<&Path>) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    let path = PathBuf::from(value);
    match (path.is_absolute(), config_dir) {
        (false, Some(dir)) => dir.join(path),
        _ => path,
    }
}

/// Unparseable slot names are dropped with a warning: a typo should cost one
/// field and say so, not silently redesign the row.
fn parse_slots(names: &[String], context: &str) -> Vec<MessageSlot> {
    names
        .iter()
        .filter_map(|name| match MessageSlot::parse(name) {
            Some(slot) => Some(slot),
            None => {
                log::warn!("unknown message row slot {name:?} in {context}, ignored");
                None
            }
        })
        .collect()
}

fn apply_message_row(row: &mut MessageRow, config: &TomlMessageRow) {
    // An empty gutter is how you say "no unread dot". An empty set of *lines*
    // leaves the list nothing to draw, so it is treated as a typo.
    if let Some(gutter) = &config.gutter {
        row.gutter = parse_slots(gutter, "gutter");
    }
    if let Some(lines) = &config.lines {
        let parsed: Vec<Vec<MessageSlot>> = lines
            .iter()
            .map(|line| parse_slots(line, "lines"))
            .filter(|line| !line.is_empty())
            .collect();
        if parsed.is_empty() {
            log::warn!("message_row.lines named no usable slots, keeping the default row");
        } else {
            row.lines = parsed;
        }
    }

    for (name, style) in &config.style {
        let Some(slot) = MessageSlot::parse(name) else {
            log::warn!("unknown message row slot {name:?} in style table, ignored");
            continue;
        };
        // Start from the slot's defaults, so a table setting only `size`
        // inherits the rest.
        let mut resolved = row.style(slot);
        if let Some(size) = style.size.filter(|size| *size > 0.0) {
            resolved.size = size;
        }
        if let Some(weight) = style.weight.as_deref().and_then(parse_weight) {
            resolved.weight = weight;
        }
        if let Some(color) = style.color.as_deref().and_then(Token::parse) {
            resolved.color = color;
            // Otherwise setting `color` alone is only ever visible on mail you
            // have already read.
            if style.color_unread.is_none() {
                resolved.color_unread = color;
            }
        }
        if let Some(color) = style.color_unread.as_deref().and_then(Token::parse) {
            resolved.color_unread = color;
        }
        row.overrides.retain(|(candidate, _)| *candidate != slot);
        row.overrides.push((slot, resolved));
    }
}

fn apply_show(show: &mut Show, config: &TomlShow) {
    show.sidebar = config.sidebar.unwrap_or(show.sidebar);
    show.toolbar = config.toolbar.unwrap_or(show.toolbar);
    show.message_list_header = config
        .message_list_header
        .unwrap_or(show.message_list_header);
    show.scrollbars = config.scrollbars.unwrap_or(show.scrollbars);
}

fn apply_theme(palette: &mut crate::theme::Palette, theme: &TomlTheme) {
    let set = |target: &mut u32, value: &Option<String>| {
        if let Some(hex) = value.as_deref().and_then(parse_hex) {
            *target = hex;
        }
    };
    set(&mut palette.bg_app, &theme.bg_app);
    set(&mut palette.bg_sidebar, &theme.bg_sidebar);
    set(&mut palette.bg_list, &theme.bg_list);
    set(&mut palette.bg_selected, &theme.bg_selected);
    set(&mut palette.bg_hover, &theme.bg_hover);
    set(&mut palette.bg_unread, &theme.bg_unread);
    set(&mut palette.bg_message, &theme.bg_message);
    set(&mut palette.border, &theme.border);
    set(&mut palette.text_primary, &theme.text_primary);
    set(&mut palette.text_secondary, &theme.text_secondary);
    set(&mut palette.text_muted, &theme.text_muted);
    set(&mut palette.accent, &theme.accent);
    set(&mut palette.danger, &theme.danger);
    set(&mut palette.scrollbar_thumb, &theme.scrollbar_thumb);
    set(
        &mut palette.scrollbar_thumb_hover,
        &theme.scrollbar_thumb_hover,
    );
}

pub fn watched_paths() -> Vec<PathBuf> {
    let path = config_path();
    let mut paths = vec![path.clone()];
    if let Some(appearance) = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| toml::from_str::<TomlFile>(&raw).ok())
        .map(|parsed| parsed.appearance)
    {
        for pointed_at in [&appearance.theme_file, &appearance.reading_css_file]
            .into_iter()
            .flatten()
        {
            paths.push(resolve_path(pointed_at, path.parent()));
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_accepts_the_usual_spellings() {
        assert_eq!(parse_hex("#282c34"), Some(0x282c34));
        assert_eq!(parse_hex("282c34"), Some(0x282c34));
        assert_eq!(parse_hex("0x282C34"), Some(0x282c34));
        assert_eq!(parse_hex(" #282c34 "), Some(0x282c34));
    }

    #[test]
    fn hex_rejects_rather_than_guesses() {
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("blue"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }

    #[test]
    fn a_theme_sets_only_what_it_names() {
        let mut palette = crate::theme::Palette::DEFAULT;
        let theme = TomlTheme {
            accent: Some("#ff0000".into()),
            ..TomlTheme::default()
        };
        apply_theme(&mut palette, &theme);
        assert_eq!(palette.accent, 0xff0000);
        assert_eq!(palette.bg_app, crate::theme::Palette::DEFAULT.bg_app);
    }

    #[test]
    fn one_bad_colour_does_not_take_the_rest_with_it() {
        let mut palette = crate::theme::Palette::DEFAULT;
        let theme = TomlTheme {
            accent: Some("not a colour".into()),
            bg_app: Some("#101010".into()),
            ..TomlTheme::default()
        };
        apply_theme(&mut palette, &theme);
        assert_eq!(palette.accent, crate::theme::Palette::DEFAULT.accent);
        assert_eq!(palette.bg_app, 0x101010);
    }

    #[test]
    fn dark_mode_and_actions_parse_case_insensitively() {
        assert_eq!(EmailDarkMode::parse("Always"), Some(EmailDarkMode::Always));
        assert_eq!(EmailDarkMode::parse(" never "), Some(EmailDarkMode::Never));
        assert_eq!(EmailDarkMode::parse("sometimes"), None);
        assert_eq!(
            ToolbarAction::parse("reply-all"),
            Some(ToolbarAction::ReplyAll)
        );
        assert_eq!(
            ToolbarAction::parse("REPLY_ALL"),
            Some(ToolbarAction::ReplyAll)
        );
        assert_eq!(ToolbarAction::parse("nope"), None);
    }

    #[test]
    fn remote_images_are_allowed_by_default_and_can_be_blocked() {
        assert_eq!(Appearance::default().remote_images, RemoteImages::Always);
        assert_eq!(RemoteImages::parse("never"), Some(RemoteImages::Never));
        assert_eq!(RemoteImages::parse("sometimes"), None);
    }

    #[test]
    fn the_default_row_is_the_row_that_was_hand_built() {
        let row = MessageRow::default();
        assert_eq!(row.gutter, vec![MessageSlot::UnreadDot]);
        assert_eq!(
            row.lines,
            vec![
                vec![MessageSlot::Sender, MessageSlot::Flag, MessageSlot::Date],
                vec![MessageSlot::Subject, MessageSlot::Attachment],
            ]
        );
        // A change here changes every row, so it is asserted.
        assert_eq!(row.height(), 61.0);
        assert_eq!(row.style(MessageSlot::Sender).size, 14.0);
        assert_eq!(row.style(MessageSlot::Subject).size, 12.0);
        assert_eq!(row.style(MessageSlot::Date).size, 11.0);
    }

    fn row_from(toml: &str) -> MessageRow {
        let parsed: TomlFile = toml::from_str(toml).expect("test config should parse");
        appearance_from(&parsed, None).message_row
    }

    #[test]
    fn a_row_can_be_reordered_and_given_a_third_line() {
        let row = row_from(
            r#"
            [appearance.message_row]
            lines = [
              ["date", "sender"],
              ["subject"],
              ["preview"],
            ]
            "#,
        );
        assert_eq!(row.lines[0], vec![MessageSlot::Date, MessageSlot::Sender]);
        assert_eq!(row.lines.len(), 3);
        assert!(row.height() > MessageRow::default().height());
    }

    #[test]
    fn omitting_a_slot_is_how_you_hide_it() {
        let row = row_from(
            r#"
            [appearance.message_row]
            gutter = []
            lines = [["sender"], ["subject"]]
            "#,
        );
        assert!(row.gutter.is_empty());
        assert!(!row
            .lines
            .iter()
            .flatten()
            .any(|slot| *slot == MessageSlot::Date));
    }

    #[test]
    fn an_unusable_line_list_keeps_the_default_row() {
        let row = row_from(
            r#"
            [appearance.message_row]
            lines = [["nonsense"], []]
            "#,
        );
        assert_eq!(row.lines, MessageRow::default().lines);
    }

    #[test]
    fn one_bad_slot_costs_only_that_slot() {
        let row = row_from(
            r#"
            [appearance.message_row]
            lines = [["sender", "nonsense", "date"], ["subject"]]
            "#,
        );
        assert_eq!(row.lines[0], vec![MessageSlot::Sender, MessageSlot::Date]);
    }

    #[test]
    fn a_style_table_sets_only_what_it_names() {
        let row = row_from(
            r#"
            [appearance.message_row.style.subject]
            size = 16
            "#,
        );
        let subject = row.style(MessageSlot::Subject);
        assert_eq!(subject.size, 16.0);
        assert_eq!(subject.weight, MessageSlot::Subject.default_style().weight);
        assert_eq!(subject.color, MessageSlot::Subject.default_style().color);
        assert!(row.height() > MessageRow::default().height());
    }

    #[test]
    fn a_slot_given_one_colour_uses_it_in_both_read_states() {
        let row = row_from(
            r#"
            [appearance.message_row.style.sender]
            color = "accent"
            "#,
        );
        let sender = row.style(MessageSlot::Sender);
        assert_eq!(sender.color_for(false), Token::Accent);
        assert_eq!(sender.color_for(true), Token::Accent);
    }

    #[test]
    fn naming_both_read_states_keeps_them_apart() {
        let row = row_from(
            r#"
            [appearance.message_row.style.sender]
            color = "text_muted"
            color_unread = "accent"
            "#,
        );
        let sender = row.style(MessageSlot::Sender);
        assert_eq!(sender.color_for(false), Token::TextMuted);
        assert_eq!(sender.color_for(true), Token::Accent);
    }

    #[test]
    fn a_mistyped_colour_or_weight_keeps_the_slot_drawable() {
        let row = row_from(
            r#"
            [appearance.message_row.style.sender]
            color = "chartreuse"
            weight = "extremely_bold"
            size = -4
            "#,
        );
        assert_eq!(
            row.style(MessageSlot::Sender),
            MessageSlot::Sender.default_style()
        );
    }

    #[test]
    fn slots_parse_by_their_aliases() {
        assert_eq!(MessageSlot::parse("from"), Some(MessageSlot::Sender));
        assert_eq!(
            MessageSlot::parse("UNREAD-DOT"),
            Some(MessageSlot::UnreadDot)
        );
        assert_eq!(MessageSlot::parse(" snippet "), Some(MessageSlot::Preview));
        assert_eq!(MessageSlot::parse("body"), None);
    }

    #[test]
    fn only_text_slots_take_the_leftover_width() {
        // An annotation must never grow, or a flagged message's icon pushes the
        // date off the row.
        assert!(MessageSlot::Sender.grows());
        assert!(MessageSlot::Preview.grows());
        assert!(!MessageSlot::Flag.grows());
        assert!(!MessageSlot::Date.grows());
        assert!(!MessageSlot::UnreadDot.grows());
    }

    #[test]
    fn regions_are_shown_unless_the_config_says_otherwise() {
        assert_eq!(
            Show::default(),
            Show {
                sidebar: true,
                toolbar: true,
                message_list_header: true,
                scrollbars: true
            }
        );
        let parsed: TomlFile = toml::from_str(
            r#"
            [appearance.show]
            sidebar = false
            scrollbars = false
            "#,
        )
        .unwrap();
        let show = appearance_from(&parsed, None).show;
        assert!(!show.sidebar);
        assert!(!show.scrollbars);
        assert!(show.toolbar);
        assert!(show.message_list_header);
    }

    #[test]
    fn an_unusable_toolbar_list_keeps_the_default() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [appearance]
            toolbar_actions = ["nonsense"]
            "#,
        )
        .unwrap();
        let appearance = appearance_from(&parsed, None);
        assert_eq!(appearance.toolbar_actions, ToolbarAction::DEFAULT.to_vec());
    }
}
