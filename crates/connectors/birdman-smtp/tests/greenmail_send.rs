//! Requires the same GreenMail container as `birdman-imap`'s integration tests
//! (see that crate's `tests/greenmail.rs`). Run with
//! `cargo test -p birdman-smtp -- --ignored`.

use std::sync::{Arc, Mutex};

use birdman_auth::Credentials;
use birdman_backend::{OutgoingMessage, Recipient};
use birdman_imap::{connect_and_login, sync_folder, sync_folder_list};
use birdman_smtp::{send, SmtpConfig};
use birdman_store::{AccountId, NewAccount, Security, Store};

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
            keyring_ref: "test:greenmail-send",
        })
        .unwrap()
}

#[tokio::test]
#[ignore = "requires a running GreenMail container, see birdman-imap/tests/greenmail.rs"]
async fn sent_message_is_actually_delivered() {
    let unique_subject = format!("mail-send integration test {}", std::process::id());

    let config = SmtpConfig {
        host: "127.0.0.1".to_string(),
        port: 3465,
        implicit_tls: true,
        username: "testuser".to_string(),
        danger_accept_invalid_certs: true,
    };
    let message = OutgoingMessage {
        from: Recipient::new(
            Some("Test Sender".to_string()),
            "testuser@localhost".to_string(),
        ),
        to: vec![Recipient::new(None, "testuser@localhost".to_string())],
        cc: vec![],
        bcc: vec![],
        subject: unique_subject.clone(),
        text_body: "Sent by mail-send's integration test.".to_string(),
        in_reply_to: None,
        references: vec![],
        message_id: None,
        date: None,
    };

    send(
        &config,
        &Credentials::Password("testpass".to_string()),
        message,
    )
    .await
    .expect("send should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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
        .unwrap();
    sync_folder(&mut session, &store, account_id, inbox.id, &inbox.imap_path)
        .await
        .unwrap();

    let page = {
        let store = store.lock().unwrap();
        store
            .list_messages_page(
                &[inbox.id],
                None,
                50,
                birdman_store::MessageFilter::default(),
            )
            .unwrap()
    };
    assert!(
        page.iter()
            .any(|m| m.subject.as_deref() == Some(unique_subject.as_str())),
        "the message we just sent should show up in the INBOX we just synced"
    );

    session.logout().await.ok();
}
