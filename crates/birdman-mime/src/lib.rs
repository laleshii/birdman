//! Wraps `mail-parser`: raw RFC822 bytes -> a flat, owned [`ParsedMessage`].
//!
//! Two guards are load-bearing, not incidental. `mail-parser` documents no
//! size or depth limits of its own, and this crate parses mail from arbitrary
//! senders:
//!
//! 1. [`MAX_RAW_MESSAGE_BYTES`] rejects oversized input before the parser sees
//!    it, and part iteration is capped at [`MAX_PARTS`] regardless of what the
//!    parser reports.
//! 2. **Never call [`mail_parser::MessagePart::message`]** to descend into
//!    nested `message/rfc822` parts. That is the exact vector of
//!    CVE-2026-26312 -- cyclical references from malformed nesting, unbounded
//!    when walked -- and nothing here needs to recurse into forwarded mail.

use mail_parser::{
    Address, ContentType, DateTime, HeaderValue, Message as RawMessage, MessageParser, MimeHeaders,
};

pub const MAX_RAW_MESSAGE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB

pub const MAX_PARTS: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("raw message too large ({size} bytes, limit {MAX_RAW_MESSAGE_BYTES})")]
    TooLarge { size: usize },
    #[error("no valid RFC5322 header block found")]
    NoHeaders,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub content_id: Option<String>,
    pub is_inline: bool,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedMessage {
    pub subject: Option<String>,
    pub from: Vec<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub reply_to: Vec<Mailbox>,
    pub date: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    /// Whatever its media type: `mail-parser` lists the `text/html` part here
    /// for an HTML-only message, so this can be raw HTML. Use
    /// [`ParsedMessage::text_plain_body`] when it must not be.
    pub text_body: Option<String>,
    /// Genuinely `text/plain`, or `None`. Anything that must not show markup
    /// reads this and accepts having nothing to show.
    pub text_plain_body: Option<String>,
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
}

pub fn parse_message(raw: &[u8]) -> Result<ParsedMessage, ParseError> {
    if raw.len() > MAX_RAW_MESSAGE_BYTES {
        return Err(ParseError::TooLarge { size: raw.len() });
    }

    let message = MessageParser::default()
        .parse(raw)
        .ok_or(ParseError::NoHeaders)?;

    Ok(to_parsed(&message))
}

fn to_parsed(message: &RawMessage<'_>) -> ParsedMessage {
    ParsedMessage {
        subject: message.subject().map(str::to_owned),
        from: mailboxes(message.from()),
        to: mailboxes(message.to()),
        cc: mailboxes(message.cc()),
        bcc: mailboxes(message.bcc()),
        reply_to: mailboxes(message.reply_to()),
        date: message.date().map(DateTime::to_timestamp),
        message_id: message.message_id().map(str::to_owned),
        in_reply_to: header_ids(message.in_reply_to()),
        references: header_ids(message.references()),
        text_body: message
            .text_bodies()
            .take(MAX_PARTS)
            .find_map(|part| part.text_contents())
            .map(str::to_owned),
        text_plain_body: message
            .text_bodies()
            .take(MAX_PARTS)
            .find(|part| is_text_plain(part))
            .and_then(|part| part.text_contents())
            .map(str::to_owned),
        html_body: message
            .html_bodies()
            .take(MAX_PARTS)
            .find_map(|part| part.text_contents())
            .map(str::to_owned),
        attachments: message
            .attachments()
            .take(MAX_PARTS)
            .filter(|part| !is_alternative_body(part))
            .map(|part| Attachment {
                filename: part.attachment_name().map(str::to_owned),
                content_type: part.content_type().map(content_type_string),
                content_id: part.content_id().map(str::to_owned),
                is_inline: part
                    .content_disposition()
                    .map(ContentType::is_inline)
                    .unwrap_or(false),
                contents: part.contents().to_vec(),
            })
            .collect(),
    }
}

/// `mail-parser` only recognises `text/plain` and `text/html` as bodies, so a
/// third rendering (`text/x-amp-html`, `text/watch-html`) is filed as an
/// attachment. Only *unnamed* parts are discarded -- a real `report.html`
/// attachment is named and still arrives as one.
fn is_alternative_body(part: &mail_parser::MessagePart) -> bool {
    if part.attachment_name().is_some() {
        return false;
    }
    let Some(content_type) = part.content_type() else {
        return false;
    };
    if !content_type.ctype().eq_ignore_ascii_case("text") {
        return false;
    }
    matches!(
        content_type
            .subtype()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("html" | "x-amp-html" | "watch-html" | "plain")
    )
}

