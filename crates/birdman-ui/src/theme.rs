use std::sync::RwLock;

use gpui::{rgb, Rgba};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token {
    BgApp,
    BgSidebar,
    BgList,
    BgSelected,
    BgHover,
    BgUnread,
    BgMessage,
    Border,
    TextPrimary,
    TextSecondary,
    TextMuted,
    Accent,
    Danger,
    ScrollbarThumb,
    ScrollbarThumbHover,
}

pub const BG_APP: Token = Token::BgApp;
pub const BG_SIDEBAR: Token = Token::BgSidebar;
pub const BG_LIST: Token = Token::BgList;
pub const BG_SELECTED: Token = Token::BgSelected;
pub const BG_HOVER: Token = Token::BgHover;
pub const BG_UNREAD: Token = Token::BgUnread;
pub const BG_MESSAGE: Token = Token::BgMessage;
pub const BORDER: Token = Token::Border;
pub const TEXT_PRIMARY: Token = Token::TextPrimary;
pub const TEXT_SECONDARY: Token = Token::TextSecondary;
pub const TEXT_MUTED: Token = Token::TextMuted;
pub const ACCENT: Token = Token::Accent;
pub const DANGER: Token = Token::Danger;
pub const SCROLLBAR_THUMB: Token = Token::ScrollbarThumb;
pub const SCROLLBAR_THUMB_HOVER: Token = Token::ScrollbarThumbHover;

impl Token {
    /// The same identifiers the `[theme]` config table uses, so a slot asking
    /// for `color = "accent"` follows the palette rather than pinning a hex.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "bg_app" => Some(Self::BgApp),
            "bg_sidebar" => Some(Self::BgSidebar),
            "bg_list" => Some(Self::BgList),
            "bg_selected" => Some(Self::BgSelected),
            "bg_hover" => Some(Self::BgHover),
            "bg_unread" => Some(Self::BgUnread),
            "bg_message" => Some(Self::BgMessage),
            "border" => Some(Self::Border),
            "text_primary" => Some(Self::TextPrimary),
            "text_secondary" => Some(Self::TextSecondary),
            "text_muted" => Some(Self::TextMuted),
            "accent" => Some(Self::Accent),
            "danger" => Some(Self::Danger),
            "scrollbar_thumb" => Some(Self::ScrollbarThumb),
            "scrollbar_thumb_hover" => Some(Self::ScrollbarThumbHover),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg_app: u32,
    pub bg_sidebar: u32,
    pub bg_list: u32,
    pub bg_selected: u32,
    pub bg_hover: u32,
    pub bg_unread: u32,
    pub bg_message: u32,
    pub border: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
    pub accent: u32,
    pub danger: u32,
    pub scrollbar_thumb: u32,
    pub scrollbar_thumb_hover: u32,
}

impl Palette {
    pub const DEFAULT: Palette = Palette {
        bg_app: 0x282c34,
        bg_sidebar: 0x21252b,
        bg_list: 0x24282f,
        bg_selected: 0x3a4657,
        bg_hover: 0x323844,
        bg_unread: 0x2d323c,
        bg_message: 0x2f343d,
        border: 0x3e4451,
        text_primary: 0xffffff,
        text_secondary: 0xc5c8c6,
        // Not ghostty's palette 8 (`#666666`) -- unreadable against `#282c34`.
        text_muted: 0x8c919c,
        accent: 0x7aa6da,
        danger: 0xcc6666,
        scrollbar_thumb: 0x767d8a,
        scrollbar_thumb_hover: 0xa6aeba,
    };

    fn get(&self, token: Token) -> u32 {
        match token {
            Token::BgApp => self.bg_app,
            Token::BgSidebar => self.bg_sidebar,
            Token::BgList => self.bg_list,
            Token::BgSelected => self.bg_selected,
            Token::BgHover => self.bg_hover,
            Token::BgUnread => self.bg_unread,
            Token::BgMessage => self.bg_message,
            Token::Border => self.border,
            Token::TextPrimary => self.text_primary,
            Token::TextSecondary => self.text_secondary,
            Token::TextMuted => self.text_muted,
            Token::Accent => self.accent,
            Token::Danger => self.danger,
            Token::ScrollbarThumb => self.scrollbar_thumb,
            Token::ScrollbarThumbHover => self.scrollbar_thumb_hover,
        }
    }
}

static CURRENT: RwLock<Palette> = RwLock::new(Palette::DEFAULT);

pub fn color(token: Token) -> Rgba {
    rgb(hex(token))
}

pub fn color_alpha(token: Token, alpha: f32) -> Rgba {
    let mut colour = color(token);
    colour.a = alpha;
    colour
}

/// Raw `0xRRGGBB`, for the CSS injected into the reading pane's webview.
pub fn hex(token: Token) -> u32 {
    // Falling back on a poisoned lock keeps the UI drawable rather than taking
    // the app down over a colour.
    CURRENT
        .read()
        .map(|p| p.get(token))
        .unwrap_or_else(|_| Palette::DEFAULT.get(token))
}

/// Callers must ask for a redraw afterwards -- nothing here observes it.
pub fn set_palette(palette: Palette) {
    if let Ok(mut current) = CURRENT.write() {
        *current = palette;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_parse_by_the_name_the_config_uses() {
        assert_eq!(Token::parse("accent"), Some(ACCENT));
        assert_eq!(Token::parse("TEXT-MUTED"), Some(TEXT_MUTED));
        assert_eq!(Token::parse(" bg_app "), Some(BG_APP));
        assert_eq!(Token::parse("chartreuse"), None);
    }

    #[test]
    fn every_token_resolves_and_they_are_distinct_roles() {
        // Not distinct colours -- a theme may reuse one -- but every token must
        // map to its own field, which a copy-pasted match arm would break.
        let probe = Palette {
            bg_app: 1,
            bg_sidebar: 2,
            bg_list: 3,
            bg_selected: 4,
            bg_hover: 5,
            bg_unread: 6,
            bg_message: 15,
            border: 7,
            text_primary: 8,
            text_secondary: 9,
            text_muted: 10,
            accent: 11,
            danger: 12,
            scrollbar_thumb: 13,
            scrollbar_thumb_hover: 14,
        };
        let tokens = [
            BG_APP,
            BG_SIDEBAR,
            BG_LIST,
            BG_SELECTED,
            BG_HOVER,
            BG_UNREAD,
            BG_MESSAGE,
            BORDER,
            TEXT_PRIMARY,
            TEXT_SECONDARY,
            TEXT_MUTED,
            ACCENT,
            DANGER,
            SCROLLBAR_THUMB,
            SCROLLBAR_THUMB_HOVER,
        ];
        let mut seen: Vec<u32> = tokens.iter().map(|t| probe.get(*t)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            tokens.len(),
            "two tokens read the same palette field"
        );
    }
}
