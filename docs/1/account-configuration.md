---
id: account-configuration
title: 'Account configuration: multi-account config.toml and first run'
altitude: 1
topics:
- config
relations:
- type: refines
  target: birdman-overview
- type: part_of
  target: gpui-application
summary: The [accounts.<id>] schema where each account declares its receiver, sender and auth connector types; the legacy single-[account] fallback; and first-run template behaviour.
---

# Account configuration: multi-account config.toml and first run

`crates/birdman-ui/src/config.rs` reads `$XDG_CONFIG_HOME/birdman/config.toml`
(normally `~/.config/birdman/config.toml`) — the standard XDG location, matching
where the SQLite database already lives (`$XDG_DATA_HOME/birdman`, via
`dirs::data_dir()` in `main.rs`) rather than a loose dotfile in `$HOME`.

## The schema

One table per account. The **table key is the account's id**.

```toml
[accounts.personal]
display_name = "Personal Gmail"   # optional; defaults to the key
email = "you@gmail.com"
receiver = { type = "imap", host = "imap.gmail.com", port = 993 }
sender   = { type = "smtp", host = "smtp.gmail.com", port = 465 }
auth     = { type = "keyring", username = "you@gmail.com" }

[accounts.work]
email = "you@corp.com"
receiver = { type = "imap", host = "outlook.office365.com" }
sender   = { type = "smtp", host = "smtp.office365.com", port = 587, starttls = true }
auth     = { username = "you@corp.com" }
```

Defaults: imap port 993, smtp port 465, `sender.host` falls back to
`receiver.host`, `auth.type` falls back to `keyring`, `auth.username` falls back
to `email`, `display_name` falls back to the table key.

## Why the type is declared instead of implied

The old schema encoded the protocol in the *key names*: `imap_host`,
`smtp_host`, `smtp_starttls`. That works exactly until a second protocol exists,
at which point every new connector needs its own key prefix and the parser has
to guess which set is authoritative.

`receiver = { type = "imap", ... }` moves the protocol into a value. Adding JMAP
is a `ReceiverKind` variant, a `parse` arm and a constructor arm in `main.rs` —
see `crates/connectors/README.md`. No new keys. See [[connector-boundary]].

`auth.type` works the same way and selects a credential adapter — `keyring`,
`command` or `env`. Its arguments are validated on load, so a `command` with no
`command = [...]` array fails when the file is read rather than at the first
attempt to send mail. See [[auth-adapter-design]].

## Accounts are a BTreeMap, so order is alphabetical

`accounts: BTreeMap<String, TomlAccount>` — sidebar sections and the compose
From picker follow that order, not file order. Renaming a key reorders the
sidebar.

## One bad account does not sink the rest

`build_account` returns `Result`, and `load()` collects failures. An account
whose `receiver.type` is unrecognised is logged with `log::warn!` and skipped;
the others still load. Losing every account because one has a typo is the worse
failure. Only when *no* account is usable does `load()` return `Unconfigured`,
carrying the collected errors so the onboarding screen can show why.

## First run writes a template rather than erroring

If the file doesn't exist, `load()` writes a commented-out template with a
filled-in Gmail example plus a second-account example, and returns
`Config::Unconfigured`. `main.rs` shows the onboarding screen.

The template is a raw string that **must** use `r##"..."##` — it contains
`"#` in hex colours, which would close an `r#"..."#` literal.

## Only keyring accounts are checked before the window opens

`main()` looks for the first **keyring** account with nothing saved yet and
shows the password prompt for it. Accounts using `command` or `env` are skipped:
there is nothing to prompt for.

Nothing else is resolved up front. Adapters are consulted per connection, so no
password is read at startup and none is held afterwards. See
[[keyring-credentials]].
