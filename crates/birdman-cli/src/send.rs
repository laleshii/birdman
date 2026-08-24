use birdman_backend::{ComposeDraft, OutgoingMessage, Recipient};
use birdman_client::Client;
use birdman_store::MessageId;

fn recipients(raw: &str) -> Vec<Recipient> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| match (part.find('<'), part.find('>')) {
            (Some(open), Some(close)) if close > open => Recipient::new(
                Some(part[..open].trim().trim_matches('"').to_string()),
                part[open + 1..close].trim().to_string(),
            ),
            _ => Recipient::new(None, part.to_string()),
        })
        .collect()
}

/// The name outgoing mail is signed with: `name` from the account's config
/// section, never `display_name`.
///
/// `display_name` labels a mailbox in a sidebar. Falling back to it is what
/// sent mail as `From: Gmail <you@gmail.com>` -- the desktop hit this and the
/// CLI shared the bug, because the store has no column for a signing name and
/// only the config file knows one.
fn signing_name(email: &str) -> Option<String> {
    let birdman_config::Config::Accounts(configured) = birdman_config::load() else {
        return None;
    };
    configured
        .into_iter()
        .find(|a| a.email.eq_ignore_ascii_case(email))
        .and_then(|a| a.name)
}

/// Reads the body from `--body`, or from stdin when it is `-` or absent.
fn body_text(body: Option<&str>) -> Result<String, String> {
    match body {
        Some("-") | None => {
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| e.to_string())?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
    }
}

/// Answers a message, threading it correctly.
///
/// The draft is built by `birdman_backend::reply_draft` from the stored envelope,
/// which is the same code the desktop uses -- so the two cannot disagree about
/// reply-all membership or about honouring `Reply-To`.
pub fn reply(
    client: &Client,
    message: MessageId,
    reply_all: bool,
    body: Option<&str>,
) -> Result<(), String> {
    let target = crate::write::locate(client, message)?;
    let account = client
        .accounts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|a| a.id == target.account)
        .ok_or_else(|| "the message's account is no longer configured".to_string())?;

    let quoted = client
        .body(message)
        .map_err(|e| e.to_string())?
        .and_then(|b| b.text);
    let parsed = birdman_backend::parsed_from_summary(&target.message, quoted);
    let draft = birdman_backend::reply_draft(&parsed, &account.email, reply_all);
    send_draft(client, &account, draft, body_text(body)?)
}

/// Forwards a message to `to`.
pub fn forward(
    client: &Client,
    message: MessageId,
    to: &str,
    body: Option<&str>,
) -> Result<(), String> {
    let target = crate::write::locate(client, message)?;
    let account = client
        .accounts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|a| a.id == target.account)
        .ok_or_else(|| "the message's account is no longer configured".to_string())?;

    let quoted = client
        .body(message)
        .map_err(|e| e.to_string())?
        .and_then(|b| b.text);
    let parsed = birdman_backend::parsed_from_summary(&target.message, quoted);
    let mut draft = birdman_backend::forward_draft(&parsed);
    draft.to = recipients(to);
    if draft.to.is_empty() {
        return Err("no recipients".to_string());
    }
    send_draft(client, &account, draft, body_text(body)?)
}

/// Puts `written` above the draft's quoted original and sends it.
fn send_draft(
    client: &Client,
    account: &birdman_store::Account,
    draft: ComposeDraft,
    written: String,
) -> Result<(), String> {
    let text_body = match written.trim().is_empty() {
        true => draft.body,
        false => format!("{}\n\n{}", written.trim_end(), draft.body),
    };
    let message = OutgoingMessage {
        from: Recipient::new(signing_name(&account.email), account.email.clone()),
        to: draft.to,
        cc: draft.cc,
        bcc: Vec::new(),
        subject: draft.subject,
        text_body,
        in_reply_to: draft.in_reply_to,
        references: draft.references,
        message_id: None,
        date: None,
    };
    let id = client
        .send_blocking(account.id, message)
        .map_err(|e| e.to_string())?;
    println!("queued as {} for {}", id.0, account.email);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn send(
    client: &Client,
    from: Option<&str>,
    to: &str,
    cc: Option<&str>,
    subject: &str,
    body: Option<&str>,
) -> Result<(), String> {
    let accounts = client.accounts().map_err(|e| e.to_string())?;
    let account = match from {
        Some(needle) => {
            let needle = needle.to_lowercase();
            accounts
                .iter()
                .find(|a| a.email.to_lowercase().starts_with(&needle))
                .ok_or_else(|| format!("no account matching {needle:?}"))?
        }
        None if accounts.len() == 1 => &accounts[0],
        None => {
            return Err(format!(
                "several accounts configured -- name one with --from: {}",
                accounts
                    .iter()
                    .map(|a| a.email.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };

    let to = recipients(to);
    if to.is_empty() {
        return Err("no recipients".to_string());
    }

    let text = body_text(body)?;

    let message = OutgoingMessage {
        bcc: Vec::new(),
        from: Recipient::new(signing_name(&account.email), account.email.clone()),
        to,
        cc: cc.map(recipients).unwrap_or_default(),
        subject: subject.to_string(),
        text_body: text,
        in_reply_to: None,
        references: Vec::new(),
        message_id: None,
        date: None,
    };
    let id = client
        .send_blocking(account.id, message)
        .map_err(|e| e.to_string())?;
    println!("queued as {} for {}", id.0, account.email);
    Ok(())
}

pub fn watch(client: &Client, json: bool) -> Result<(), String> {
    use birdman_proto::Event;
    let events = client.subscribe().map_err(|e| e.to_string())?;
    if !json {
        eprintln!("watching for changes (ctrl-c to stop)");
    }
    while let Ok(event) = events.recv_blocking() {
        if json {
            println!("{}", crate::format::event_json(&event));
            continue;
        }
        match event {
            Event::FoldersChanged { account } => {
                println!("folders changed   account {}", account.0)
            }
            Event::MessagesChanged { folder } => println!("messages changed  folder {}", folder.0),
            Event::SyncProgress { account, folder } => println!(
                "syncing           account {} {}",
                account.0,
                folder.unwrap_or_default()
            ),
            Event::SyncIdle { account } => println!("idle              account {}", account.0),
            Event::SyncFailed { account, message } => {
                println!("failed            account {} -- {message}", account.0)
            }
            Event::OutboxChanged { account } => println!("outbox changed    account {}", account.0),
        }
    }
    Ok(())
}
