# Birdman

**A scriptable mail daemon for Linux and macOS, with a command-line client.**
Written in Rust. A desktop app is included, but it is one client among several
rather than the point.

`birdmand` owns the mailbox: the SQLite cache, the connectors, the IMAP sync
engine, and your credentials. Clients speak to it over a Unix socket using a
small, documented protocol — newline-delimited JSON you can drive with `socat`
if you like. `birdman`, the CLI, is the reference client, and everything the
mailbox can do is reachable from it and from a pipe.

Early and actively developed: expect rough edges.

```sh
birdman ls --unread                       # what's new
birdman search "invoice" --json | jq .    # scriptable by construction
birdman read 4821                         # fetches the body on demand
birdman attachments 4821 --save ~/Desktop
birdman reply 4821 --all --body "On it."
birdman watch                             # a change stream to hang scripts off
```

## Why a daemon

One process owns the mailbox, so there is exactly one IMAP connection per
account, one IDLE loop, one writer, and one place a credential lives. Clients
hold none of that. That is what makes a second client cheap to write, and it is
why the CLI is not a reimplementation of the app — both send the same commands
to the same daemon and read the same store.

It starts on demand and stops when idle, the way `ssh-agent` does. There is no
install step and no service manager to configure.

## Features

- **A complete command-line interface.** Read, search, sync, flag, move,
  archive, delete, mark read/unread, list and save attachments, reply,
  reply-all, forward, send, and follow a live change stream. `--json` on every
  read command.
- **A documented client protocol.** `Query`/`Command`/`Event` over NDJSON on a
  Unix socket, versioned with a handshake. Writing a third client — a TUI, an
  editor plugin, a cron job — means speaking that, not linking a library.
- **Pluggable connectors.** IMAP and SMTP ship; the UI and the CLI name no
  protocol. A JMAP, Gmail-API or Maildir backend is a new crate plus two match
  arms.
