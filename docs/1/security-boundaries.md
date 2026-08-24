---
id: security-boundaries
title: 'Security boundaries: who can reach the mailbox'
altitude: 1
topics:
- security
relations:
- type: part_of
  target: birdman-overview
- type: references
  target: daemon-and-clients
summary: The local trust boundary (socket permissions plus a peer-uid check), what the app makes owner-only and why, the supply-chain policy in deny.toml, and the GPL dependency the desktop binary carries.
---

# Security boundaries: who can reach the mailbox

## The boundary is "one Unix user", enforced twice

Everything Birdman protects sits behind a single claim: the mailbox is reachable
by exactly the user who owns it. There is no per-client authentication, no
token, no capability — a connected client can read every message, send as the
user, and delete anything.

That claim is now enforced in two independent places, in `crates/birdman-daemon/src/main.rs`:

1. **Filesystem.** `bind()` refuses to bind a socket whose *directory* is
   group- or world-reachable, then chmods the socket to `0600`. The directory is
   the control that matters — a `0600` socket in a world-writable directory can
   be unlinked and replaced with someone else's.
2. **Peer credentials.** Every accepted connection is checked with
   `peer_cred()` against `getuid()`. Root is refused along with everyone else;
   it can read the mailbox by other means anyway, so allowing it would only
   widen the surface.

Two mechanisms for one boundary is deliberate. Modes are set at start and can be
changed afterwards by anything running as the user; a uid comparison holds
regardless and costs one syscall.

### Why the second check needs three outcomes, not two

`peer_check` returns `Ours`, `NotOurs(uid)`, or `Gone(err)`. The third is not
padding: `ensure_daemon` and `wait_for_stop` probe liveness by connecting and
dropping immediately, so `getpeereid` returns `ENOTCONN` several times a second
while any client waits for the daemon to come up. Folding that into "refused"
put a warning in the log for every probe and buried the one line that would
matter — a real uid mismatch.

## Owner-only is applied, not assumed

Before this existed, nothing set permissions on anything except the config
template. The socket was `0755`, `mail.db` was `0644`, the attachment cache was
`0755` — every message body world-readable. On macOS that was survivable only
because `~/Library` is itself `0700`, which is an accident of the platform, not
a design. On Linux, where the data directory lands in `~/.local/share`, the
claim above was simply false.

`birdman_config::restrict_to_owner` (`0700` for a directory, `0600` for a file)
is now applied to the data directory, the database and its `-wal`/`-shm`
sidecars, the attachment cache and every blob in it, the pid file, and the
config. `birdman-store` carries its own copy of the helper rather than depending
upward on `birdman-config` to save six lines.

It runs on **every** start, not only at creation, so an install made by an
earlier build is repaired rather than left exposed. That behaviour is what
`a_store_opened_by_an_older_build_is_repaired` pins.

The first version of that claim was **false for the attachment cache**, and
counting the modes on a real store is what showed it: 208 of 219 shards were
still `0755` holding 537 `0644` blobs. A write fixes its own shard and its own
file, and `init` fixed only the top of the tree -- so shards created before any
of this kept their modes indefinitely, with the `0700` above them as the only
thing between the mail and anyone else on the machine. Which is the
defence-by-accident the whole exercise was meant to replace.

`restrict_tree` walks the cache at open instead. One `stat` per entry over a
two-level tree of at most 256 shards, and it converges: files written afterwards
restrict themselves, so later runs find nothing to do.

## What was already sound

Worth recording so it is not re-audited: MIME parsing is bounded
([[mime-hardening-rationale]]), SQL is fully parameterized, attachment paths are
content-addressed so a sender's filename never reaches the filesystem, OAuth
uses PKCE S256 with a validated `state` on a loopback-bound listener
([[oauth2-flow]]), and the reading pane runs with JavaScript disabled and
top-level navigation refused ([[reading-pane-webview]]).

## Supply chain

