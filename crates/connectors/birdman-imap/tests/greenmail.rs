//! Requires a running GreenMail container:
//!
//! ```sh
//! docker run -d --name birdman-test-greenmail \
//!   -p 3993:3993 -p 3143:3143 -p 3025:3025 \
//!   -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.users=testuser:testpass@localhost -Dgreenmail.auth.disabled=false' \
//!   greenmail/standalone:2.1.12
//! ```
//!
//! Then send it two messages and run `cargo test -p birdman-imap -- --ignored`.

use std::sync::{Arc, Mutex};

use birdman_imap::{connect_and_login, fetch_message_body, sync_folder, sync_folder_list};
use birdman_store::{AccountId, NewAccount, Security, Store};
use futures_util::StreamExt;

fn test_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory(dir.path()).unwrap();
    (store, dir)
}

fn insert_test_account(store: &Store) -> AccountId {
    store
        .insert_account(&NewAccount {
            display_name: "GreenMail",
            email: "testuser@localhost",
            imap_host: "127.0.0.1",
            imap_port: 3993,
            imap_security: Security::Tls,
            smtp_host: "127.0.0.1",
            smtp_port: 3025,
            smtp_security: Security::None,
            username: "testuser",
            keyring_ref: "test:greenmail",
        })
        .unwrap()
}

#[tokio::test]
#[ignore = "requires a running GreenMail container, see module docs"]
async fn full_sync_against_a_real_imap_server() {
    let (store, _dir) = test_store();
    let store = Arc::new(Mutex::new(store));
    let account_id = insert_test_account(&store.lock().unwrap());

    let mut session = connect_and_login("127.0.0.1", 3993, "testuser", "testpass", true)
        .await
        .expect("connect+login to GreenMail should succeed");

    let folders = sync_folder_list(&mut session, &store, account_id)
        .await
        .expect("folder listing should succeed");
    let inbox = folders
        .iter()
        .find(|f| f.imap_path.eq_ignore_ascii_case("INBOX"))
        .expect("GreenMail's test user should have an INBOX");

    let result = sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .expect("envelope sync should succeed");
    assert_eq!(
        result.new_uids.len(),
        2,
        "expected the 2 messages seeded before this test ran"
    );

    let page = {
        let store = store.lock().unwrap();
        store
            .list_messages_page(
                &[inbox.id],
                None,
                10,
                birdman_store::MessageFilter::default(),
            )
            .unwrap()
    };
    assert_eq!(page.len(), 2);
    assert_eq!(
        page[0].subject.as_deref(),
        Some("Integration test message 2 with attachment")
    );
    assert_eq!(
        page[1].subject.as_deref(),
        Some("Integration test message 1")
    );
    assert!(!page[0].body_fetched, "envelope sync must not fetch bodies");
    assert!(
        !page[0].flags.seen,
        "PEEK-based sync must not mark messages \\Seen"
    );

    let msg2 = &page[0];
    fetch_message_body(&mut session, &store, msg2.id, msg2.uid)
        .await
        .expect("body fetch should succeed");
    let (text, _html) = {
        let store = store.lock().unwrap();
        store
            .get_message_body(msg2.id)
            .unwrap()
            .expect("body should now be cached")
    };
    assert_eq!(text.as_deref(), Some("Second message body.\r\n"));

    // A *second* body fetch on the same session, which is what regressed:
    // abandoning the first fetch's stream before its tagged completion left
    // that reply in the connection buffer, so this call read it instead and
    // came back with message 2's body under message 1's uid. One fetch alone
    // never showed it.
    let msg1 = &page[1];
    fetch_message_body(&mut session, &store, msg1.id, msg1.uid)
        .await
        .expect("a second body fetch on the same session should succeed");
    let (text, _html) = {
        let store = store.lock().unwrap();
        store
            .get_message_body(msg1.id)
            .unwrap()
            .expect("body should now be cached")
    };
    assert_eq!(text.as_deref(), Some("First message body.\r\n"));

    let msg1_uid = page[1].uid;
    session
        .uid_store(msg1_uid.to_string(), "+FLAGS (\\Seen)")
        .await
        .expect("STORE should succeed")
        .collect::<Vec<_>>()
        .await;

    sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .expect("re-sync should succeed");

    let page_after = {
        let store = store.lock().unwrap();
        store
            .list_messages_page(
                &[inbox.id],
                None,
                10,
                birdman_store::MessageFilter::default(),
            )
            .unwrap()
    };
    let msg1_after = page_after.iter().find(|m| m.uid == msg1_uid).unwrap();
    assert!(
        msg1_after.flags.seen,
        "flag reconciliation should have picked up the server-side \\Seen flag"
    );

    let result_again = sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .unwrap();
    assert!(result_again.new_uids.is_empty());

    session.logout().await.ok();
}

#[tokio::test]
#[ignore = "requires a running GreenMail container, see module docs"]
async fn idle_wakes_on_new_mail() {
    let (store, _dir) = test_store();
    let store = Arc::new(Mutex::new(store));
    let account_id = insert_test_account(&store.lock().unwrap());

    let mut session = connect_and_login("127.0.0.1", 3993, "testuser", "testpass", true)
        .await
        .expect("connect+login should succeed");

    let folders = sync_folder_list(&mut session, &store, account_id)
        .await
        .unwrap();
    let inbox = folders
        .iter()
        .find(|f| f.imap_path.eq_ignore_ascii_case("INBOX"))
        .unwrap()
        .clone();
    sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .unwrap();

    assert!(
        birdman_imap::server_supports_idle(&mut session)
            .await
            .unwrap(),
        "GreenMail should advertise the IDLE extension"
    );
    session.select(&inbox.imap_path).await.unwrap();

    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        tokio::task::spawn_blocking(send_idle_test_message)
            .await
            .unwrap();
    });

    let (outcome, mut session) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        birdman_imap::idle_once(session),
    )
    .await
    .expect("IDLE should wake within 15s of new mail arriving")
    .expect("idle_once should succeed");

    assert!(
        matches!(outcome, birdman_imap::IdleOutcome::Activity),
        "expected server-pushed activity, not a refresh timeout"
    );

    let result = sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .unwrap();
    assert_eq!(
        result.new_uids.len(),
        1,
        "the message sent during IDLE should show up as exactly one new UID"
    );

    session.logout().await.ok();
}

/// Shells out to `python3` rather than adding an SMTP client to `birdman-imap`'s
/// dependencies for one test.
fn send_idle_test_message() {
    let script = r#"
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"] = "sender@example.com"
msg["To"] = "testuser@localhost"
msg["Subject"] = "Message sent during IDLE"
msg.set_content("This should wake the IDLE handle.")

with smtplib.SMTP("127.0.0.1", 3025, timeout=10) as s:
    s.login("testuser", "testpass")
    s.send_message(msg)
"#;
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .status()
        .expect("failed to run python3");
    assert!(status.success(), "sending the IDLE test message failed");
}