- **Pluggable credentials.** OS keyring, an external command (`pass`,
  1Password's CLI, …), an environment variable, or OAuth2 — resolved per
  connection, never written to disk.
- IMAP sync with a persistent local cache (SQLite) — fast startup, and
  already-synced mail is readable offline.
- A durable outbox. Sending first commits the message locally; the daemon
  delivers it in the background with exponential-backoff retries, surviving
  client exits, network failures and daemon restarts.
- Full-text search across all folders, deduplicated across Gmail's label/folder
  copies of the same message.
- Several accounts at once.

### The desktop app

`birdman-desktop` is included and is a real client, not a demo — but it is a
perk, not the product. It is built on
[GPUI](https://github.com/zed-industries/zed), the UI framework behind Zed:
three-pane layout, a command palette, and sanitized HTML rendering in an
embedded platform webview (WKWebView on macOS, WebKitGTK on Linux, via
[wry](https://github.com/tauri-apps/wry)) with JavaScript disabled and clicked
links opening in your browser. Unstyled messages are rendered dark to match the
app; messages that bring their own styling are left as the sender intended.

It talks to the same daemon over the same socket as everything else. Anything it
can do to your mailbox, `birdman` can do too — that parity is deliberate and is
what keeps the protocol honest.

## Prerequisites

- **Rust** (stable), via [rustup](https://rustup.rs). The pinned toolchain is in
  `rust-toolchain.toml`; rustup installs it for you on the first build.
- **On macOS, the Xcode Command Line Tools** for the linker — `xcode-select --install`.
  Nothing else: WKWebView and the Keychain are part of the system, so the desktop app
  needs no extra packages.
- **Somewhere to keep the credential.** On macOS that is the system Keychain, already present. On Linux it is a running Secret Service provider — `gnome-keyring`, KWallet (with its Secret Service integration), or equivalent. Neither is needed if you configure `auth.type = "command"` or `"env"` instead.
- **Only if you want the desktop app, and only on Linux**, the system libraries GPUI and
  the embedded webview need. The daemon and the CLI need none of them, so
  `cargo build -p birdmand -p birdman-cli` works on a bare machine.

On Debian/Ubuntu — this is the list CI installs, so it is the one that is kept true:

```sh
sudo apt-get install -y --no-install-recommends \
  libwebkit2gtk-4.1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig-dev libfreetype-dev libssl-dev libwayland-dev libxcb1-dev \
  libx11-dev libgbm-dev libvulkan-dev
```

The equivalents elsewhere:

```sh
# Arch
sudo pacman -S --needed webkit2gtk-4.1 libxkbcommon libxkbcommon-x11 fontconfig \
  freetype2 openssl wayland libxcb libx11 mesa vulkan-icd-loader

# Fedora
sudo dnf install webkit2gtk4.1-devel libxkbcommon-devel libxkbcommon-x11-devel \
  fontconfig-devel freetype-devel openssl-devel wayland-devel libxcb-devel \
  libX11-devel mesa-libgbm-devel vulkan-loader-devel
```

On most desktop Linux installs these are already present. If a build fails looking for
one of them, install its `-dev`/`-devel` package and retry.

## Installing

Birdman is distributed through cargo; there are no prebuilt binaries. With the
prerequisites above in place:

```sh
cargo install --git https://github.com/laleshii/birdman --tag v0.1.1 birdman-daemon
cargo install --git https://github.com/laleshii/birdman --tag v0.1.1 birdman-cli
cargo install --git https://github.com/laleshii/birdman --tag v0.1.1 birdman-ui   # desktop app
```

That installs `birdmand`, `birdman` and `birdman-desktop` into `~/.cargo/bin`. Keep
`birdman` and `birdmand` together there — a client looks for the daemon beside its own
binary before falling back to `PATH`.

Drop the last line if you only want the mailbox and the CLI; it is the only one that
needs the Linux system libraries.

### Installing the desktop app as an application

`cargo install` leaves a bare executable, which is not an application: macOS shows only
`.app` bundles in Spotlight and Launchpad, and Linux launchers read `.desktop` entries.
To get an entry you can actually launch, clone and run the installer instead:

```sh
git clone --branch v0.1.1 https://github.com/laleshii/birdman
cd birdman
./scripts/install.sh              # --no-desktop for just the mailbox and the CLI
```

It installs the same binaries and then, on macOS, builds `~/Applications/Birdman.app`
and code-signs it; on Linux it writes `birdman.desktop` into
`~/.local/share/applications` with an absolute `Exec` path, since a launcher does not
run a login shell and will not have `~/.cargo/bin` on its `PATH`.

The macOS bundle carries its own copy of `birdmand`. An app launched from Finder gets
LaunchServices' minimal `PATH`, so the daemon beside the binary is the only one it can
find. Re-run the script after upgrading, or the bundle keeps the older binaries.

Signing is not cosmetic on macOS: a keychain item's access control is tied to the code
signature, so an unsigned build asks permission again every time it is replaced. The
script uses your first codesigning identity, overridable with `BIRDMAN_SIGN_ID` (`-`
for ad-hoc).

Installing from git rather than crates.io is deliberate. The workspace patches three
dependencies to vendored copies (`[patch.crates-io]` in the root `Cargo.toml`), and a
`[patch]` table applies only to builds from within that workspace. Published to
crates.io, the same crates would resolve the unpatched upstream versions and take back
the bugs those patches fix.

## Building

```sh
cargo build -p birdmand -p birdman-cli   # the mailbox and the CLI
cargo build                            # ...and the desktop app
```

Three binaries land in `target/debug/` (or `target/release/` with `--release`):

| Binary | What it is |
|---|---|
| `birdmand` | the daemon that owns the mailbox. You never start it yourself — a client starts one on demand and it stops again when idle |
| `birdman` | the command-line client, and the reference consumer of the protocol |
| `birdman-desktop` | the GPUI application |

The first form skips GPUI and the embedded webview entirely, which is the whole
of the Linux system-library list above. Put `birdman` and `birdmand` on your
`PATH` together — a client looks for the daemon beside its own binary before
falling back to `PATH`, so a development build talks to its own daemon rather
than an installed one.

## Configuring an account

The first run writes a commented-out template to
`~/.config/birdman/config.toml` — `birdman accounts` is enough to trigger it, and
the desktop app shows an onboarding screen pointing at the same file. Uncomment
an `[accounts.*]` block and fill in your details:

```toml
[accounts.personal]
display_name = "Personal Gmail"      # optional; how clients label the account
name = "Ada Lovelace"                # optional; what your outgoing mail is signed with
email = "you@gmail.com"
receiver = { type = "imap", host = "imap.gmail.com", port = 993 }
sender   = { type = "smtp", host = "smtp.gmail.com", port = 465 }
auth     = { type = "keyring", username = "you@gmail.com" }
# save_to_sent = "auto"  # auto (default) | yes | no
```

The table key (`personal`) names the account, and is what `birdman login`,
`birdman authorize` and `--from` match on. `receiver` and `sender` each declare a
connector **`type`** rather than encoding the protocol in a key name, so adding
a protocol never means inventing new keys.

`display_name` labels the mailbox; `name` is what recipients see in `From`.
Keeping them apart matters — falling back to the label is how mail goes out as
`From: Personal Gmail <you@gmail.com>`.

Check it took:

```sh
birdman accounts
birdman check-auth personal    # resolves the credential and tries a real login
```

### More than one account

Add another block. Each gets its own connectors and credentials. The CLI takes
`--account` on reads and `--from` on `send`; the desktop gets a section in the
sidebar and an entry in compose's From picker:

```toml
[accounts.work]
display_name = "Work"
email = "you@corp.com"
receiver = { type = "imap", host = "outlook.office365.com", port = 993 }
sender   = { type = "smtp", host = "smtp.office365.com", port = 587, starttls = true }
auth     = { type = "keyring", username = "you@corp.com" }
```

Defaults: port 993 for `imap`, 465 for `smtp`, `sender.host` falls back to
`receiver.host`, `auth.type` falls back to `keyring`, and `auth.username` falls
back to `email`. `insecure_tls = true` on an account skips certificate
validation — only ever for a local test server.

### Where the password comes from

`auth.type` picks a credential adapter. The password is never in the config
file.

```toml
auth = { type = "keyring", username = "you@gmail.com" }          # OS keyring (default)
auth = { type = "command", command = ["pass", "show", "mail/gmail"] }
auth = { type = "env", var = "BIRDMAN_GMAIL_PASSWORD" }
```

`command` runs the program and reads the secret from stdout (trailing newline
trimmed), which makes `pass`, `gopass`, 1Password's CLI and anything else with a
command-line interface work without Birdman integrating with any of them. `env`
is for containers and CI.

Adapters are consulted on every connection, so a rotated secret takes effect on
the next reconnect rather than at the next restart. Only `keyring` accounts
trigger the first-run password prompt; the others have nothing to prompt for.

After SMTP accepts a message, Birdman files the same rendered bytes in the
account's RFC 6154 `\Sent` folder. `save_to_sent = "auto"` skips that step for
Gmail, which already files authenticated submissions and would otherwise make
two copies. Use `"yes"` or `"no"` to override the detection.

### Gmail with OAuth2

Birdman ships no OAuth client credentials, so you register your own one-time.
It takes about five minutes.

1. In the [Google Cloud console](https://console.cloud.google.com/), create a
   project.
2. **APIs & Services → OAuth consent screen.** Choose *External*, fill in the
   required fields, and add `https://mail.google.com/` as a scope.
3. **Publishing status → Publish app.** This matters more than it looks — see
   the warning below. You do *not* need to submit for verification.
4. **Credentials → Create credentials → OAuth client ID → Desktop app.** Copy
   the client ID and client secret.
5. Put them in the account:

```toml
[accounts.gmail]
email = "you@gmail.com"
receiver = { type = "imap", host = "imap.gmail.com", port = 993 }
sender   = { type = "smtp", host = "smtp.gmail.com", port = 465 }
auth     = { type = "oauth2", provider = "google", username = "you@gmail.com", client_id = "...", client_secret = "..." }
```

6. Grant consent once:

```sh
birdman authorize gmail
```

That opens your browser, catches the redirect on a local loopback port, and
saves the **refresh token** to your keyring. Google will show a "Google hasn't
verified this app" warning — that is expected for an unverified client, and
*Advanced → Go to Birdman (unsafe)* proceeds. From then on Birdman exchanges the
refresh token for a short-lived access token on each connection; the access
token is never written to disk.

> **Leave publishing status on "In production".** While an app is in *Testing*,
> Google expires its refresh tokens after **7 days**, so Birdman would stop
> authenticating every week and need re-authorizing. Publishing removes that
> expiry; it does not require verification.

The `client_secret` Google issues for a *Desktop app* is embedded in software
users can read, and Google documents it as not confidential. PKCE is what
actually protects the exchange, and Birdman always sends it.

`provider = "microsoft"` works the same way for Outlook/Office 365. For anything
else, give `auth_url`, `token_url` and `scope` explicitly instead of `provider`.

OAuth2 does not use an App Password or prompt for your account password. App
Passwords apply only to the `keyring` configuration shown earlier. If the daemon
was already running when you changed the account to OAuth2, run
`birdman daemon restart` after authorization so it reloads the account.

## Using it

Nothing needs starting. The first command you run brings the daemon up:

```sh
birdman ls                          # newest mail in every inbox
birdman ls --folder Archive --unread --attachments --limit 50
birdman search "from the accountant"
birdman read 4821                   # fetches the body if it isn't cached, and marks it read
birdman read 4821 --peek            # ...without marking it read
birdman attachments 4821 --save .
birdman reply 4821 --all --body "Confirmed."
birdman forward 4821 --to ada@example.com < note.txt
birdman move 4821 Archive
birdman mark 4821 unread
birdman outbox                         # queued, sent and failed deliveries
birdman outbox retry 7                 # restart a failed delivery cycle
birdman outbox cancel 8
```

Every read command takes `--json`, so the mailbox composes with everything else:

```sh
# every unread sender, most frequent first
birdman ls --unread --limit 500 --json | jq -r '.[].from' | sort | uniq -c | sort -rn

# archive anything from a newsletter you have already read
birdman search newsletter --json | jq -r '.[] | select(.seen) | .id' |
  xargs -n1 birdman archive
```

`birdman watch` prints a line per change — new mail, a flag edit, a sync
failure — which is what you hang a notifier or a sync script off.

Looking after the daemon:

```sh
birdman daemon status
birdman log --follow      # both processes write here; sync failures land in it
birdman daemon restart    # picks up a credential added after it started
```

### The desktop app

```sh
birdman-desktop
```

Same daemon, same store, same commands underneath.

### Where things live

The SQLite cache and the attachment store are under `~/.local/share/birdman/` on
Linux and `~/Library/Application Support/birdman/` on macOS. The socket is
`$XDG_RUNTIME_DIR/birdman.sock`, or the data directory on macOS; `BIRDMAN_SOCKET`
overrides it, which is how you run a second daemon against a test mailbox.

## Writing another client

The contract is `crates/birdman-proto`: `Query` (reads), `birdman_backend::Command`
(writes) and `Event` (an unsolicited change stream), as newline-delimited JSON
over the socket. A client sends `Hello` with a protocol version, then one JSON
object per line and reads one back per line.

Requests carry an id and are answered in order per connection; events carry
none, which is what tells them apart. `birdman-client` is a Rust implementation
of exactly that and is what `birdman` and `birdman-desktop` both use, but nothing
about the protocol is Rust-specific.

## Project layout

- `crates/birdman-store` — SQLite persistence for accounts, folders, messages, and attachments
- `crates/birdman-mime` — RFC 822/MIME message parsing
- `crates/birdman-backend` — the connector contract: `MailReceiver`, `MailSender`, and the commands a client can issue, naming no protocol
- `crates/birdman-auth` — credential resolution behind a pluggable `AuthAdapter` (keyring, command, env, oauth2)
- `crates/connectors/birdman-imap` — receiving, over IMAP (its own Tokio runtime, independent of any client)
- `crates/connectors/birdman-smtp` — sending, over SMTP
- `crates/birdman-proto` — the client/server contract: `Query`, `Response`, `Event`, and the wire framing
- `crates/birdman-service` — answers it, over the local store and the connectors
- `crates/birdman-daemon` — the daemon: owns the store, the connectors and the sync engine
- `crates/birdman-client` — talks to the daemon over its Unix socket
- `crates/birdman-config` — account configuration, shared by every client
- `crates/birdman-cli` — the command-line client (binary `birdman`)
- `crates/birdman-ui` — the GPUI application (binary `birdman-desktop`), including the reading pane's embedded webview

Only the last of those depends on GPUI. Everything the mailbox *is* — the store,
the sync engine, the connectors, the protocol, the CLI — builds and runs without
it.

## Extending it

There are three seams, and none of them is the UI.

**A new protocol** is a crate under `crates/connectors/` implementing
`MailReceiver`, `MailSender`, or both, plus two match arms in the config. No
client changes: which connector serves an account is declared in that account's
config, so JMAP, the Gmail API or a Maildir reader slots in without anything
above it knowing. A partial implementation is legitimate — a backend returns
`Unsupported` for what it cannot do and the client presents that as a limitation
rather than a failure. See
[`crates/connectors/README.md`](crates/connectors/README.md).

**A new credential source** is one `AuthAdapter` impl in `birdman-auth` and one
`auth.type` in the config. `command` already covers most of the space: anything
with a CLI works without Birdman integrating with it.

**A new client** is whatever can write JSON to a Unix socket. See *Writing
another client* above.

## Appearance (desktop only)

Everything in this section configures `birdman-desktop` and is ignored by the
daemon and the CLI. Same file, separate `[appearance]` and `[theme]` tables —
serde ignores what it is not told about, so the two readers never have to
agree on the file's whole shape.

Everything below is optional, lives in the same `config.toml`, and is re-read
while the app is running — save the file and the window changes. A setting that
doesn't parse is ignored with a warning and its default kept, so a typo costs
you one line rather than the UI.

### Colours

The palette is a set of named roles rather than a list of widgets, so a theme
sets fourteen values and is done:

```toml
[theme]
bg_app     = "#282c34"
bg_sidebar = "#21252b"
bg_message = "#2f343d"   # the reading pane, a step lighter than the chrome
accent     = "#7aa6da"
text_muted = "#8c919c"
```

Name only what you want to change; the rest are inherited. To keep the theme in
its own file — so an external theme switcher can swap it — point at one instead:

```toml
[appearance]
theme_file = "~/.config/omarchy/current/theme/birdman.toml"
```

### What a message row shows

A row is a **gutter** (drawn beside the whole row) plus a stack of **lines**.
Each is a list of slots, and the order you write them is the order they appear:

```toml
[appearance.message_row]
gutter = ["unread_dot"]
lines = [
  ["sender", "flag", "date"],
  ["subject", "attachment"],
]
```

That is the default. To add the body preview as a third line, drop the unread
dot and lead with the recipient instead of the sender:

```toml
[appearance.message_row]
gutter = []
lines = [
  ["recipients", "date"],
  ["subject", "attachment"],
  ["preview"],
]
```

**Hiding something is omitting it.** There is no separate `show_date` flag —
a slot you don't list isn't drawn.

Available slots: `unread_dot`, `sender`, `recipients`, `subject`, `preview`,
`date`, `flag`, `attachment`, and `spacer` (draws nothing, pushes everything
after it to the right edge). Text slots take the leftover width and ellipsise;
annotations hold their size, so a long sender truncates before the date does.

The row height follows from the lines you name — nothing to keep in sync.

### How a slot looks

```toml
[appearance.message_row.style.sender]
size         = 15
weight       = "bold"        # normal | medium | semibold | bold
color        = "text_muted"  # a [theme] role, not a hex
color_unread = "accent"
```

Colours are theme *roles*, so a customised slot still follows whatever palette
is loaded. `color` on its own applies to both read states; add `color_unread`
only when you want them to differ.

### Your own CSS in the reading pane

```toml
[appearance]
reading_css_file = "~/.config/birdman/reading.css"
```

Its contents are appended to the pane's stylesheet, so your rules come last and
win. Hot-reloaded like everything else.

Appended rather than replacing the built-in sheet, deliberately. That sheet is
doing two jobs: taste (typography, colours, width — all configurable above) and
mechanism (stripping a sender's inline paint, the `*` reset, `color-scheme`).
The mechanism is what stops an email rendering dark-on-dark, and its failure
mode looks like an Birdman bug rather than a config mistake. Appending gives you
every override without that cliff.

```css
/* reading.css */
body { font-family: "Iosevka", monospace; }
a { text-decoration: underline; }
blockquote { border-left: 2px solid #7aa6da; padding-left: 12px; }
```

### Hiding whole regions

Slot lists say what a component is made of. This says whether it's there at all:

```toml
[appearance.show]
sidebar             = true
toolbar             = true
message_list_header = true
scrollbars          = true
```

### The reading pane toolbar

Same idea, one list. `spacer` pushes the rest right; `divider` draws a hairline:

```toml
[appearance]
toolbar_actions = ["reply", "reply_all", "forward", "spacer", "move", "flag", "divider", "archive", "delete"]
email_dark_mode = "auto"   # auto | always | never
load_remote_images = "always" # always (default) | never
```

Set `load_remote_images = "never"` to block network images globally. Inline
`cid:` images remain available because they come from the local attachment cache.

## Security

The trust boundary is one Unix user. There is no per-client authentication: a
client that reaches the daemon's socket can read every message, send as you, and
delete anything. That boundary is enforced twice — `birdmand` refuses to bind a
socket whose directory is group- or world-reachable, and checks the peer uid on
every connection it accepts.

Everything Birdman writes is owner-only: the data directory and attachment cache
at `0700`, the database, its WAL sidecars, each attachment, the pid file and the
config at `0600`. Applied on every start, so an install made by an older build
is repaired rather than left as it was.

Mail is stored **unencrypted** at rest. Full-disk encryption is the answer to
that, not anything Birdman does.

Remote images are allowed by default and can be blocked globally with
`appearance.load_remote_images = "never"`. HTML is sanitized with `ammonia` and
rendered with JavaScript disabled and top-level navigation refused.

Dependencies are checked by `cargo deny` on every pull request and weekly on a
schedule; the policy is in `deny.toml`.

To report something, open an issue — or email the address in the git log if it
shouldn't be public.

## License

MIT — see [LICENSE](LICENSE). This includes `birdman-desktop`: it now uses the
crates.io gpui release, which does not pull Zed's GPL `zlog` or `ztracing`
crates. The supply-chain policy in `deny.toml` prevents that old dependency
path from being reintroduced unnoticed.
