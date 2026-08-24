use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

pub mod logging;

/// Shared across binaries: the CLI writing a token the desktop cannot read
/// would be a silent, baffling failure.
pub const KEYRING_SERVICE: &str = "birdman";

/// What the keyring, the config directory and the data directory were called
/// before the project was renamed. Read once, to move an existing install
/// across; delete this and [`adopt_legacy_dir`] a release or two after that.
pub const LEGACY_NAME: &str = "osprey";

impl AuthConfig {
    pub fn adapter(&self) -> std::sync::Arc<dyn birdman_auth::AuthAdapter> {
        use std::sync::Arc;
        match &self.kind {
            AuthKind::Keyring => Arc::new(birdman_auth::KeyringAdapter::new(KEYRING_SERVICE)),
            AuthKind::Command { program, args } => Arc::new(birdman_auth::CommandAdapter::new(
                program.clone(),
                args.clone(),
            )),
            AuthKind::Env { var } => Arc::new(birdman_auth::EnvAdapter::new(var.clone())),
            AuthKind::OAuth2 {
                endpoints,
                client_id,
                client_secret,
            } => Arc::new(birdman_auth::OAuth2Adapter::new(
                endpoints.clone(),
                client_id.clone(),
                client_secret.clone(),
                self.username.clone(),
                KEYRING_SERVICE,
            )),
        }
    }

    /// Whether a first-run prompt has anywhere to write. `command`, `env` and
    /// `oauth2` get their credential elsewhere.
    pub fn is_prompted(&self) -> bool {
        matches!(self.kind, AuthKind::Keyring)
    }
}

pub fn data_dir() -> PathBuf {
    let base = dirs::data_dir().expect("no data directory available");
    adopt_legacy_dir(base.join(LEGACY_NAME), base.join("birdman"))
}

/// Moves an `birdman`-era directory to its `birdman` name, once.
///
/// Only when the new path does not exist and the old one does, so it cannot
/// overwrite anything and is a no-op on every run after the first. A failure
/// is not fatal: the caller carries on against the new path, which then starts
/// empty -- a re-sync, not a crash.
pub fn adopt_legacy_dir(old: PathBuf, new: PathBuf) -> PathBuf {
    if !new.exists() && old.is_dir() {
        if let Some(parent) = new.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::rename(&old, &new) {
            Ok(()) => log::info!("moved {} to {}", old.display(), new.display()),
            Err(err) => log::warn!(
                "could not move {} to {}: {err}",
                old.display(),
                new.display()
            ),
        }
    }
    new
}

/// Only the account half. serde ignores the rest, so two readers of one file
/// need not agree on its whole shape.
#[derive(Deserialize)]
struct TomlFile {
    #[serde(default)]
    accounts: std::collections::BTreeMap<String, TomlAccount>,
    #[serde(default)]
    daemon: TomlDaemon,
}

#[derive(Deserialize, Default)]
struct TomlDaemon {
    auto_stop: Option<bool>,
    idle_timeout: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonConfig {
    /// On by default. Turn it off where a service manager owns the process.
    pub auto_stop: bool,
    pub idle_timeout: std::time::Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            auto_stop: true,
            idle_timeout: std::time::Duration::from_secs(60),
        }
    }
}

/// Partial sections fall back field by field, so a config setting only
/// `idle_timeout` still gets `auto_stop = true`.
pub fn load_daemon() -> DaemonConfig {
    let defaults = DaemonConfig::default();
    let Ok(raw) = fs::read_to_string(config_path()) else {
        return defaults;
    };
    let Ok(parsed) = toml::from_str::<TomlFile>(&raw) else {
        return defaults;
    };
    DaemonConfig {
        auto_stop: parsed.daemon.auto_stop.unwrap_or(defaults.auto_stop),
        idle_timeout: parsed
            .daemon
            .idle_timeout
            .map(std::time::Duration::from_secs)
            .unwrap_or(defaults.idle_timeout),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReceiverKind {
    Imap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SenderKind {
    Smtp,
}

/// Whether a copy of a sent message is filed in the account's Sent folder.
///
/// `Auto` defers to the server: some providers (Gmail is the known one)
/// archive submissions to Sent themselves, and filing again leaves two
/// copies of every send. Servers with no such habit get an explicit copy
/// via IMAP `APPEND` after delivery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveToSent {
    Auto,
    Yes,
    No,
}

impl SaveToSent {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "yes" | "always" | "on" | "true" => Some(Self::Yes),
            "no" | "never" | "off" | "false" => Some(Self::No),
            _ => None,
        }
    }
}

