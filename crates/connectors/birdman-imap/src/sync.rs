use std::sync::{Arc, Mutex};

use async_imap::types::{Flag, NameAttribute};
use birdman_store::{
    AccountId, Folder, FolderId, MessageFlags, MessageId, NewFolder, SpecialUse, Store,
};
use futures_util::TryStreamExt;

use crate::connect::{connect_for_account, ImapSession};
use crate::{AccountConfig, CoreError};

/// `BODY.PEEK[]`, so fetching never marks the message `\Seen`.
///
/// A uid means nothing except relative to the selected mailbox, and this
/// function cannot see which one that is. When a caller had the wrong one
/// selected, uid 4102 returned a *different real message* and it was stored
/// under this id -- silently and permanently, since the store then had a body.
/// Two accounts both having an `INBOX` is all it takes. Hence the `Message-ID`
/// check: it does not depend on every caller having selected correctly.
pub async fn fetch_message_body(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    message_id: MessageId,
    uid: u32,
) -> Result<(), CoreError> {
    let mut stream = session.uid_fetch(uid.to_string(), "(BODY.PEEK[])").await?;
    let fetch = stream.try_next().await?.ok_or(CoreError::MessageMissing)?;
    let body = fetch.body().ok_or(CoreError::MessageMissing)?;
    let parsed = birdman_mime::parse_message(body)?;
    // Drained, not just dropped. The stream ends at the tagged completion, and
    // abandoning it early leaves that in the connection buffer for the next
    // command to read as its own reply -- so every backfill left the session one
    // response behind and the next body arrived under the previous message's
    // uid. The bulk fetches below consume theirs with `while let`; this one
    // takes a single item and has to finish the job by hand.
    while stream.try_next().await?.is_some() {}
    drop(stream);

    let expected = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store
            .get_message(message_id)?
            .and_then(|m| m.message_id_header)
    };
    // Only when both sides have one: a message with no `Message-ID` is
    // malformed but real, and never showing its body would be worse.
    if let (Some(expected), Some(fetched)) = (expected.as_deref(), parsed.message_id.as_deref()) {
        if expected != fetched {
            log::error!(
                "refusing body for message {message_id:?} (uid {uid}): server returned {fetched:?}, \
                 expected {expected:?} -- not storing it under this id"
            );
            return Err(CoreError::MessageMissing);
        }
    }

    let mut store = store.lock().expect("birdman-store mutex poisoned");
    store.store_message_body(message_id, &parsed)?;
    Ok(())
}

/// A replace, not a merge: callers pass the complete target flag set.
pub async fn set_flags_remote(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    message_id: MessageId,
    uid: u32,
    flags: MessageFlags,
) -> Result<(), CoreError> {
    let mut parts = Vec::new();
    if flags.seen {
        parts.push("\\Seen");
    }
    if flags.flagged {
        parts.push("\\Flagged");
    }
    if flags.answered {
        parts.push("\\Answered");
    }
    if flags.deleted {
        parts.push("\\Deleted");
    }
    if flags.draft {
        parts.push("\\Draft");
    }
    let query = format!("FLAGS ({})", parts.join(" "));
    session
        .uid_store(uid.to_string(), query)
        .await?
        .try_collect::<Vec<_>>()
        .await?;

    let store = store.lock().expect("birdman-store mutex poisoned");
    store.set_flags(message_id, flags)?;
    Ok(())
}

/// A bare `EXPUNGE` removes *every* message carrying `\Deleted`, and that flag
/// is shared state -- another client can have left it on mail nobody meant to
/// destroy. `UID EXPUNGE` (RFC 4315) names one. Needs UIDPLUS, so it falls
/// back the same way the move falls back from `UID MOVE`.
async fn expunge_uid(session: &mut ImapSession, uid: u32) -> Result<(), CoreError> {
    let scoped = match session.uid_expunge(uid.to_string()).await {
        Ok(stream) => {
            stream.try_collect::<Vec<_>>().await?;
            true
        }
        Err(_) => false,
    };
    if !scoped {
        session.expunge().await?.try_collect::<Vec<_>>().await?;
    }
    Ok(())
}

