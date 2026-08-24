use birdman_client::Client;
use birdman_store::MessageId;

use crate::format;

pub fn list(
    client: &Client,
    json: bool,
    message: MessageId,
    save: Option<&str>,
) -> Result<(), String> {
    let attachments = match save {
        Some(_) => client
            .materialise_attachments_blocking(message)
            .map_err(|e| e.to_string())?,
        None => client.attachments(message).map_err(|e| e.to_string())?,
    };

    if let Some(dir) = save {
        let dir = std::path::Path::new(dir);
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for attachment in &attachments {
            let Some(source) = attachment.path.as_deref() else {
                continue;
            };
            let target = dir.join(format::safe_basename(&attachment.filename));
            std::fs::copy(source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            if !json {
                println!("{}", target.display());
            }
        }
        if json {
            println!(
                "[{}]",
                attachments
                    .iter()
                    .map(format::attachment_json)
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        return Ok(());
    }

    if json {
        println!(
            "[{}]",
            attachments
                .iter()
                .map(format::attachment_json)
                .collect::<Vec<_>>()
                .join(",")
        );
        return Ok(());
    }
    if attachments.is_empty() {
        println!("(none)");
        return Ok(());
    }
    for attachment in &attachments {
        println!(
            "{:>9}  {:<28} {}",
            format::byte_size(attachment.size),
            format::truncate(&attachment.filename, 28),
            attachment
                .content_type
                .as_deref()
                .unwrap_or("application/octet-stream")
        );
    }
    Ok(())
}

pub fn contacts(client: &Client, json: bool, limit: u32) -> Result<(), String> {
    let contacts = client.contacts(limit).map_err(|e| e.to_string())?;
    if json {
        println!(
            "[{}]",
            contacts
                .iter()
                .map(format::contact_json)
                .collect::<Vec<_>>()
                .join(",")
        );
        return Ok(());
    }
    for contact in &contacts {
        match &contact.name {
            Some(name) => println!("{:>5}  {} <{}>", contact.seen, name, contact.address),
            None => println!("{:>5}  {}", contact.seen, contact.address),
        }
    }
    Ok(())
}
