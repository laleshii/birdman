use birdman_client::Client;
use birdman_store::{AccountId, FolderId, MessageId, MessageSummary, SpecialUse};

pub struct Target {
    pub message: MessageSummary,
    pub account: AccountId,
}

pub fn locate(client: &Client, message: MessageId) -> Result<Target, String> {
    let found = client
        .message(message)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no message {}", message.0))?;
    let account = client
        .folders(None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|f| f.id == found.folder_id)
        .map(|f| f.account_id)
        .ok_or_else(|| format!("message {} is in a folder that no longer exists", message.0))?;
    Ok(Target {
        message: found,
        account,
    })
}

/// A special-use folder wins a tie: on Gmail an account can hold both a user
/// folder named `Trash` and `[Google Mail]/Trash`, and the second is meant.
fn folder_named(client: &Client, account: AccountId, name: &str) -> Result<FolderId, String> {
    let matches: Vec<_> = client
        .folders(Some(account))
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|f| f.imap_path.eq_ignore_ascii_case(name) || f.name.eq_ignore_ascii_case(name))
        .collect();

    if matches.len() > 1 {
        let wanted = match name.to_ascii_lowercase().as_str() {
            "trash" | "bin" | "deleted" => Some(SpecialUse::Trash),
            "archive" | "all" | "all mail" => Some(SpecialUse::Archive),
            "sent" => Some(SpecialUse::Sent),
            "drafts" => Some(SpecialUse::Drafts),
            "spam" | "junk" => Some(SpecialUse::Junk),
            _ => None,
        };
        if let Some(wanted) = wanted {
            let special: Vec<_> = matches
                .iter()
                .filter(|f| f.special_use == Some(wanted))
                .collect();
            if let [one] = special.as_slice() {
                return Ok(one.id);
            }
        }
    }
    match matches.as_slice() {
        [one] => Ok(one.id),
        [] => Err(format!(
            "no folder matching {name:?} on that account -- try `birdman folders`"
        )),
        several => Err(format!(
            "{name:?} matches {} folders: {}",
            several.len(),
            several
                .iter()
                .map(|f| f.imap_path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Fetches the body if it is not cached and marks the message read, which is
/// what opening one in any client means. `peek` does the fetch without the
/// flag, for reading without changing what the mailbox says.
pub fn open(client: &Client, message: MessageId, peek: bool) -> Result<(), String> {
    let target = locate(client, message)?;
    let needs_body = !target.message.body_fetched;
    let mark_read = !peek && !target.message.flags.seen;
    if !needs_body && !mark_read {
        return Ok(());
    }
    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::OpenMessage {
                message,
                fetch_body: needs_body,
                mark_read,
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Sets or clears `\Seen`.
pub fn mark_seen(client: &Client, message: MessageId, seen: bool) -> Result<(), String> {
    let target = locate(client, message)?;
    let mut flags = target.message.flags;
    if flags.seen == seen {
        return Ok(());
    }
    flags.seen = seen;
    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::SetFlags { message, flags },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn flag(client: &Client, message: MessageId, on: bool) -> Result<(), String> {
    let target = locate(client, message)?;
    // `SetFlags` replaces the whole set: sending only `flagged` would silently
    // mark the message unread.
    let mut flags = target.message.flags;
    flags.flagged = on;
    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::SetFlags { message, flags },
        )
        .map_err(|e| e.to_string())?;
    println!("{} {}", if on { "flagged" } else { "unflagged" }, message.0);
    Ok(())
}

pub fn move_to(client: &Client, message: MessageId, folder: &str) -> Result<(), String> {
    let target = locate(client, message)?;
    let to_folder = folder_named(client, target.account, folder)?;
    if to_folder == target.message.folder_id {
        return Err("already there".to_string());
    }
    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::MoveMessage { message, to_folder },
        )
        .map_err(|e| e.to_string())?;
    println!("moved {} to {folder}", message.0);
    Ok(())
}

pub fn archive(client: &Client, message: MessageId) -> Result<(), String> {
    let target = locate(client, message)?;
    // Gmail exposes no `\Archive`: archiving there is a move into All Mail.
    let folders = client
        .folders(Some(target.account))
        .map_err(|e| e.to_string())?;
    let to_folder = folders
        .iter()
        .find(|f| f.special_use == Some(SpecialUse::Archive))
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.special_use == Some(SpecialUse::All))
        })
        .map(|f| f.id)
        .ok_or_else(|| "no archive folder on that account".to_string())?;

    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::MoveMessage { message, to_folder },
        )
        .map_err(|e| e.to_string())?;
    println!("archived {}", message.0);
    Ok(())
}

pub fn delete(client: &Client, message: MessageId) -> Result<(), String> {
    let target = locate(client, message)?;
    client
        .execute_blocking(
            target.account,
            birdman_backend::Command::DeleteMessage { message },
        )
        .map_err(|e| e.to_string())?;
    println!("deleted {}", message.0);
    Ok(())
}

pub fn sync(client: &Client, folder: Option<&str>) -> Result<(), String> {
    let folders = client.folders(None).map_err(|e| e.to_string())?;
    let targets: Vec<_> = match folder {
        Some(name) => folders
            .iter()
            .filter(|f| f.imap_path.eq_ignore_ascii_case(name) || f.name.eq_ignore_ascii_case(name))
            .collect(),
        None => folders
            .iter()
            .filter(|f| f.imap_path.eq_ignore_ascii_case("INBOX"))
            .collect(),
    };
    if targets.is_empty() {
        return Err("nothing matching that folder -- try `birdman folders`".to_string());
    }

    let mut listed: Vec<AccountId> = Vec::new();
    for target in targets {
        if !listed.contains(&target.account_id) {
            listed.push(target.account_id);
            client
                .execute_blocking(target.account_id, birdman_backend::Command::ListFolders)
                .map_err(|e| e.to_string())?;
        }
        let outcome = client
            .execute_blocking(
                target.account_id,
                birdman_backend::Command::SyncFolder { folder: target.id },
            )
            .map_err(|e| e.to_string())?;
        println!(
            "synced {}{}",
            target.imap_path,
            match outcome.bodies_fetched {
                0 => String::new(),
                n => format!(" ({n} bodies)"),
            }
        );
    }
    Ok(())
}
