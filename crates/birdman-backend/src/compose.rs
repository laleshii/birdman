use birdman_mime::ParsedMessage;
use birdman_store::MessageSummary;

use crate::message::Recipient;

/// Rebuilds enough of a [`ParsedMessage`] from a stored envelope to drive
/// [`reply_draft`] and [`forward_draft`].
///
/// Lives here rather than in a client because reply-all membership and
/// `Reply-To` handling are contract, not presentation: two front ends deriving
/// it separately is how one of them ends up answering the wrong address.
pub fn parsed_from_summary(msg: &MessageSummary, body: Option<String>) -> ParsedMessage {
    ParsedMessage {
        subject: msg.subject.clone(),
        from: msg
            .from_addr
            .clone()
            .map(|address| {
                vec![birdman_mime::Mailbox {
                    name: msg.from_name.clone(),
                    address,
                }]
            })
            .unwrap_or_default(),
        to: split_addrs(msg.to_addrs.as_deref()),
        cc: split_addrs(msg.cc_addrs.as_deref()),
        // Dropping these two is how replies kept going to `From`.
        reply_to: split_addrs(msg.reply_to_addrs.as_deref()),
        bcc: split_addrs(msg.bcc_addrs.as_deref()),
        date: msg.date,
        message_id: msg.message_id_header.clone(),
        references: msg.references.clone(),
        text_body: body,
        ..Default::default()
    }
}