impl ReceiverKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "imap" => Some(Self::Imap),
            _ => None,
        }
    }
}

impl SenderKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "smtp" => Some(Self::Smtp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReceiverConfig {
    pub kind: ReceiverKind,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct SenderConfig {
    pub kind: SenderKind,
    pub host: String,
    pub port: u16,
    pub implicit_tls: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AuthKind {
    Keyring,
    Command {
        program: String,
        args: Vec<String>,
    },
    Env {
        var: String,
    },
    /// Requires a one-time `birdman authorize <account>` to obtain consent.
    OAuth2 {
        endpoints: birdman_auth::OAuth2Endpoints,
        client_id: String,
        /// Not actually secret -- it ships in a readable binary, and PKCE is
        /// what protects the exchange. Optional: some clients are issued none.
        client_secret: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub kind: AuthKind,
    pub username: String,
}

#[derive(Clone, Debug)]
pub struct ConfiguredAccount {
    /// The table key (`[accounts.work]` -> `work`). Stable across restarts, so
    /// renaming or changing servers does not orphan stored credentials.
    pub id: String,
    pub display_name: String,
    /// The name mail is *signed* with. Distinct from `display_name`, which
    /// labels the mailbox: falling back to it sent `From: Gmail <...>`.
    pub name: Option<String>,
    pub email: String,
    pub receiver: ReceiverConfig,
    pub sender: SenderConfig,
    pub auth: AuthConfig,
    pub danger_accept_invalid_certs: bool,
    pub save_to_sent: SaveToSent,
}

pub enum Config {
    Accounts(Vec<ConfiguredAccount>),
    /// `error` is set when the file exists but could not be parsed, so
    /// onboarding can show why rather than just "not configured".
    Unconfigured {
        path: PathBuf,
        error: Option<String>,
    },
}

#[derive(Deserialize)]
struct TomlAccount {
    display_name: Option<String>,
    name: Option<String>,
    email: Option<String>,
    receiver: TomlReceiver,
    sender: Option<TomlSender>,
    auth: Option<TomlAuth>,
    insecure_tls: Option<bool>,
    save_to_sent: Option<String>,
}

#[derive(Deserialize)]
struct TomlReceiver {
    #[serde(rename = "type")]
    kind: String,
    host: String,
    port: Option<u16>,
}

#[derive(Deserialize)]
struct TomlSender {
    #[serde(rename = "type")]
    kind: String,
    host: Option<String>,
    port: Option<u16>,
    starttls: Option<bool>,
}

#[derive(Deserialize, Default)]
struct TomlAuth {
    #[serde(rename = "type")]
    kind: Option<String>,
    username: Option<String>,
    command: Option<Vec<String>>,
    var: Option<String>,
    provider: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    auth_url: Option<String>,
    token_url: Option<String>,
    scope: Option<String>,
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().expect("no XDG config directory available");
    adopt_legacy_dir(base.join(LEGACY_NAME), base.join("birdman")).join("config.toml")
}

pub fn load() -> Config {
    let path = config_path();

    if !path.exists() {
        return match write_template(&path) {
            Ok(()) => Config::Unconfigured { path, error: None },
            Err(err) => Config::Unconfigured {
                path,
                error: Some(format!("couldn't create config file: {err}")),
            },
        };
    }

    // This file can hold an OAuth client secret, and one created by hand keeps
    // whatever the editor left. Tightened on every load, not only at creation.
    let _ = restrict_to_owner(&path);

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            return Config::Unconfigured {
                path,
                error: Some(format!("couldn't read config file: {err}")),
            };
        }
    };
    let parsed: TomlFile = match toml::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            return Config::Unconfigured {
                path,
                error: Some(format!("config file has a syntax error: {err}")),
            };
        }
    };
    let mut accounts = Vec::new();
    let mut errors = Vec::new();

    for (id, account) in parsed.accounts {
        match build_account(&id, account) {
            Ok(account) => accounts.push(account),
            Err(err) => errors.push(format!("[accounts.{id}]: {err}")),
        }
    }
    if accounts.is_empty() {
        return Config::Unconfigured {
            path,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
        };
    }
    // Logged rather than fatal: losing every account to one typo is worse.
    for err in &errors {
        log::warn!("ignoring misconfigured account -- {err}");
    }
    Config::Accounts(accounts)
}

