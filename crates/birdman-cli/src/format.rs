use birdman_proto::{Event, MessageBody};
use birdman_store::{Account, Attachment, Contact, Folder, MessageSummary};

pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt(value: Option<&str>) -> String {
    value.map(quote).unwrap_or_else(|| "null".to_string())
}

pub fn account_json(account: &Account) -> String {
    format!(
        r#"{{"id":{},"display_name":{},"email":{}}}"#,
        account.id.0,
        quote(&account.display_name),
        quote(&account.email)
    )
}

pub fn folder_json(folder: &Folder, unread: u32) -> String {
    format!(
        r#"{{"id":{},"account_id":{},"path":{},"unread":{}}}"#,
        folder.id.0,
        folder.account_id.0,
        quote(&folder.imap_path),
        unread
    )
}

pub fn message_json(message: &MessageSummary) -> String {
    format!(
        r#"{{"id":{},"folder_id":{},"date":{},"from":{},"subject":{},"seen":{},"flagged":{},"has_attachments":{}}}"#,
        message.id.0,
        message.folder_id.0,
        message
            .date
            .map(|d| d.to_string())
            .unwrap_or_else(|| "null".into()),
        opt(message
            .from_name
            .as_deref()
            .or(message.from_addr.as_deref())),
        opt(message.subject.as_deref()),
        message.flags.seen,
        message.flags.flagged,
        message.has_attachments,
    )
}

pub fn attachment_json(attachment: &Attachment) -> String {
    format!(
        r#"{{"filename":{},"content_type":{},"size":{},"path":{}}}"#,
        quote(&attachment.filename),
        opt(attachment.content_type.as_deref()),
        attachment.size,
        opt(attachment.path.as_deref()),
    )
}

pub fn contact_json(contact: &Contact) -> String {
    format!(
        r#"{{"address":{},"name":{},"seen":{},"last_seen":{}}}"#,
        quote(&contact.address),
        opt(contact.name.as_deref()),
        contact.seen,
        contact.last_seen,
    )
}

/// A coarse size, because the question a listing answers is "is this going to
/// be a problem to send on", not how many bytes it is.
pub fn byte_size(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// The last path component of a sender-chosen filename, so `--save` cannot be
/// talked into writing outside the directory it was given.
pub fn safe_basename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment".to_string()
    } else {
        cleaned.to_string()
    }
}

pub fn body_json(body: &MessageBody) -> String {
    format!(
        r#"{{"text":{},"html":{}}}"#,
        opt(body.text.as_deref()),
        opt(body.html.as_deref())
    )
}