/// The store keeps `to`/`cc`/`bcc` as one comma-joined string of bare
/// addresses, so nothing here carries a display name.
pub fn split_addrs(joined: Option<&str>) -> Vec<birdman_mime::Mailbox> {
    joined
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|address| birdman_mime::Mailbox {
            name: None,
            address: address.to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ComposeDraft {
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

pub fn reply_draft(original: &ParsedMessage, self_address: &str, reply_all: bool) -> ComposeDraft {
    let subject = prefixed_subject(original.subject.as_deref(), "Re:");

    // RFC 5322: `Reply-To` overrides `From` as the answer address. Ignoring
    // it answers a list's posting address, or a `no-reply@` box.
    let answer_to = if original.reply_to.is_empty() {
        &original.from
    } else {
        &original.reply_to
    };
    let mut to: Vec<Recipient> = answer_to
        .iter()
        .map(|m| Recipient::new(m.name.clone(), m.address.clone()))
        .collect();
    let to_addresses: Vec<String> = to.iter().map(|r| r.address.clone()).collect();
    let mut cc = Vec::new();
    if reply_all {
        to.extend(
            original
                .to
                .iter()
                .filter(|m| {
                    !to_addresses
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case(&m.address))
                })
                .filter(|m| !m.address.eq_ignore_ascii_case(self_address))
                .map(|m| Recipient::new(m.name.clone(), m.address.clone())),
        );
        cc = original
            .cc
            .iter()
            .filter(|m| !m.address.eq_ignore_ascii_case(self_address))
            .map(|m| Recipient::new(m.name.clone(), m.address.clone()))
            .collect();
    }

    let mut references = original.references.clone();
    if let Some(id) = &original.message_id {
        if !references.iter().any(|r| r == id) {
            references.push(id.clone());
        }
    }

    ComposeDraft {
        to,
        cc,
        subject,
        body: format!("\n\n{}", quote_body(original)),
        in_reply_to: original.message_id.clone(),
        references,
    }
}

pub fn forward_draft(original: &ParsedMessage) -> ComposeDraft {
    let subject = prefixed_subject(original.subject.as_deref(), "Fwd:");
    let from = original
        .from
        .first()
        .map(|m| format_recipient(m.name.as_deref(), &m.address))
        .unwrap_or_default();
    let to = original
        .to
        .iter()
        .map(|m| format_recipient(m.name.as_deref(), &m.address))
        .collect::<Vec<_>>()
        .join(", ");

    let body = format!(
        "\n\n---------- Forwarded message ----------\nFrom: {from}\nDate: {}\nSubject: {}\nTo: {to}\n\n{}",
        original.date.map(format_date).unwrap_or_default(),
        original.subject.clone().unwrap_or_default(),
        original.text_body.clone().unwrap_or_default(),
    );

    ComposeDraft {
        body,
        subject,
        ..Default::default()
    }
}

fn quote_body(original: &ParsedMessage) -> String {
    let from = original
        .from
        .first()
        .map(|m| format_recipient(m.name.as_deref(), &m.address))
        .unwrap_or_default();
    let date = original.date.map(format_date).unwrap_or_default();
    let header = format!("On {date}, {from} wrote:");
    let quoted_lines = original
        .text_body
        .as_deref()
        .unwrap_or_default()
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{header}\n{quoted_lines}")
}

fn prefixed_subject(subject: Option<&str>, prefix: &str) -> String {
    let subject = subject.unwrap_or("");
    if subject.to_lowercase().starts_with(&prefix.to_lowercase()) {
        subject.to_string()
    } else {
        format!("{prefix} {subject}")
    }
}

fn format_recipient(name: Option<&str>, address: &str) -> String {
    match name {
        Some(name) if !name.is_empty() => format!("{name} <{address}>"),
        _ => address.to_string(),
    }
}

fn format_date(ts: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(ts) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            dt.year(),
            dt.month() as u8,
            dt.day(),
            dt.hour(),
            dt.minute()
        ),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    /// The CLI and the desktop both reply through `parsed_from_summary`, so a
    /// header it drops is a header both of them answer without.
    #[test]
    fn a_stored_envelope_carries_every_header_a_reply_needs() {
        let stored = MessageSummary {
            id: birdman_store::MessageId(1),
            folder_id: birdman_store::FolderId(1),
            uid: 7,
            subject: Some("Release notes".into()),
            from_addr: Some("bot@list.example".into()),
            from_name: Some("List Bot".into()),
            to_addrs: Some("you@example.com, ada@example.com".into()),
            cc_addrs: Some("bob@example.com".into()),
            reply_to_addrs: Some("list@list.example".into()),
            bcc_addrs: None,
            message_id_header: Some("<abc@list.example>".into()),
            references: vec!["<root@list.example>".into()],
            date: Some(1_700_000_000),
            has_attachments: false,
            flags: Default::default(),
            body_fetched: true,
            preview: None,
        };

        let parsed = parsed_from_summary(&stored, Some("the original".into()));
        let draft = reply_draft(&parsed, "you@example.com", true);

        // Reply-To wins over From, and the sender is not also copied.
        assert_eq!(draft.to[0].address, "list@list.example");
        assert_eq!(
            draft
                .to
                .iter()
                .filter(|r| r.address == "bot@list.example")
                .count(),
            0
        );
        // Reply-all keeps the other recipient and drops us.
        assert!(draft.to.iter().any(|r| r.address == "ada@example.com"));
        assert!(!draft.to.iter().any(|r| r.address == "you@example.com"));
        assert_eq!(draft.cc[0].address, "bob@example.com");
        // Threading survives the round trip through the store's flat columns.
        assert_eq!(draft.in_reply_to.as_deref(), Some("<abc@list.example>"));
        assert!(draft.references.iter().any(|r| r == "<root@list.example>"));
        assert!(draft.body.contains("the original"));
    }

    fn message_from(from: &str, reply_to: &[&str]) -> ParsedMessage {
        ParsedMessage {
            subject: Some("Release notes".into()),
            from: vec![birdman_mime::Mailbox {
                name: Some("List Bot".into()),
                address: from.into(),
            }],
            reply_to: reply_to
                .iter()
                .map(|a| birdman_mime::Mailbox {
                    name: None,
                    address: (*a).into(),
                })
                .collect(),
            ..ParsedMessage::default()
        }
    }

    #[test]
    fn a_reply_goes_to_reply_to_when_the_sender_set_one() {
        let original = message_from("no-reply@example.com", &["list@example.com"]);
        let draft = reply_draft(&original, "me@example.com", false);
        assert_eq!(draft.to.len(), 1);
        assert_eq!(draft.to[0].address, "list@example.com");
    }

    #[test]
    fn without_one_it_still_goes_to_from() {
        let original = message_from("person@example.com", &[]);
        let draft = reply_draft(&original, "me@example.com", false);
        assert_eq!(draft.to[0].address, "person@example.com");
    }

    #[test]
    fn reply_all_does_not_repeat_an_address_reply_to_already_named() {
        let mut original = message_from("bot@example.com", &["list@example.com"]);
        original.to = vec![
            birdman_mime::Mailbox {
                name: None,
                address: "list@example.com".into(),
            },
            birdman_mime::Mailbox {
                name: None,
                address: "other@example.com".into(),
            },
        ];
        let draft = reply_draft(&original, "me@example.com", true);
        let addresses: Vec<_> = draft.to.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(addresses, vec!["list@example.com", "other@example.com"]);
    }

    use super::*;
    use birdman_mime::Mailbox;

    fn sample() -> ParsedMessage {
        ParsedMessage {
            subject: Some("Lunch?".to_string()),
            from: vec![Mailbox {
                name: Some("Alice".to_string()),
                address: "alice@example.com".to_string(),
            }],
            to: vec![
                Mailbox {
                    name: Some("Bob".to_string()),
                    address: "bob@example.com".to_string(),
                },
                Mailbox {
                    name: Some("Me".to_string()),
                    address: "me@example.com".to_string(),
                },
            ],
            cc: vec![Mailbox {
                name: None,
                address: "carol@example.com".to_string(),
            }],
            date: Some(1_700_000_000),
            message_id: Some("abc@example.com".to_string()),
            text_body: Some("Want to grab lunch?".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn reply_goes_to_sender_only_by_default() {
        let draft = reply_draft(&sample(), "me@example.com", false);
        assert_eq!(draft.to.len(), 1);
        assert_eq!(draft.to[0].address, "alice@example.com");
        assert!(draft.cc.is_empty());
        assert_eq!(draft.subject, "Re: Lunch?");
        assert_eq!(draft.in_reply_to.as_deref(), Some("abc@example.com"));
        assert!(draft.body.contains("> Want to grab lunch?"));
    }

    #[test]
    fn reply_all_includes_to_and_cc_but_excludes_self() {
        let draft = reply_draft(&sample(), "me@example.com", true);
        let to_addrs: Vec<_> = draft.to.iter().map(|r| r.address.as_str()).collect();
        assert!(to_addrs.contains(&"alice@example.com"));
        assert!(to_addrs.contains(&"bob@example.com"));
        assert!(!to_addrs.contains(&"me@example.com"));
        assert_eq!(draft.cc.len(), 1);
        assert_eq!(draft.cc[0].address, "carol@example.com");
    }

    #[test]
    fn reply_does_not_double_prefix_subject() {
        let mut msg = sample();
        msg.subject = Some("Re: Lunch?".to_string());
        let draft = reply_draft(&msg, "me@example.com", false);
        assert_eq!(draft.subject, "Re: Lunch?");
    }

    #[test]
    fn forward_has_no_recipients_and_quotes_headers() {
        let draft = forward_draft(&sample());
        assert!(draft.to.is_empty());
        assert_eq!(draft.subject, "Fwd: Lunch?");
        assert!(draft.body.contains("From: Alice <alice@example.com>"));
        assert!(draft.body.contains("Want to grab lunch?"));
        assert!(draft.in_reply_to.is_none());
    }
}