/// `UID MOVE` (RFC 6851) first, falling back to copy-then-delete: `MOVE` is an
/// extension and a server without it fails rather than degrading. Copy-first
/// ordering is load bearing -- deleting first and failing the copy loses the
/// message outright.
pub async fn move_message_remote(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    message_id: MessageId,
    uid: u32,
    current_flags: MessageFlags,
    target_mailbox: &str,
) -> Result<(), CoreError> {
    let uid_set = uid.to_string();
    if session.uid_mv(&uid_set, target_mailbox).await.is_err() {
        session.uid_copy(&uid_set, target_mailbox).await?;
        let mut flags = current_flags;
        flags.deleted = true;
        set_flags_remote(session, store, message_id, uid, flags).await?;
        expunge_uid(session, uid).await?;
    }

    // The copy in the target folder is picked up by that folder's own sync.
    let store = store.lock().expect("birdman-store mutex poisoned");
    store.delete_message(message_id)?;
    Ok(())
}

/// `\Deleted` + `EXPUNGE` means "remove from this mailbox", not "move to
/// Trash", and what happens next is the server's choice -- Gmail's default
/// setting *archives*, so the mail stays in All Mail and Trash never sees it.
/// Hence the explicit move.
///
/// Expunging is right in exactly two cases: the message is already in Trash,
/// or the account has no Trash folder at all.
pub async fn delete_message_remote(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    message_id: MessageId,
    folder_id: FolderId,
    uid: u32,
    current_flags: MessageFlags,
) -> Result<(), CoreError> {
    // Scoped: the awaits below take this lock again themselves.
    let trash_path = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        let current = store.get_folder(folder_id)?;
        let in_trash = current
            .as_ref()
            .is_some_and(|f| f.special_use == Some(SpecialUse::Trash));
        match current {
            Some(folder) if !in_trash => store
                .list_folders(folder.account_id)?
                .into_iter()
                .find(|f| f.special_use == Some(SpecialUse::Trash))
                .map(|f| f.imap_path),
            _ => None,
        }
    };

    if let Some(target) = trash_path {
        return move_message_remote(session, store, message_id, uid, current_flags, &target).await;
    }

    let mut flags = current_flags;
    flags.deleted = true;
    set_flags_remote(session, store, message_id, uid, flags).await?;
    expunge_uid(session, uid).await?;

    let store = store.lock().expect("birdman-store mutex poisoned");
    store.delete_message(message_id)?;
    Ok(())
}

/// Diagnostic only: `sync_folder_list` upserts what it finds, so comparing its
/// result against the store cannot reveal rows the server no longer offers.
pub async fn list_folder_paths(session: &mut ImapSession) -> Result<Vec<String>, CoreError> {
    let names: Vec<_> = session.list(None, Some("*")).await?.try_collect().await?;
    Ok(names.iter().map(|n| n.name().to_string()).collect())
}

pub async fn sync_folder_list(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    account_id: AccountId,
) -> Result<Vec<Folder>, CoreError> {
    let names: Vec<_> = session.list(None, Some("*")).await?.try_collect().await?;

    adopt_renamed_folders(session, store, account_id, &names).await?;

    let mut selectable = Vec::new();
    for name in &names {
        let path = name.name().to_string();
        let delimiter = name.delimiter().map(|d| d.to_string());
        let no_select = name
            .attributes()
            .iter()
            .any(|a| matches!(a, NameAttribute::NoSelect));
        let special_use = name
            .attributes()
            .iter()
            .find_map(special_use_from_attribute);

        let folder_id = {
            let store = store.lock().expect("birdman-store mutex poisoned");
            store.upsert_folder(&NewFolder {
                account_id,
                name: &path,
                imap_path: &path,
                delimiter: delimiter.as_deref(),
                subscribed: true,
                special_use,
            })?
        };

        if !no_select {
            selectable.push(Folder {
                id: folder_id,
                account_id,
                name: path.clone(),
                imap_path: path,
                delimiter,
                uid_validity: None,
                uid_next: None,
                highest_modseq: None,
                subscribed: true,
                special_use,
            });
        }
    }

    prune_vanished_folders(store, account_id, &names)?;
    Ok(selectable)
}