fn build_account(id: &str, account: TomlAccount) -> Result<ConfiguredAccount, String> {
    let kind = ReceiverKind::parse(&account.receiver.kind)
        .ok_or_else(|| format!("unknown receiver type {:?}", account.receiver.kind))?;

    let auth = account.auth.unwrap_or_default();
    let auth_kind = match auth
        .kind
        .as_deref()
        .map(|k| k.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("keyring") => AuthKind::Keyring,
        Some("command") => {
            // On load, not at use: a config error should not first surface on
            // an attempt to send mail hours later.
            let mut parts = auth
                .command
                .clone()
                .ok_or_else(|| "auth.type = \"command\" needs a command = [...] array".to_string())?
                .into_iter();
            let program = parts
                .next()
                .ok_or_else(|| "auth.command is empty".to_string())?;
            AuthKind::Command {
                program,
                args: parts.collect(),
            }
        }
        Some("env") => AuthKind::Env {
            var: auth
                .var
                .clone()
                .ok_or_else(|| "auth.type = \"env\" needs a var = \"...\"".to_string())?,
        },
        Some("oauth2") => {
            let endpoints = match auth.provider.as_deref() {
                Some(provider) => birdman_auth::OAuth2Endpoints::parse_provider(provider)
                    .ok_or_else(|| format!("unknown oauth2 provider {provider:?}"))?,
                None => birdman_auth::OAuth2Endpoints {
                    auth_url: auth.auth_url.clone().ok_or_else(|| {
                        "oauth2 needs a provider, or an explicit auth_url".to_string()
                    })?,
                    token_url: auth.token_url.clone().ok_or_else(|| {
                        "oauth2 needs a provider, or an explicit token_url".to_string()
                    })?,
                    scope: auth.scope.clone().ok_or_else(|| {
                        "oauth2 needs a provider, or an explicit scope".to_string()
                    })?,
                },
            };
            AuthKind::OAuth2 {
                endpoints,
                client_id: auth
                    .client_id
                    .clone()
                    .ok_or_else(|| "oauth2 needs a client_id".to_string())?,
                client_secret: auth.client_secret.clone(),
            }
        }
        Some(raw) => return Err(format!("unknown auth type {raw:?}")),
    };

    let email = account
        .email
        .clone()
        .or_else(|| auth.username.clone())
        .ok_or_else(|| "needs an email (or auth.username)".to_string())?;
    let username = auth.username.unwrap_or_else(|| email.clone());

    let sender = account.sender.unwrap_or(TomlSender {
        kind: "smtp".to_string(),
        host: None,
        port: None,
        starttls: None,
    });
    let sender_kind = SenderKind::parse(&sender.kind)
        .ok_or_else(|| format!("unknown sender type {:?}", sender.kind))?;

    let save_to_sent = match account.save_to_sent.as_deref() {
        Some(raw) => SaveToSent::parse(raw).ok_or_else(|| {
            format!("save_to_sent should be \"auto\", \"yes\" or \"no\", not {raw:?}")
        })?,
        None => SaveToSent::Auto,
    };

    Ok(ConfiguredAccount {
        id: id.to_string(),
        display_name: account.display_name.unwrap_or_else(|| id.to_string()),
        name: account.name,
        receiver: ReceiverConfig {
            kind,
            host: account.receiver.host.clone(),
            port: account.receiver.port.unwrap_or(993),
        },
        sender: SenderConfig {
            kind: sender_kind,
            host: sender.host.unwrap_or_else(|| account.receiver.host.clone()),
            port: sender.port.unwrap_or(465),
            implicit_tls: !sender.starttls.unwrap_or(false),
        },
        auth: AuthConfig {
            kind: auth_kind,
            username,
        },
        danger_accept_invalid_certs: account.insecure_tls.unwrap_or(false),
        save_to_sent,
        email,
    })
}

