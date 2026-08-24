#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Recipient {
    pub name: String,
    pub address: String,
}

impl Recipient {
    pub fn new(name: Option<String>, address: String) -> Self {
        Self {
            name: name.unwrap_or_else(|| address.clone()),
            address,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutgoingMessage {
    pub from: Recipient,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    /// Goes in the SMTP envelope only. Rendering it as a header defeats it.
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub text_body: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub date: Option<i64>,
}