/// A rename is indistinguishable from "one folder vanished, another appeared"
/// except by `UIDVALIDITY`, which a rename preserves. Only *unambiguous*
/// matches are followed -- exactly one vanished folder and one new path sharing
/// a uidvalidity -- so this never merges two folders by accident.
async fn adopt_renamed_folders(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    account_id: AccountId,
    names: &[async_imap::types::Name],
) -> Result<(), CoreError> {
    let listed: std::collections::HashSet<&str> = names.iter().map(|n| n.name()).collect();

    let stored = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.list_folders(account_id)?
    };
    let vanished: Vec<_> = stored
        .iter()
        .filter(|f| !listed.contains(f.imap_path.as_str()))
        .filter(|f| f.uid_validity.is_some_and(|v| v != 0))
        .collect();
    if vanished.is_empty() {
        return Ok(());
    }

    let known: std::collections::HashSet<&str> =
        stored.iter().map(|f| f.imap_path.as_str()).collect();
    let fresh: Vec<&str> = names
        .iter()
        .map(|n| n.name())
        .filter(|path| !known.contains(path))
        .collect();
    if fresh.is_empty() {
        return Ok(());
    }

    // `STATUS` does not disturb the session's currently selected mailbox.
    let mut fresh_validity: Vec<(&str, u32)> = Vec::new();
    for path in fresh {
        match session.status(path, "(UIDVALIDITY)").await {
            Ok(status) => {
                if let Some(validity) = status.uid_validity {
                    fresh_validity.push((path, validity));
                }
            }
            Err(err) => log::debug!("status failed for {path}: {err}"),
        }
    }

    for folder in vanished {
        let validity = folder.uid_validity.expect("filtered above");
        let mut matches = fresh_validity.iter().filter(|(_, v)| *v == validity);
        let (Some((new_path, _)), None) = (matches.next(), matches.next()) else {
            continue;
        };
        if stored
            .iter()
            .filter(|f| f.uid_validity == Some(validity))
            .count()
            > 1
        {
            continue;
        }
        log::info!(
            "folder renamed on the server: {} -> {new_path}",
            folder.imap_path
        );
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.rename_folder(folder.id, new_path, new_path)?;
    }
    Ok(())
}

/// Gmail migrates accounts between the `[Gmail]` and `[Google Mail]`
/// namespaces, so without this an account that lived through one ends up with
/// two complete sets of special-use folders.
///
/// **Guarded on a non-empty listing.** A `LIST` returning nothing is far more
/// likely a truncated response than an account with no folders, and acting on
/// it would wipe every cached message.
fn prune_vanished_folders(
    store: &Arc<Mutex<Store>>,
    account_id: AccountId,
    names: &[async_imap::types::Name],
) -> Result<(), CoreError> {
    if names.is_empty() {
        return Ok(());
    }
    let listed: std::collections::HashSet<&str> = names.iter().map(|n| n.name()).collect();

    let mut store = store.lock().expect("birdman-store mutex poisoned");
    let stale: Vec<_> = store
        .list_folders(account_id)?
        .into_iter()
        .filter(|folder| !listed.contains(folder.imap_path.as_str()))
        .collect();

    for folder in stale {
        log::info!(
            "removing folder no longer on the server: {}",
            folder.imap_path
        );
        store.delete_folder(folder.id)?;
    }
    Ok(())
}

/// RFC 6154 `SPECIAL-USE` attributes. `\Inbox` is not among them; `INBOX` is
/// identified by its own reserved name instead.
fn special_use_from_attribute(attr: &NameAttribute<'_>) -> Option<SpecialUse> {
    match attr {
        NameAttribute::Drafts => Some(SpecialUse::Drafts),
        NameAttribute::Sent => Some(SpecialUse::Sent),
        NameAttribute::Flagged => Some(SpecialUse::Flagged),
        NameAttribute::Junk => Some(SpecialUse::Junk),
        NameAttribute::Trash => Some(SpecialUse::Trash),
        NameAttribute::Archive => Some(SpecialUse::Archive),
        NameAttribute::All => Some(SpecialUse::All),
        _ => None,
    }
}

pub struct FolderSyncResult {
    pub new_uids: Vec<u32>,
    /// Ascending UID order, so the newest are at the end.
    pub new_messages: Vec<(MessageId, u32)>,
}