/// `0700` for a directory, `0600` for a file. Without it these are created at
/// whatever the umask is -- `0644`/`0755` by default, which left every message
/// body world-readable under `~/.local/share`.
///
/// Applied on every start, not only at creation, so an older store is repaired.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mut perms = metadata.permissions();
    if perms.mode() & 0o777 == mode {
        return Ok(());
    }
    perms.set_mode(mode);
    fs::set_permissions(path, perms)
}

/// Checks the *directory*, not the socket: a `0600` socket inside a
/// world-writable directory can still be unlinked and replaced.
pub fn is_reachable_by_others(dir: &Path) -> io::Result<bool> {
    Ok(fs::metadata(dir)?.permissions().mode() & 0o077 != 0)
}

fn write_template(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, TEMPLATE)?;
    restrict_to_owner(path)
}

/// `xdg-open` does not reliably launch a terminal for editors whose desktop
/// entry has `Terminal=true` (`nvim.desktop`), so Omarchy's own
/// `omarchy-launch-editor` is preferred where present. Never waits.
pub fn open_editor(path: &Path) -> io::Result<()> {
    match Command::new("omarchy-launch-editor").arg(path).spawn() {
        Ok(_child) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => open::that_detached(path),
        Err(err) => Err(err),
    }
}

const TEMPLATE: &str = r##"# Birdman account configuration.
#
# Uncomment an [accounts.*] block below, fill in your details, and restart
# Birdman. The first time you do, you'll be asked for your password once --
# it's saved in your system keyring from then on, never in this file.
#
# The table key names the account ("personal" here) and is what shows in the
# sidebar unless you set display_name. Add as many blocks as you like.
#
# `name` is what your mail is signed with -- the display name a recipient sees
# in their From column. `display_name` only labels the account in the sidebar.
#
# `receiver` and `sender` each declare a connector `type`. Today that's "imap"
# and "smtp"; the type is declared rather than implied by a key name so adding
# a protocol doesn't mean inventing new keys.
#
# Example for Gmail -- you'll need an App Password, not your normal login
# password: generate one at https://myaccount.google.com/apppasswords
# (requires 2-Step Verification to already be turned on).
#
# [accounts.personal]
# display_name = "Personal Gmail"
# name = "Ada Lovelace"
# email = "you@gmail.com"
# receiver = { type = "imap", host = "imap.gmail.com", port = 993 }
# sender   = { type = "smtp", host = "smtp.gmail.com", port = 465 }
# auth     = { type = "keyring", username = "you@gmail.com" }
#
# A second account, on a server that wants STARTTLS on 587:
#
# [accounts.work]
# display_name = "Work"
# email = "you@corp.com"
# receiver = { type = "imap", host = "outlook.office365.com", port = 993 }
# sender   = { type = "smtp", host = "smtp.office365.com", port = 587, starttls = true }
# auth     = { type = "keyring", username = "you@corp.com" }
#
# Defaults: port 993 for imap, 465 for smtp, sender host falls back to the
# receiver host, auth type falls back to "keyring", and auth.username falls
# back to email. `insecure_tls = true` skips certificate validation -- only
# ever for a local test server.
#
# After a send succeeds, a copy is filed in the account's Sent folder:
#   save_to_sent = "auto"   (default) file a copy unless the server does it
#   save_to_sent = "yes"    always file -- use if you keep the Sent copy
#                           somewhere auto would not look
#   save_to_sent = "no"     never file
# "auto" knows that Gmail archives submissions itself, and skips filing
# there to avoid two copies of every send.
#
# auth.type picks where the password comes from. It is never in this file:
#
#   auth = { type = "keyring", username = "you@gmail.com" }
#   auth = { type = "command", command = ["pass", "show", "mail/gmail"] }
#   auth = { type = "env", var = "BIRDMAN_GMAIL_PASSWORD" }
#
# "command" reads the secret from the program's stdout, so pass/gopass/op/bw
# all work. Only "keyring" accounts are asked for a password on first run.

