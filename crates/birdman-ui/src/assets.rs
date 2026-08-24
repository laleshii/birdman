//! Vendored [Lucide](https://lucide.dev) icons, ISC licensed.
//!
//! Two edits are applied when vendoring a new one, and neither is cosmetic:
//! replace `currentColor` with `black`, and drop the `class` attributes.
//! gpui rasterizes an SVG to an *alpha mask* and fills it with the element's
//! `text_color`, so the file's own colour is discarded but must still resolve
//! to something opaque -- and `currentColor` has no cascade to resolve against.
//! Colour comes from `.text_color(...)`, size from `.size(...)`; the file's
//! own `width`/`height` are ignored.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/refresh.svg",
        include_bytes!("../assets/icons/refresh-cw.svg"),
    ),
    (
        "icons/settings.svg",
        include_bytes!("../assets/icons/settings.svg"),
    ),
    (
        "icons/sidebar-hide.svg",
        include_bytes!("../assets/icons/panel-left-close.svg"),
    ),
    (
        "icons/sidebar-show.svg",
        include_bytes!("../assets/icons/panel-left-open.svg"),
    ),
    (
        "icons/paperclip.svg",
        include_bytes!("../assets/icons/paperclip.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/inbox.svg",
        include_bytes!("../assets/icons/inbox.svg"),
    ),
    (
        "icons/drafts.svg",
        include_bytes!("../assets/icons/file-pen.svg"),
    ),
    ("icons/sent.svg", include_bytes!("../assets/icons/send.svg")),
    (
        "icons/trash.svg",
        include_bytes!("../assets/icons/trash-2.svg"),
    ),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/folder.svg"),
    ),
    (
        "icons/reply.svg",
        include_bytes!("../assets/icons/reply.svg"),
    ),
    (
        "icons/reply-all.svg",
        include_bytes!("../assets/icons/reply-all.svg"),
    ),
    (
        "icons/forward.svg",
        include_bytes!("../assets/icons/forward.svg"),
    ),
    ("icons/flag.svg", include_bytes!("../assets/icons/flag.svg")),
    (
        "icons/archive.svg",
        include_bytes!("../assets/icons/archive.svg"),
    ),
    (
        "icons/compose.svg",
        include_bytes!("../assets/icons/square-pen.svg"),
    ),
    ("icons/sun.svg", include_bytes!("../assets/icons/sun.svg")),
    ("icons/moon.svg", include_bytes!("../assets/icons/moon.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Widen this list when another module starts drawing icons, or the check
    /// quietly stops covering it.
    #[test]
    fn table_and_call_sites_agree() {
        const SOURCES: [&str; 2] = [include_str!("root.rs"), include_str!("state.rs")];
        for (name, bytes) in ICONS {
            assert!(!bytes.is_empty(), "{name} is empty");
            assert!(
                SOURCES.iter().any(|source| source.contains(name)),
                "{name} is in the table but nothing renders it"
            );
        }
        for source in SOURCES {
            for referenced in source.match_indices("icons/").map(|(at, _)| {
                let rest = &source[at..];
                &rest[..rest.find(".svg").map(|end| end + 4).unwrap_or(rest.len())]
            }) {
                assert!(
                    ICONS.iter().any(|(name, _)| *name == referenced),
                    "{referenced} is drawn but missing from the table"
                );
            }
        }
    }

    #[test]
    fn load_returns_none_for_unknown_paths() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn list_filters_by_prefix() {
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len());
        assert!(Assets.list("other/").unwrap().is_empty());
    }
}