/// `BODY.PEEK[HEADER]`, never `BODY[HEADER]`: syncing must not mark messages
/// `\Seen`. The preview rides along on the same FETCH as a truncated
/// `BODY.PEEK[TEXT]`, capped so envelope sync does not become a body download.
pub async fn sync_folder(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    account_id: AccountId,
    folder_id: FolderId,
    imap_path: &str,
) -> Result<FolderSyncResult, CoreError> {
    // RFC 7162 requires clients to check CAPABILITY before adding the
    // CONDSTORE parameter. Some servers reject the extended SELECT outright
    // instead of ignoring it (GreenMail is one), so fall back to plain SELECT.
    // No HIGHESTMODSEQ still means "reconcile all" below.
    let supports_condstore = session.capabilities().await?.has_str("CONDSTORE");
    let mailbox = if supports_condstore {
        session.select_condstore(imap_path).await?
    } else {
        session.select(imap_path).await?
    };
    let server_uid_validity = mailbox.uid_validity;
    let server_uid_next = mailbox.uid_next.unwrap_or(0);

    let stored = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.get_folder(folder_id)?
    };
    let stored_uid_validity = stored.as_ref().and_then(|f| f.uid_validity);
    let mut stored_modseq = stored.as_ref().and_then(|f| f.highest_modseq);

    // Both sides must have a value. This read `unwrap_or(0)` once, so a
    // `SELECT` that omitted the response became zero, matched no stored
    // validity, and wiped the folder -- an INBOX went from 8,824 messages to
    // 329. Silence must mean "carry on", never "start again".
    if let (Some(stored), Some(server)) = (stored_uid_validity, server_uid_validity) {
        if stored != server {
            log::warn!(
                "{imap_path} reissued its uids ({stored} -> {server}); re-downloading the folder"
            );
            let store = store.lock().expect("birdman-store mutex poisoned");
            store.clear_folder_messages(folder_id)?;
            // A modseq means nothing outside the uid space it was seen in.
            store.set_folder_modseq(folder_id, None)?;
            stored_modseq = None;
        }
    }
    // Only what the server actually said: storing a zero would guarantee the
    // next sync saw a mismatch and wiped the folder for real.
    if let Some(validity) = server_uid_validity {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.set_folder_uid_state(folder_id, validity, server_uid_next)?;
    }

    let start_uid = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.max_uid(folder_id)?.map(|u| u + 1).unwrap_or(1)
    };

    let mut new_uids = Vec::new();
    let mut new_messages = Vec::new();
    if mailbox.exists > 0 {
        let range = format!("{start_uid}:*");
        // No BODYSTRUCTURE here, deliberately -- see the note below.
        let query =
            format!("(UID FLAGS BODY.PEEK[HEADER] BODY.PEEK[TEXT]<0.{PREVIEW_FETCH_BYTES}>)");
        let mut stream = session.uid_fetch(&range, query).await?;
        while let Some(fetch) = stream.try_next().await? {
            let Some(uid) = fetch.uid else { continue };
            // Some servers echo back UIDs outside the requested range when the
            // mailbox is empty or the range is degenerate.
            if uid < start_uid {
                continue;
            }
            let Some(header) = fetch.header() else {
                continue;
            };
            let Ok(parsed) = birdman_mime::parse_message(header) else {
                continue;
            };
            let flags = to_message_flags(fetch.flags());
            let preview = fetch
                .text()
                .and_then(|text| preview_from_fragment(header, text));

            let store = store.lock().expect("birdman-store mutex poisoned");
            let message_id =
                store.upsert_message_envelope(account_id, folder_id, uid, &parsed, flags)?;
            if let Some(preview) = &preview {
                store.set_message_preview(message_id, preview)?;
            }
            drop(store);
            new_uids.push(uid);
            new_messages.push((message_id, uid));
        }
    }

    reconcile_existing(
        session,
        store,
        folder_id,
        mailbox.exists,
        stored_modseq,
        mailbox.highest_modseq,
    )
    .await?;

    if let Some(modseq) = mailbox.highest_modseq {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.set_folder_modseq(folder_id, Some(modseq))?;
    }

    Ok(FolderSyncResult {
        new_uids,
        new_messages,
    })
}

// **Do not add BODYSTRUCTURE to the FETCH above.** It is the obvious way to
// spot attachments without downloading, and it was used for that until Gmail
// returned `BODYSTRUCTURE ("ALTERNATIVE" ("CHARSET" "UTF-8") NIL NIL)` for a
// real message -- malformed, since RFC 3501 puts a multipart's child bodies
// before the subtype. `imap-proto` rejects it from the response stream rather
// than from one message's data, which takes down the entire folder's FETCH and
// leaves the stream mid-parse with no way to skip and carry on.
//
// Attachment state is set from the real body instead, in
// `Store::store_message_body`, so the marker appears once a message has been
// opened. A genuine downgrade, and still better than a fetch item that can
// silently stop a mailbox syncing.