# The daemon (birdmand) owns the mailbox: one process holds the connections and
# writes to the store, and every client talks to it. It starts on demand and
# stops again once nothing is connected.
#
# [daemon]
# auto_stop = true      # stop once no client has been connected for a while
# idle_timeout = 60     # seconds to wait before stopping
#
# Set auto_stop = false to keep mail syncing with no client open, or when a
# service manager owns the process.
#
# For Gmail without an App Password, register a Desktop-app OAuth client in the
# Google Cloud console and use:
#
#   auth = { type = "oauth2", provider = "google", username = "you@gmail.com",
#            client_id = "...", client_secret = "..." }
#
# then run `birdman authorize <account>` once. See the README -- in particular,
# publish the consent screen, or Google expires the refresh token every 7 days.

# [appearance]
# How mail that shows no dark-mode support of its own is rendered:
#   "auto"   recolour it to match the app, unless it has its own
#            prefers-color-scheme rules  (default)
#   "always" recolour everything, even senders that do handle dark
#   "never"  leave mail exactly as sent, on white
# email_dark_mode = "auto"
# load_remote_images = "always"  # always (default) | never
#
# Reading-pane buttons, in order. "spacer" pushes the rest to the right.
# Drop a name to hide that button.
#   reply, reply_all, forward, flag, archive, delete, spacer
# toolbar_actions = ["reply", "reply_all", "forward", "spacer", "flag", "spacer", "archive", "delete"]
#
# Read the palette from another file instead of the [theme] table below.
# Relative paths resolve next to this file. The file is re-read whenever it
# changes -- including when a symlink is repointed at a different theme, which
# is how Omarchy switches system-wide -- and the app recolours without a
# restart.
# theme_file = "~/.config/omarchy/current/theme/birdman.toml"

# [theme]
# Any subset; anything left out keeps its default. `#rrggbb`, `rrggbb` or
# `0xrrggbb`.
# bg_app = "#282c34"
# bg_sidebar = "#21252b"
# bg_list = "#24282f"
# bg_selected = "#3a4657"
# bg_hover = "#323844"
# bg_unread = "#2d323c"
# border = "#3e4451"
# text_primary = "#ffffff"
# text_secondary = "#c5c8c6"
# text_muted = "#8c919c"
# accent = "#7aa6da"
# danger = "#cc6666"
# scrollbar_thumb = "#767d8a"
# scrollbar_thumb_hover = "#a6aeba"
"##;