/// A part with no `Content-Type` counts: RFC 2045 makes `text/plain` the
/// default.
fn is_text_plain(part: &mail_parser::MessagePart<'_>) -> bool {
    match part.content_type() {
        Some(ct) => {
            ct.ctype().eq_ignore_ascii_case("text")
                && ct
                    .subtype()
                    .is_none_or(|sub| sub.eq_ignore_ascii_case("plain"))
        }
        None => true,
    }
}

fn content_type_string(ct: &ContentType<'_>) -> String {
    match ct.subtype() {
        Some(subtype) => format!("{}/{}", ct.ctype(), subtype),
        None => ct.ctype().to_owned(),
    }
}

fn mailboxes(addr: Option<&Address<'_>>) -> Vec<Mailbox> {
    let Some(addr) = addr else {
        return Vec::new();
    };
    match addr {
        Address::List(addrs) => addrs
            .iter()
            .take(MAX_PARTS)
            .filter_map(to_mailbox)
            .collect(),
        Address::Group(groups) => groups
            .iter()
            .take(MAX_PARTS)
            .flat_map(|group| group.addresses.iter())
            .take(MAX_PARTS)
            .filter_map(to_mailbox)
            .collect(),
    }
}

fn to_mailbox(addr: &mail_parser::Addr<'_>) -> Option<Mailbox> {
    addr.address.as_ref().map(|address| Mailbox {
        name: addr.name.as_ref().map(|n| n.to_string()),
        address: address.to_string(),
    })
}