/// The header block and the body fragment are **concatenated and parsed
/// together**, never separately: a bare `BODY[TEXT]` fragment is undecodable in
/// isolation, since its Content-Type, boundary and transfer encoding are all
/// declared in headers it does not contain.
fn preview_from_fragment(header: &[u8], text: &[u8]) -> Option<String> {
    let mut raw = Vec::with_capacity(header.len() + text.len() + BOUNDARY_SLACK);
    raw.extend_from_slice(header);
    raw.extend_from_slice(text);
    // Close every open multipart, innermost first. `mail-parser` will not apply
    // a part's Content-Transfer-Encoding until it has seen that part end, and
    // the fetch cuts mid-part; the symptom is a preview that looks like text
    // but still carries its raw encoding ("Hello=0Athere").
    //
    // A loop, not one append: `multipart/mixed` wrapping a
    // `multipart/alternative` is ordinary, and closing only the outer one
    // leaves the part undecoded *and* leaks the delimiter into the preview.
    // Boundaries appear outermost-first, so reversing closes innermost-first.
    for boundary in multipart_boundaries(&raw).into_iter().rev() {
        raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    }
    let parsed = birdman_mime::parse_message(&raw).ok()?;
    birdman_mime::preview_snippet(&parsed, PREVIEW_CHARS)
}

/// Outermost first. Scans the whole fragment, not just the header block: a
/// nested multipart declares its boundary in a *part* header, inside the body.
fn multipart_boundaries(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(hit) = lower[from..].find("boundary=") {
        let at = from + hit + "boundary=".len();
        from = at;
        let rest = text[at..].trim_start();
        let value = match rest.strip_prefix('"') {
            Some(quoted) => quoted.find('"').map(|end| &quoted[..end]),
            None => rest.split([';', '\r', '\n', ' ']).next(),
        };
        if let Some(value) = value {
            if !value.is_empty() && !found.iter().any(|b| b == value) {
                found.push(value.to_string());
                if found.len() >= MAX_BOUNDARIES {
                    break;
                }
            }
        }
    }
    found
}

const MAX_BOUNDARIES: usize = 8;

const BOUNDARY_SLACK: usize = 128;

/// Large enough to clear the MIME preamble and part headers and still reach
/// prose; small enough that syncing a big mailbox is not a body download.
const PREVIEW_FETCH_BYTES: usize = 2048;

const PREVIEW_CHARS: usize = 200;

/// Updates flags, and removes anything the server no longer has.
///
/// The removal half matters as much as the flags: the envelope pass only
/// fetches uids above the highest stored, so without this a message deleted on
/// the server stays forever and a moved one shows up in both folders.
async fn reconcile_existing(
    session: &mut ImapSession,
    store: &Arc<Mutex<Store>>,
    folder_id: FolderId,
    server_exists: u32,
    stored_modseq: Option<u64>,
    server_modseq: Option<u64>,
) -> Result<(), CoreError> {
    // HIGHESTMODSEQ covers every metadata change in the mailbox, so an
    // unchanged value means there is provably nothing to reconcile. On Gmail's
    // All Mail that is one command instead of a fetch over 40,000 messages.
    if let (Some(stored), Some(server)) = (stored_modseq, server_modseq) {
        if stored == server && stored != 0 {
            return Ok(());
        }
    }

    // `UID SEARCH ALL` returns just the numbers, so it is the cheap half and
    // the only half needed to spot deletions and moves.
    let seen = session.uid_search("ALL").await?;

    let query = match stored_modseq {
        Some(stored) if server_modseq.is_some() => format!("(UID FLAGS) (CHANGEDSINCE {stored})"),
        _ => "(UID FLAGS)".to_string(),
    };
    let mut stream = session.uid_fetch("1:*", query).await?;
    while let Some(fetch) = stream.try_next().await? {
        let Some(uid) = fetch.uid else { continue };
        let flags = to_message_flags(fetch.flags());
        let message_id = {
            let store = store.lock().expect("birdman-store mutex poisoned");
            store.message_id_for_uid(folder_id, uid)?
        };
        if let Some(message_id) = message_id {
            let store = store.lock().expect("birdman-store mutex poisoned");
            store.set_flags(message_id, flags)?;
        }
    }
    drop(stream);

    // Both guards exist so a partial or empty listing is never mistaken for
    // "the rest were deleted". A response that completes but says nothing while
    // the mailbox claims to hold messages is not worth acting on.
    if seen.is_empty() && server_exists > 0 {
        log::warn!("skipping message reconcile: server reported {server_exists} messages but listed no uids");
        return Ok(());
    }

    let stored = {
        let store = store.lock().expect("birdman-store mutex poisoned");
        store.message_uids(folder_id)?
    };
    let vanished: Vec<_> = stored
        .into_iter()
        .filter(|(_, uid)| !seen.contains(uid))
        .collect();
    if !vanished.is_empty() {
        log::info!(
            "removing {} message(s) no longer on the server",
            vanished.len()
        );
        let store = store.lock().expect("birdman-store mutex poisoned");
        for (message_id, _) in vanished {
            store.delete_message(message_id)?;
        }
    }
    Ok(())
}