#[cfg(test)]
mod tests {
    #[test]
    fn a_signing_name_is_separate_from_the_sidebar_label() {
        let accounts = accounts_from(
            r#"
            [accounts.gmail]
            display_name = "Gmail"
            name = "Ada Lovelace"
            email = "ada@example.com"
            receiver = { type = "imap", host = "imap.example.com" }
            "#,
        );
        assert_eq!(accounts[0].display_name, "Gmail");
        assert_eq!(accounts[0].name.as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn no_signing_name_means_none_rather_than_the_label() {
        let accounts = accounts_from(
            r#"
            [accounts.gmail]
            display_name = "Gmail"
            email = "ada@example.com"
            receiver = { type = "imap", host = "imap.example.com" }
            "#,
        );
        assert_eq!(accounts[0].name, None);
    }

    #[test]
    fn restricting_a_file_leaves_it_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.toml");
        fs::write(&file, "x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        restrict_to_owner(&file).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn restricting_a_directory_uses_the_executable_bit() {
        // 0600 on a directory would make it unlistable by its owner too.
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("data");
        fs::create_dir(&inner).unwrap();
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();

        restrict_to_owner(&inner).unwrap();
        assert_eq!(
            fs::metadata(&inner).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn reachability_is_about_group_and_other_only() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("data");
        fs::create_dir(&inner).unwrap();

        fs::set_permissions(&inner, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!is_reachable_by_others(&inner).unwrap());

        // Execute-only is enough to traverse in and reach a socket by name.
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o701)).unwrap();
        assert!(is_reachable_by_others(&inner).unwrap());

        fs::set_permissions(&inner, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(is_reachable_by_others(&inner).unwrap());
    }

    use super::*;

    fn accounts_from(raw: &str) -> Vec<ConfiguredAccount> {
        let parsed: TomlFile = toml::from_str(raw).expect("fixture should parse");
        parsed
            .accounts
            .into_iter()
            .map(|(id, a)| build_account(&id, a).expect("fixture should build"))
            .collect()
    }

    #[test]
    fn several_accounts_each_declare_their_own_connectors() {
        let accounts = accounts_from(
            r#"
            [accounts.personal]
            email = "me@gmail.com"
            receiver = { type = "imap", host = "imap.gmail.com", port = 993 }
            sender = { type = "smtp", host = "smtp.gmail.com", port = 465 }
            auth = { type = "keyring", username = "me@gmail.com" }

            [accounts.work]
            display_name = "Work"
            email = "me@corp.com"
            receiver = { type = "imap", host = "outlook.office365.com" }
            sender = { type = "smtp", host = "smtp.office365.com", port = 587, starttls = true }
            auth = { username = "me@corp.com" }
            "#,
        );
        assert_eq!(accounts.len(), 2);

        assert_eq!(accounts[0].id, "personal");
        assert_eq!(accounts[0].display_name, "personal");
        assert_eq!(accounts[0].receiver.kind, ReceiverKind::Imap);
        assert!(accounts[0].sender.implicit_tls);

        assert_eq!(accounts[1].display_name, "Work");
        assert_eq!(accounts[1].receiver.port, 993, "port should default");
        assert!(
            !accounts[1].sender.implicit_tls,
            "starttls = true means no implicit TLS"
        );
        assert_eq!(accounts[1].auth.kind, AuthKind::Keyring);
    }

    #[test]
    fn auth_types_select_their_adapter_and_are_validated_on_load() {
        let accounts = accounts_from(
            r#"
            [accounts.a]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }
            auth = { type = "command", command = ["pass", "show", "mail"] }

            [accounts.b]
            email = "d@e.f"
            receiver = { type = "imap", host = "h" }
            auth = { type = "env", var = "SECRET" }
            "#,
        );
        assert_eq!(
            accounts[0].auth.kind,
            AuthKind::Command {
                program: "pass".into(),
                args: vec!["show".into(), "mail".into()]
            }
        );
        assert_eq!(
            accounts[1].auth.kind,
            AuthKind::Env {
                var: "SECRET".into()
            }
        );
    }

    #[test]
    fn oauth2_resolves_a_provider_to_its_endpoints() {
        let accounts = accounts_from(
            r#"
            [accounts.gmail]
            email = "me@gmail.com"
            receiver = { type = "imap", host = "imap.gmail.com" }
            auth = { type = "oauth2", provider = "google", client_id = "cid", client_secret = "sec" }
            "#,
        );
        match &accounts[0].auth.kind {
            AuthKind::OAuth2 {
                endpoints,
                client_id,
                client_secret,
            } => {
                assert_eq!(client_id, "cid");
                assert_eq!(client_secret.as_deref(), Some("sec"));
                assert_eq!(endpoints.token_url, "https://oauth2.googleapis.com/token");
                assert_eq!(endpoints.scope, "https://mail.google.com/");
            }
            other => panic!("expected oauth2, got {other:?}"),
        }
    }

    #[test]
    fn a_personal_and_a_workspace_account_can_authenticate_differently() {
        let accounts = accounts_from(
            r#"
            [accounts.personal]
            email = "me@gmail.com"
            receiver = { type = "imap", host = "imap.gmail.com" }
            sender = { type = "smtp", host = "smtp.gmail.com" }
            auth = { type = "keyring", username = "me@gmail.com" }

            [accounts.work]
            display_name = "Montis"
            email = "me@example.nl"
            receiver = { type = "imap", host = "imap.gmail.com" }
            sender = { type = "smtp", host = "smtp.gmail.com" }
            auth = { type = "oauth2", provider = "google", username = "me@example.nl", client_id = "cid" }
            "#,
        );
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].auth.kind, AuthKind::Keyring);
        assert!(matches!(accounts[1].auth.kind, AuthKind::OAuth2 { .. }));
        assert_eq!(accounts[1].auth.username, "me@example.nl");
        assert_eq!(accounts[1].receiver.host, "imap.gmail.com");
    }

    #[test]
    fn oauth2_without_a_provider_requires_every_endpoint_spelled_out() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }
            auth = { type = "oauth2", client_id = "cid" }
            "#,
        )
        .unwrap();
        let (id, account) = parsed.accounts.into_iter().next().unwrap();
        let err = build_account(&id, account).unwrap_err();
        assert!(err.contains("auth_url"), "{err}");
    }

    #[test]
    fn oauth2_without_a_client_id_is_rejected_on_load() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }
            auth = { type = "oauth2", provider = "google" }
            "#,
        )
        .unwrap();
        let (id, account) = parsed.accounts.into_iter().next().unwrap();
        assert!(build_account(&id, account)
            .unwrap_err()
            .contains("client_id"));
    }

    #[test]
    fn a_command_auth_without_a_command_fails_on_load_not_on_first_send() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }
            auth = { type = "command" }
            "#,
        )
        .unwrap();
        let (id, account) = parsed.accounts.into_iter().next().unwrap();
        let err = build_account(&id, account).unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn an_unknown_connector_type_is_rejected_by_name() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "pop3", host = "h" }
            "#,
        )
        .unwrap();
        let (id, account) = parsed.accounts.into_iter().next().unwrap();
        let err = build_account(&id, account).unwrap_err();
        assert!(
            err.contains("pop3"),
            "the error should name the type it could not resolve: {err}"
        );
    }

    #[test]
    fn a_sender_omitted_entirely_falls_back_to_smtp_on_the_receiver_host() {
        let accounts = accounts_from(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "imap", host = "mail.example.com" }
            "#,
        );
        assert_eq!(accounts[0].sender.kind, SenderKind::Smtp);
        assert_eq!(accounts[0].sender.host, "mail.example.com");
    }

    #[test]
    fn save_to_sent_defaults_to_auto_and_parses() {
        let accounts = accounts_from(
            r#"
            [accounts.a]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }

            [accounts.b]
            email = "b@b.c"
            receiver = { type = "imap", host = "h" }
            save_to_sent = "no"
            "#,
        );
        assert_eq!(accounts[0].save_to_sent, SaveToSent::Auto);
        assert_eq!(accounts[1].save_to_sent, SaveToSent::No);
    }

    #[test]
    fn an_unknown_save_to_sent_value_is_rejected_on_load() {
        let parsed: TomlFile = toml::from_str(
            r#"
            [accounts.x]
            email = "a@b.c"
            receiver = { type = "imap", host = "h" }
            save_to_sent = "sometimes"
            "#,
        )
        .unwrap();
        let (id, account) = parsed.accounts.into_iter().next().unwrap();
        let err = build_account(&id, account).unwrap_err();
        assert!(err.contains("sometimes"), "{err}");
    }
}