pub fn event_json(event: &Event) -> String {
    match event {
        Event::FoldersChanged { account } => {
            format!(r#"{{"event":"folders_changed","account":{}}}"#, account.0)
        }
        Event::MessagesChanged { folder } => {
            format!(r#"{{"event":"messages_changed","folder":{}}}"#, folder.0)
        }
        Event::SyncProgress { account, folder } => format!(
            r#"{{"event":"sync_progress","account":{},"folder":{}}}"#,
            account.0,
            opt(folder.as_deref())
        ),
        Event::SyncIdle { account } => {
            format!(r#"{{"event":"sync_idle","account":{}}}"#, account.0)
        }
        Event::SyncFailed { account, message } => format!(
            r#"{{"event":"sync_failed","account":{},"message":{}}}"#,
            account.0,
            quote(message)
        ),
        Event::OutboxChanged { account } => {
            format!(r#"{{"event":"outbox_changed","account":{}}}"#, account.0)
        }
    }
}

pub fn when(date: Option<i64>) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(date) = date.and_then(|d| Local.timestamp_opt(d, 0).single()) else {
        return "     ".to_string();
    };
    let today = Local::now();
    if date.year() == today.year() && date.ordinal() == today.ordinal() {
        date.format("%H:%M").to_string()
    } else {
        date.format("%d %b").to_string()
    }
}

/// Counts chars, not bytes: a byte cut would split a multi-byte char and
/// misalign the column.
pub fn truncate(value: &str, width: usize) -> String {
    let flat = value.replace(['\n', '\r', '\t'], " ");
    if flat.chars().count() <= width {
        return flat;
    }
    let kept: String = flat.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// `mail-parser` reports the HTML part as the text body when a message has no
/// `text/plain` alternative, so the store's text column needs checking.
pub fn looks_like_html(value: &str) -> bool {
    let head = value.trim_start().to_ascii_lowercase();
    // An XML prolog or a comment can sit in front of the real opening tag --
    // real mail does both -- so a prefix test alone reports plain text and the
    // terminal prints a wall of markup.
    let head = head.strip_prefix("<?xml").map_or(head.as_str(), |rest| {
        rest.find("?>")
            .map_or(rest, |end| rest[end + 2..].trim_start())
    });
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.starts_with("<head")
        || head.get(..2048).unwrap_or(head).contains("<html")
}

pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<&str> = None;

    while let Some(at) = rest.find('<') {
        let (text, tail) = rest.split_at(at);
        if skip_until.is_none() {
            out.push_str(text);
        }
        let end = tail.find('>').map(|e| e + 1).unwrap_or(tail.len());
        let tag = &tail[..end];
        let lower = tag.to_ascii_lowercase();

        if let Some(closing) = skip_until {
            if lower.starts_with(closing) {
                skip_until = None;
            }
        } else if lower.starts_with("<style") {
            skip_until = Some("</style");
        } else if lower.starts_with("<script") {
            skip_until = Some("</script");
        } else if lower.starts_with("<head") {
            skip_until = Some("</head");
        } else if BREAKS.iter().any(|b| lower.starts_with(b)) {
            out.push('\n');
        }
        rest = &tail[end..];
    }
    if skip_until.is_none() {
        out.push_str(rest);
    }

    collapse(&unescape(&out))
}

const BREAKS: &[&str] = &[
    "<br", "</p", "<p", "</div", "</tr", "</h1", "</h2", "</h3", "</li", "<li", "</table",
];

fn unescape(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn collapse(value: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() && lines.last().map(|l: &&str| l.is_empty()).unwrap_or(true) {
            continue;
        }
        lines.push(trimmed);
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_what_would_break_a_parser() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quote("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn absent_values_are_null_not_empty_strings() {
        assert_eq!(opt(None), "null");
        assert_eq!(opt(Some("")), "\"\"");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("abcdefghij", 5), "abcd\u{2026}");
        assert_eq!(
            truncate("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}", 3),
            "\u{e9}\u{e9}\u{2026}"
        );
    }

    #[test]
    fn newlines_never_reach_a_list_row() {
        assert_eq!(truncate("two\nlines", 20), "two lines");
    }

    #[test]
    fn a_text_body_that_is_really_markup_is_recognised() {
        assert!(looks_like_html("<!DOCTYPE html>\n<html>"));
        assert!(looks_like_html("  <html><body>hi"));
        assert!(!looks_like_html("Dear Ada,\n\nYour parcel"));
        assert!(!looks_like_html("a < b and c > d"));
    }

    /// A real ticketing receipt opens with an XML prolog, which made the
    /// prefix test report plain text and printed the markup verbatim.
    #[test]
    fn an_xml_prolog_does_not_hide_the_html() {
        let body = "<?xml version=\"1.0\" encoding=\"utf-16\"?>\n<html xmlns=\"x\">\n<head>";
        assert!(looks_like_html(body));
        assert!(!looks_like_html(
            "<?xml version=\"1.0\"?>\n<invoice><total>9</total></invoice>"
        ));
    }

    #[test]
    fn style_and_script_contents_never_reach_the_terminal() {
        let html =
            "<html><head><style>.a{color:red}</style></head><body><p>Hello</p></body></html>";
        let text = html_to_text(html);
        assert!(!text.contains("color:red"), "{text}");
        assert!(text.contains("Hello"), "{text}");
    }

    #[test]
    fn block_boundaries_become_line_breaks() {
        let text = html_to_text("<p>one</p><p>two</p><br>three");
        assert_eq!(text, "one\n\ntwo\n\nthree");
    }

    #[test]
    fn entities_are_decoded_and_blank_runs_squeezed() {
        let text = html_to_text("<div>a &amp; b</div><div></div><div></div><div>c</div>");
        assert_eq!(
            text, "a & b\n\nc",
            "three empty divs collapse to one separator"
        );
    }
}
