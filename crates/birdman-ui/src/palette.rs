use gpui::{Context, Window};

use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Global,
    Message,
}

impl Section {
    pub fn title(self) -> Option<&'static str> {
        match self {
            Section::Global => None,
            Section::Message => Some("Message"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Compose,
    View,
    Mailbox,
    Window,
    Respond,
    File,
    Remove,
}

impl Group {
    pub fn section(self) -> Section {
        match self {
            Group::Compose | Group::View | Group::Mailbox | Group::Window => Section::Global,
            Group::Respond | Group::File | Group::Remove => Section::Message,
        }
    }
}

/// The printed name only; gpui's `Modifiers::secondary()` already resolves the
/// behaviour.
pub const MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// The table is written in Cmd; the substitution happens once, here.
pub fn shortcut_label(shortcut: &'static str) -> std::borrow::Cow<'static, str> {
    match shortcut.strip_prefix("Cmd") {
        Some(rest) if MODIFIER != "Cmd" => std::borrow::Cow::Owned(format!("{MODIFIER}{rest}")),
        _ => std::borrow::Cow::Borrowed(shortcut),
    }
}

pub struct PaletteCommand {
    pub name: &'static str,
    /// Searched alongside the name, so "trash" finds Delete.
    pub aliases: &'static str,
    /// Not optional: the palette is the only place bindings are advertised, and
    /// a test pins that each one is actually bound.
    pub shortcut: &'static str,
    pub group: Group,
    /// `name` stays the searchable identity; only the row's text changes. Both
    /// spellings must be covered by `name` + `aliases` or the command becomes
    /// unsearchable half the time.
    pub label: Option<fn(&AppState) -> &'static str>,
    pub run: fn(&mut AppState, &mut Window, &mut Context<AppState>),
}

impl PaletteCommand {
    pub fn label(&self, state: &AppState) -> &'static str {
        self.label.map_or(self.name, |label| label(state))
    }
}

pub const COMMANDS: &[PaletteCommand] = &[
    PaletteCommand {
        name: "New message",
        aliases: "compose write",
        shortcut: "Cmd N",
        group: Group::Compose,
        label: None,
        run: |state, _, cx| state.compose_new(cx),
    },
    PaletteCommand {
        name: "Search",
        aliases: "find filter",
        shortcut: "Cmd F",
        group: Group::View,
        label: None,
        run: |state, window, cx| state.toggle_search(window, cx),
    },
    PaletteCommand {
        name: "Show unread only",
        aliases: "filter hide read",
        shortcut: "Cmd U",
        group: Group::View,
        label: None,
        run: |state, _, cx| state.toggle_unread_only(cx),
    },
    PaletteCommand {
        name: "Show messages with attachments",
        aliases: "filter paperclip files",
        shortcut: "Cmd I",
        group: Group::View,
        label: None,
        run: |state, _, cx| state.toggle_attachments_only(cx),
    },
    PaletteCommand {
        name: "Toggle sidebar",
        aliases: "hide show folders",
        shortcut: "Cmd B",
        group: Group::View,
        label: None,
        run: |state, _, cx| state.toggle_sidebar(cx),
    },
    PaletteCommand {
        name: "Sync now",
        aliases: "refresh fetch",
        shortcut: "Cmd Shift S",
        group: Group::Mailbox,
        label: None,
        run: |state, _, cx| state.sync_now(cx),
    },
    PaletteCommand {
        name: "Switch account",
        aliases: "change mailbox",
        shortcut: "Cmd Shift A",
        group: Group::Mailbox,
        label: None,
        run: |state, _, cx| state.toggle_account_picker(cx),
    },
    PaletteCommand {
        name: "Close window",
        aliases: "quit hide",
        shortcut: "Cmd W",
        group: Group::Window,
        label: None,
        run: |_, window, _| window.remove_window(),
    },
    PaletteCommand {
        name: "Reply",
        aliases: "respond answer",
        shortcut: "Cmd R",
        group: Group::Respond,
        label: None,
        run: |state, _, cx| state.reply(false, cx),
    },
    PaletteCommand {
        name: "Reply all",
        aliases: "respond everyone",
        shortcut: "Cmd Shift R",
        group: Group::Respond,
        label: None,
        run: |state, _, cx| state.reply(true, cx),
    },
    PaletteCommand {
        name: "Forward",
        aliases: "send on",
        shortcut: "Cmd Shift F",
        group: Group::Respond,
        label: None,
        run: |state, _, cx| state.forward(cx),
    },
    PaletteCommand {
        name: "Toggle dark rendering",
        aliases: "light dark sun moon theme colours colors sender adapt original",
        shortcut: "Cmd D",
        group: Group::File,
        label: Some(|state| {
            if state.selected_is_darkened() {
                "Show the sender's own colours"
            } else {
                "Adapt this message to dark"
            }
        }),
        run: |state, _, cx| state.toggle_dark_mode(cx),
    },
    PaletteCommand {
        name: "Move to folder",
        aliases: "file filing",
        shortcut: "Cmd Shift M",
        group: Group::File,
        label: None,
        run: |state, _, cx| state.set_move_picker(true, cx),
    },
    PaletteCommand {
        name: "Flag",
        aliases: "star mark important",
        shortcut: "Cmd L",
        group: Group::File,
        label: None,
        run: |state, _, cx| state.toggle_flag_selected(cx),
    },
    PaletteCommand {
        name: "Archive",
        aliases: "done",
        shortcut: "Cmd E",
        group: Group::Remove,
        label: None,
        run: |state, _, cx| state.archive_selected(cx),
    },
    PaletteCommand {
        name: "Delete",
        aliases: "trash bin remove",
        shortcut: "Delete",
        group: Group::Remove,
        label: None,
        run: |state, _, cx| state.delete_selected(cx),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_named_and_findable() {
        for command in COMMANDS {
            assert!(!command.name.is_empty());
            assert!(
                !command.aliases.is_empty(),
                "{} has no aliases",
                command.name
            );
            assert!(
                command.name.chars().next().is_some_and(char::is_uppercase),
                "{} should read as a sentence",
                command.name
            );
        }
    }

    #[test]
    fn a_state_dependent_label_stays_findable_by_its_own_words() {
        for command in COMMANDS.iter().filter(|c| c.label.is_some()) {
            let searchable = format!("{} {}", command.name.to_ascii_lowercase(), command.aliases);
            for word in ["dark", "light", "sun", "moon", "sender", "adapt"] {
                assert!(
                    searchable.contains(word),
                    "{} shows a label that changes, but typing {word:?} finds nothing",
                    command.name
                );
            }
        }
    }

    #[test]
    fn shortcuts_are_spelled_for_the_platform() {
        for command in COMMANDS {
            let shown = shortcut_label(command.shortcut);
            if cfg!(target_os = "macos") {
                assert_eq!(shown, command.shortcut);
            } else {
                assert!(
                    !shown.contains("Cmd"),
                    "{} still says Cmd: {shown}",
                    command.name
                );
            }
        }
        assert_eq!(shortcut_label("Delete"), "Delete");
    }

    #[test]
    fn the_message_section_is_exactly_the_commands_that_need_one() {
        for command in COMMANDS {
            let needs_message = command.group.section() == Section::Message;
            // Spelled out as a list so adding a command forces the question.
            let acts_on_selection = matches!(
                command.name,
                "Reply"
                    | "Reply all"
                    | "Forward"
                    | "Toggle dark rendering"
                    | "Move to folder"
                    | "Flag"
                    | "Archive"
                    | "Delete"
            );
            assert_eq!(
                needs_message, acts_on_selection,
                "{} is in the wrong section for what it does",
                command.name
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two commands share a name");
    }

    /// The renderer emits a rule whenever the group changes, so interleaved
    /// entries would draw the same divider twice.
    #[test]
    fn groups_and_sections_are_contiguous() {
        let mut groups: Vec<Group> = Vec::new();
        for command in COMMANDS {
            if groups.last() != Some(&command.group) {
                assert!(
                    !groups.contains(&command.group),
                    "{:?} appears in two runs",
                    command.group
                );
                groups.push(command.group);
            }
        }
        let sections: Vec<Section> = groups.iter().map(|g| g.section()).collect();
        let mut runs: Vec<Section> = Vec::new();
        for section in sections {
            if runs.last() != Some(&section) {
                assert!(!runs.contains(&section), "{section:?} appears in two runs");
                runs.push(section);
            }
        }
        assert_eq!(runs, vec![Section::Global, Section::Message]);
    }

    #[test]
    fn every_advertised_shortcut_is_actually_bound() {
        const ROOT: &str = include_str!("root.rs");
        const MAIN: &str = include_str!("main.rs");
        const BOUND: &[(&str, &str, &str)] = &[
            ("Cmd N", ROOT, "\"n\" =>"),
            ("Cmd R", ROOT, "\"r\" =>"),
            ("Cmd Shift R", ROOT, "state.reply(true, cx)"),
            ("Cmd Shift F", ROOT, "state.forward(cx)"),
            ("Cmd Shift S", ROOT, "state.sync_now(cx)"),
            ("Cmd U", ROOT, "state.toggle_unread_only(cx)"),
            ("Cmd I", ROOT, "state.toggle_attachments_only(cx)"),
            ("Cmd Shift A", ROOT, "state.toggle_account_picker(cx)"),
            ("Cmd B", ROOT, "state.toggle_sidebar(cx)"),
            ("Cmd Shift M", ROOT, "state.set_move_picker(true, cx)"),
            ("Cmd L", ROOT, "state.toggle_flag_selected(cx)"),
            ("Cmd E", ROOT, "state.archive_selected(cx)"),
            ("Cmd D", ROOT, "state.toggle_dark_mode(cx)"),
            ("Delete", ROOT, "\"backspace\" | \"delete\" =>"),
            // The chord is built from a per-platform modifier, so match the
            // suffix rather than the whole binding.
            ("Cmd F", MAIN, "{modifier}-f"),
            ("Cmd W", MAIN, "{modifier}-w"),
        ];
        for command in COMMANDS {
            let entry = BOUND
                .iter()
                .find(|(label, _, _)| *label == command.shortcut);
            let Some((_, source, snippet)) = entry else {
                panic!(
                    "{} advertises {}, which this test doesn't know how to verify",
                    command.name, command.shortcut
                );
            };
            assert!(
                source.contains(snippet),
                "{} advertises {} but {snippet:?} is gone",
                command.name,
                command.shortcut
            );
        }
    }

    #[test]
    fn no_two_commands_claim_the_same_shortcut() {
        let mut seen: Vec<&str> = COMMANDS.iter().map(|c| c.shortcut).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two commands advertise one binding");
    }
}