fn header_ids(value: &HeaderValue<'_>) -> Vec<String> {
    match value {
        HeaderValue::Text(text) => vec![text.to_string()],
        HeaderValue::TextList(texts) => texts
            .iter()
            .take(MAX_PARTS)
            .map(|t| t.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Reads `text_plain_body` only. Falling back to the HTML part was tried and
/// removed: a preview is built from the first couple of KB of a body, and that
/// slice is usually mid-`<style>` with no opening tag in view to strip against.
pub fn preview_snippet(parsed: &ParsedMessage, max_chars: usize) -> Option<String> {
    let collapsed = parsed
        .text_plain_body
        .as_deref()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(max_chars).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_amp_html_part_is_a_body_not_an_attachment() {
        let raw = b"From: a@example.com\r\n\
                    Subject: review\r\n\
                    Content-Type: multipart/alternative; boundary=\"b\"\r\n\
                    \r\n\
                    --b\r\n\
                    Content-Type: text/plain\r\n\r\nplain\r\n\
                    --b\r\n\
                    Content-Type: text/x-amp-html\r\n\r\n<html>amp</html>\r\n\
                    --b\r\n\
                    Content-Type: text/html\r\n\r\n<html>real</html>\r\n\
                    --b--\r\n";
        let parsed = parse_message(raw).unwrap();
        assert!(parsed.attachments.is_empty(), "{:?}", parsed.attachments);
        assert_eq!(parsed.html_body.as_deref(), Some("<html>real</html>"));
    }

    #[test]
    fn a_named_html_file_is_still_an_attachment() {
        let raw = b"From: a@example.com\r\n\
                    Subject: report\r\n\
                    Content-Type: multipart/mixed; boundary=\"b\"\r\n\
                    \r\n\
                    --b\r\n\
                    Content-Type: text/plain\r\n\r\nsee attached\r\n\
                    --b\r\n\
                    Content-Type: text/html\r\n\
                    Content-Disposition: attachment; filename=\"report.html\"\r\n\r\n<html>x</html>\r\n\
                    --b--\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("report.html")
        );
    }

    use super::*;

    fn parsed_with(text: Option<&str>, html: Option<&str>) -> ParsedMessage {
        parsed_with_plain(text, text, html)
    }

    fn parsed_with_plain(
        text: Option<&str>,
        plain: Option<&str>,
        html: Option<&str>,
    ) -> ParsedMessage {
        ParsedMessage {
            subject: None,
            from: vec![],
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: vec![],
            date: None,
            message_id: None,
            in_reply_to: vec![],
            references: vec![],
            text_body: text.map(str::to_string),
            text_plain_body: plain.map(str::to_string),
            html_body: html.map(str::to_string),
            attachments: vec![],
        }
    }

    #[test]
    fn preview_uses_plaintext_and_collapses_whitespace() {
        let parsed = parsed_with(Some("Hello   there\n\n  world\t!"), Some("<p>ignored</p>"));
        assert_eq!(
            preview_snippet(&parsed, 100).unwrap(),
            "Hello there world !"
        );
    }

    #[test]
    fn preview_ignores_html_when_there_is_no_plaintext_part() {
        let parsed = parsed_with(None, Some("<p>Real text</p>"));
        assert!(preview_snippet(&parsed, 100).is_none());
    }

    #[test]
    fn html_only_message_has_no_text_plain_body() {
        let raw =
            b"From: a@b.c\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<div>&gt; quoted</div>";
        let parsed = parse_message(raw).unwrap();
        assert!(parsed.text_body.as_deref().unwrap().contains("<div>"));
        assert!(parsed.text_plain_body.is_none());
        assert!(preview_snippet(&parsed, 100).is_none());
    }

    #[test]
    fn multipart_alternative_preview_uses_the_plain_part() {
        let raw = b"From: a@b.c\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nPlain wins\r\n--B\r\nContent-Type: text/html\r\n\r\n<p>markup</p>\r\n--B--\r\n";
        let parsed = parse_message(raw).unwrap();
        assert_eq!(preview_snippet(&parsed, 100).unwrap(), "Plain wins");
    }

    #[test]
    fn preview_truncates_to_max_chars() {
        let parsed = parsed_with(Some(&"ab ".repeat(100)), None);
        assert_eq!(preview_snippet(&parsed, 10).unwrap().chars().count(), 10);
    }

    #[test]
    fn preview_is_none_when_there_is_no_body_at_all() {
        assert!(preview_snippet(&parsed_with(None, None), 100).is_none());
        assert!(preview_snippet(&parsed_with(Some("   \n\t "), None), 100).is_none());
    }

    #[test]
    fn parses_a_simple_plaintext_message() {
        let raw = b"From: Alice <alice@example.com>\r\n\
                     To: Bob <bob@example.com>\r\n\
                     Subject: Hello\r\n\
                     Message-ID: <1@example.com>\r\n\
                     Date: Mon, 1 Jan 2024 12:00:00 +0000\r\n\
                     Content-Type: text/plain\r\n\
                     \r\n\
                     Hi Bob!\r\n";

        let parsed = parse_message(raw).expect("should parse");
        assert_eq!(parsed.subject.as_deref(), Some("Hello"));
        assert_eq!(
            parsed.from,
            vec![Mailbox {
                name: Some("Alice".to_string()),
                address: "alice@example.com".to_string(),
            }]
        );
        assert_eq!(
            parsed.to,
            vec![Mailbox {
                name: Some("Bob".to_string()),
                address: "bob@example.com".to_string(),
            }]
        );
        assert_eq!(parsed.text_body.as_deref(), Some("Hi Bob!\r\n"));
        assert_eq!(parsed.message_id.as_deref(), Some("1@example.com"));
        assert!(parsed.date.is_some());
    }

    #[test]
    fn parses_html_body_and_attachment() {
        let raw = b"From: a@example.com\r\n\
                     To: b@example.com\r\n\
                     Subject: With attachment\r\n\
                     Content-Type: multipart/mixed; boundary=\"BOUNDARY\"\r\n\
                     \r\n\
                     --BOUNDARY\r\n\
                     Content-Type: text/html\r\n\
                     \r\n\
                     <p>hello</p>\r\n\
                     --BOUNDARY\r\n\
                     Content-Type: text/plain; name=\"note.txt\"\r\n\
                     Content-Disposition: attachment; filename=\"note.txt\"\r\n\
                     \r\n\
                     attachment body\r\n\
                     --BOUNDARY--\r\n";

        let parsed = parse_message(raw).expect("should parse");
        assert_eq!(parsed.html_body.as_deref(), Some("<p>hello</p>"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some("note.txt"));
        assert!(!parsed.attachments[0].is_inline);
    }

    #[test]
    fn rejects_oversized_input_without_parsing() {
        let raw = vec![b'a'; MAX_RAW_MESSAGE_BYTES + 1];
        let err = parse_message(&raw).unwrap_err();
        assert!(matches!(err, ParseError::TooLarge { .. }));
    }

    #[test]
    fn returns_no_headers_error_on_garbage() {
        let err = parse_message(b"").unwrap_err();
        assert!(matches!(err, ParseError::NoHeaders));
    }

    /// Not a full exploit reproduction: we never walk into nested messages
    /// (see module docs), so this only checks that top-level parsing of deeply
    /// nested input completes quickly and without panicking.
    #[test]
    fn deeply_nested_rfc822_parts_do_not_hang_or_panic() {
        let mut raw = String::new();
        raw.push_str("From: a@example.com\r\nTo: b@example.com\r\nSubject: nested\r\n");
        let depth = 300;
        for _ in 0..depth {
            raw.push_str("Content-Type: message/rfc822\r\n\r\n");
        }
        raw.push_str("From: innermost@example.com\r\nSubject: bottom\r\n\r\nbody\r\n");

        let start = std::time::Instant::now();
        let _ = parse_message(raw.as_bytes());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "parsing {depth} nested message/rfc822 parts took too long"
        );
    }
}
