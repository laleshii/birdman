use birdman_backend::OutgoingMessage;
use birdman_client::Client;
use birdman_store::{OutboxId, OutboxState};

fn state_label(state: OutboxState) -> &'static str {
    match state {
        OutboxState::Queued => "queued",
        OutboxState::Sending => "sending",
        OutboxState::Sent => "sent",
        OutboxState::Failed => "failed",
    }
}

fn describe(payload: &str) -> (String, String) {
    match serde_json::from_str::<OutgoingMessage>(payload) {
        Ok(message) => (
            message
                .to
                .iter()
                .chain(&message.cc)
                .map(|r| r.address.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            message.subject,
        ),
        Err(_) => ("?".to_string(), "(unreadable payload)".to_string()),
    }
}

pub fn list(client: &Client, json: bool) -> Result<(), String> {
    let entries = client.outbox().map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&entries).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if entries.is_empty() {
        println!("outbox is empty");
        return Ok(());
    }
    for entry in &entries {
        let (recipients, subject) = describe(&entry.payload);
        println!(
            "{:<6} {:<8} {} to {} -- {}",
            entry.id.0,
            state_label(entry.state),
            crate::format::when(Some(entry.created_at)),
            recipients,
            crate::format::truncate(&subject, 50)
        );
        if let Some(error) = &entry.last_error {
            println!("       attempt {}: {error}", entry.attempts);
        }
    }
    Ok(())
}

pub fn retry(client: &Client, id: OutboxId) -> Result<(), String> {
    if client.outbox_retry(id).map_err(|e| e.to_string())? {
        println!("retrying {}", id.0);
        Ok(())
    } else {
        Err(format!("{} is not waiting to be sent", id.0))
    }
}

pub fn cancel(client: &Client, id: OutboxId) -> Result<(), String> {
    if client.outbox_cancel(id).map_err(|e| e.to_string())? {
        println!("cancelled {}", id.0);
        Ok(())
    } else {
        Err(format!(
            "{} could not be cancelled -- not found, or mid-flight",
            id.0
        ))
    }
}