fn to_message_flags<'a>(flags: impl Iterator<Item = Flag<'a>>) -> MessageFlags {
    let mut out = MessageFlags::default();
    for flag in flags {
        match flag {
            Flag::Seen => out.seen = true,
            Flag::Flagged => out.flagged = true,
            Flag::Answered => out.answered = true,
            Flag::Deleted => out.deleted = true,
            Flag::Draft => out.draft = true,
            _ => {}
        }
    }
    out
}

/// Files a fully rendered message in an IMAP folder, marked `\Seen` -- after
/// sending, this is how the Sent copy lands.
///
/// A dedicated connection rather than the session cache: `APPEND` targets a
/// mailbox by name and needs no `SELECT`, so borrowing the cached session
/// would only force whatever it had open to be selected again. Sends are rare
/// enough that a fresh login per copy is the cheaper habit.
///
/// The folder must exist (servers create Sent themselves and tag it
/// SPECIAL-USE); callers resolve it from the store first. Logout is best
/// effort -- the connection closes under us either way.
pub async fn append_message(
    config: &AccountConfig,
    auth: &Arc<dyn birdman_auth::AuthAdapter>,
    mailbox_path: &str,
    rfc822: &[u8],
) -> Result<(), CoreError> {
    crate::with_timeout(async move {
        let mut session = connect_for_account(config, auth).await?;
        let appended = session
            .append(mailbox_path, Some("(\\Seen)"), None, rfc822)
            .await;
        let _ = session.logout().await;
        appended.map_err(CoreError::from)
    })
    .await
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    const PLAIN_HEADER: &[u8] = b"From: a@b.c\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n";

    #[test]
    fn finds_boundaries_outermost_first_and_dedupes() {
        let raw = b"Content-Type: multipart/mixed; boundary=\"OUT\"\r\n\r\n--OUT\r\nContent-Type: multipart/alternative; boundary=IN;\r\n\r\n--OUT\r\n";
        assert_eq!(
            multipart_boundaries(raw),
            vec!["OUT".to_string(), "IN".to_string()]
        );
    }

    #[test]
    fn finds_no_boundary_in_a_single_part_message() {
        assert!(multipart_boundaries(PLAIN_HEADER).is_empty());
    }

    #[test]
    fn decodes_a_single_part_fragment_cut_mid_body() {
        let text = b"Beste heer, =0A=0AHartelijk dank voor uw beste";
        let preview = preview_from_fragment(PLAIN_HEADER, text).unwrap();
        assert_eq!(preview, "Beste heer, Hartelijk dank voor uw beste");
    }

    #[test]
    fn decodes_a_multipart_fragment_cut_inside_the_plain_part() {
        // Without the closing delimiter this is "Hello=0Athere" -- readable
        // enough to look correct, which is what makes it worth a test.
        let header = b"From: a@b.c\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"B\"\r\n\r\n";
        let text = b"--B\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello=0Athere, cut off";
        assert_eq!(
            preview_from_fragment(header, text).unwrap(),
            "Hello there, cut off"
        );
    }

    #[test]
    fn decodes_a_nested_multipart_fragment() {
        // Closing only the outer boundary leaves the part undecoded *and*
        // leaks "--OUT--" into the preview text.
        let header = b"From: a@b.c\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"OUT\"\r\n\r\n";
        let text = b"--OUT\r\nContent-Type: multipart/alternative; boundary=\"IN\"\r\n\r\n--IN\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello=0Athere nested";
        let preview = preview_from_fragment(header, text).unwrap();
        assert_eq!(preview, "Hello there nested");
        assert!(!preview.contains("--OUT--"));
    }

    #[test]
    fn gives_no_preview_for_an_html_only_message() {
        let header = b"From: a@b.c\r\nContent-Type: text/html; charset=utf-8\r\n\r\n";
        assert!(preview_from_fragment(header, b"<div>&gt; quoted</div>").is_none());
    }
}
