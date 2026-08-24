use std::sync::Arc;

use birdman_auth::{AuthAdapter, AuthContext, Credentials};
use birdman_backend::{boxed_send, BackendError, MailSender, OutgoingMessage, SendFuture};

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub implicit_tls: bool,
    pub username: String,
    /// Never set for a real account: self-signed local/test servers only.
    pub danger_accept_invalid_certs: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("smtp error: {0}")]
    Smtp(#[from] smtp::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("message must have at least one recipient")]
    NoRecipients,
    #[error("invalid smtp config: {0}")]
    InvalidConfig(String),
}

pub struct SmtpSender {
    config: Arc<SmtpConfig>,
    auth: Arc<dyn AuthAdapter>,
    account_id: String,
}

impl SmtpSender {
    pub fn new(
        config: SmtpConfig,
        auth: Arc<dyn AuthAdapter>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            auth,
            account_id: account_id.into(),
        }
    }
}

impl MailSender for SmtpSender {
    fn name(&self) -> &'static str {
        "smtp"
    }

    fn send(&self, message: OutgoingMessage) -> SendFuture {
        let config = self.config.clone();
        let auth = self.auth.clone();
        let ctx = AuthContext {
            account_id: self.account_id.clone(),
            username: config.username.clone(),
        };
        boxed_send(async move {
            let credentials = auth
                .credentials(&ctx)
                .await
                .map_err(|err| BackendError::Failed(format!("credentials unavailable: {err}")))?;
            send(&config, &credentials, message)
                .await
                .map_err(|err| BackendError::Failed(err.to_string()))
        })
    }
}

pub async fn send(
    config: &SmtpConfig,
    credentials: &Credentials,
    message: OutgoingMessage,
) -> Result<(), SendError> {
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        return Err(SendError::NoRecipients);
    }

    // OAuth's HTTP stack enables rustls/ring while mail-send enables
    // rustls/aws-lc-rs. With both compiled, rustls deliberately refuses to
    // guess and otherwise panics on the first SMTP connection. Install the
    // provider mail-send selected; an already-installed provider is also
    // valid and makes this a no-op.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let raw = render(&message)?;
    let smtp_credentials = match credentials {
        Credentials::Password(password) => {
            smtp::Credentials::new(config.username.clone(), password.clone())
        }
        Credentials::OAuth2 {
            username,
            access_token,
        } => smtp::Credentials::new_xoauth2(username.clone(), access_token.clone()),
    };
    let mut builder_client = smtp::SmtpClientBuilder::new(config.host.clone(), config.port)
        .map_err(SendError::InvalidConfig)?
        .implicit_tls(config.implicit_tls)
        .credentials(smtp_credentials);
    if config.danger_accept_invalid_certs {
        builder_client = builder_client.allow_invalid_certs();
    }
    let mut client = builder_client.connect().await?;

    let rcpt_to: Vec<smtp::smtp::message::Address> = message
        .to
        .iter()
        .chain(&message.cc)
        .chain(&message.bcc)
        .map(|r| smtp::smtp::message::Address {
            email: r.address.clone().into(),
            parameters: Default::default(),
        })
        .collect();
    client
        .send(smtp::smtp::message::Message {
            mail_from: smtp::smtp::message::Address {
                email: message.from.address.clone().into(),
                parameters: Default::default(),
            },
            rcpt_to,
            body: raw.into(),
        })
        .await?;
    Ok(())
}

pub fn render(message: &OutgoingMessage) -> Result<Vec<u8>, SendError> {
    let mut builder = mail_builder::MessageBuilder::new()
        .from((message.from.name.clone(), message.from.address.clone()))
        .subject(message.subject.clone())
        .text_body(message.text_body.clone());

    if !message.to.is_empty() {
        builder = builder.to(message
            .to
            .iter()
            .map(|r| (r.name.clone(), r.address.clone()))
            .collect::<Vec<_>>());
    }
    if !message.cc.is_empty() {
        builder = builder.cc(message
            .cc
            .iter()
            .map(|r| (r.name.clone(), r.address.clone()))
            .collect::<Vec<_>>());
    }
    if let Some(message_id) = &message.message_id {
        builder = builder.message_id(message_id.clone());
    }
    if let Some(date) = message.date {
        builder = builder.date(date);
    }
    if let Some(in_reply_to) = &message.in_reply_to {
        builder = builder.in_reply_to(in_reply_to.clone());
    }
    if !message.references.is_empty() {
        builder = builder.references(message.references.clone());
    }
    Ok(builder.write_to_vec()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use birdman_backend::Recipient;

    #[test]
    fn rendered_sent_copy_matches_the_message_without_exposing_bcc() {
        let message = OutgoingMessage {
            from: Recipient::new(Some("Ada".into()), "ada@example.com".into()),
            to: vec![Recipient::new(None, "to@example.com".into())],
            cc: Vec::new(),
            bcc: vec![Recipient::new(None, "blind@example.com".into())],
            subject: "Quarterly report".into(),
            text_body: "Attached later".into(),
            in_reply_to: None,
            references: Vec::new(),
            message_id: Some("outbox-42@example.com".into()),
            date: Some(1_777_027_200),
        };

        let rendered = String::from_utf8(render(&message).unwrap()).unwrap();
        assert!(rendered.contains("Subject: Quarterly report"), "{rendered}");
        assert!(rendered.contains("to@example.com"), "{rendered}");
        assert!(!rendered.contains("blind@example.com"), "{rendered}");
        assert!(
            !rendered.to_ascii_lowercase().contains("\nbcc:"),
            "{rendered}"
        );
        assert_eq!(render(&message).unwrap(), rendered.as_bytes());
    }
}