`deny.toml` plus the `supply chain` job in `.github/workflows/ci.yml`. It runs
on pull requests *and* weekly on a cron, because advisories land against code
that has not changed.

Two settings are judgement calls worth knowing about:

- **`unmaintained = "workspace"`.** The first run found thirteen unmaintained
  crates and every one arrived through gpui or wry — gtk3 bindings,
  `ttf-parser`, `proc-macro-error`. None are actionable without replacing the UI
  framework, and a check that is permanently red is a check nobody reads.
  Vulnerabilities are still denied everywhere, transitive or not.
- **`allow-git` for the zed-industries repos.** gpui has no crates.io release,
  so it is pinned to a rev ([[gpui-dependency-pinning]]). That pin is what makes
  the allowance safe, and it is also why gpui is the one dependency no advisory
  database will ever warn us about.

Every workspace crate carries `publish = false`. It is true — these are an
application, not libraries — and it is what makes cargo-deny's
`allow-wildcard-paths` apply to our path dependencies.

## The desktop binary is GPL, whatever the LICENSE file says

`birdman-ui` → `gpui` → `ztracing` → `zlog`, and `zlog` is **GPL-3.0-or-later**.
gpui itself is Apache-2.0, which is why this is easy to miss; cargo-deny found
it on the first run.

Birdman is MIT. Linking GPL-3.0 code means the *combined work* is GPL-3.0, so the
desktop binary cannot honestly be distributed under MIT as it stands.

Nothing else is affected. `birdmand`, `birdman`, the store, the connectors and the
protocol crates never touch gpui, so the daemon and CLI stay cleanly MIT.

Resolving it means relicensing the desktop binary to GPL-3.0, or patching
`ztracing` out of the gpui build. The exception in `deny.toml` is scoped to
those three crates by name, so any *other* GPL dependency still fails the build.

## A sender's filename is not a filename

`safe_attachment_name`. Blobs are content-addressed, so until now no filename a
sender chose had ever reached the filesystem -- that was listed above as one of
the things already sound. Handing an attachment to another application ends
that: a file saved out with `birdman attachments --save` has to arrive called
`invoice.pdf`.

The hazard is not theoretical. Document scanners routinely produce names of
the shape `9876543210 / 20240117 121339.PDF` -- a reference number, a separator
with spaces around it, a timestamp -- and `dir.join()` on that silently writes
into a subdirectory. A `..` leaves the directory entirely.

Last path component only, both separators (a Windows sender's `\` is not a
separator here and would otherwise survive into a name), no control characters
or bidi overrides (`U+202E` is how `invoice<RLO>fdp.exe` displays as
`invoiceexe.pdf`), not `.` or `..`, and bounded in **bytes** on a character
boundary. `None` when nothing usable is left, and the caller falls back to the
content hash, which is always a valid name.

Note what is *not* rejected: a name containing `..` in the middle. A Dutch
invoice ending `..._B.V..pdf` -- the `B.V.` abbreviation running into the
extension -- is ordinary, and a blanket "contains `..`" rule would break it.

## Materialised attachments are marked as downloaded

`Store::attachments` copies each blob into `attachment-files/<message id>/` under
its [`safe_attachment_name`], because a file handed to another application has
to arrive called `invoice.pdf` rather than `a3f9...`.

Three things go with that copy:

- **Per message**, so two attachments with the same name cannot overwrite each
  other.
- **Copied, not hard linked.** A link would let an editor write back through it
  and corrupt the content-addressed original, which every other copy of that
  message shares.
- **`com.apple.quarantine`**, set by hand. macOS stamps downloads with it and
  Gatekeeper reads it on open, so an executable that arrived by mail gets a
  dialog rather than running. Nothing applies it for us -- we write these files
  ourselves -- and the entire point of materialising them is that they leave the
  app. Clicking a pill hands the file to the system's default handler, which
  runs whatever the extension is associated with; the attribute is what stands
  between that and a `.command` from a stranger.
