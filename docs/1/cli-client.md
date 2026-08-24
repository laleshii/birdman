---
id: cli-client
title: 'The CLI: the primary client'
altitude: 1
topics:
- cli
- architecture
relations:
- type: refines
  target: service-boundary
- type: depends_on
  target: birdman-overview
summary: What birdman does today, the feature-parity rule against the desktop, how reply and attachments reach the CLI, and what building it proved about the Query contract.
---

# The CLI: the primary client

`crates/birdman-cli`, binary **`birdman`**. The desktop app is
**`birdman-desktop`**.

The CLI takes the plain name deliberately. It is the primary client, not a
companion: features are built for it first and given a desktop UX afterwards.

## The parity rule

**Anything the desktop can do to the mailbox, `birdman` can do.** Presentation is
allowed to be desktop-only -- the sidebar toggle, the dark-rendering switch, the
window itself -- but nothing that reads or changes mail is.

This is not aspirational tidiness. The parity is what keeps `birdman-proto`
honest: a capability that exists only in the app is a capability that never had
to be expressed in the protocol, and a third client would have to reinvent it.
An audit against the desktop's command palette found five such gaps, all of them
thin wrappers over protocol the daemon already spoke:

| Was desktop-only | Now |
|---|---|
| open a message (fetch its body, mark it read) | `birdman read` issues `OpenMessage`; `--peek` skips the flag |
| reply / reply-all / forward | `birdman reply [--all]`, `birdman forward --to` |
| attachments | `birdman attachments [--save DIR]` |
| filter to messages with attachments | `birdman ls --attachments` |
| the address book behind compose's autocomplete | `birdman contacts` |

`birdman read` is the one worth singling out. It only ever printed a *cached*
body, so on a large mailbox most messages answered "no cached body yet" while
the desktop simply fetched them. That is the shape of the whole class: the
protocol could always do it, and only one client asked.

## Everything goes through the daemon

`birdmand` owns the mailbox. This binary opens no database and no connection to
a mail server -- it speaks the protocol and resolves names to ids.

An earlier version read the store directly, which WAL mode makes safe. Routing
reads through the daemon too was a deliberate trade: one code path that is
always exercised beats a fast path that is used and a slow path that merely
exists. See [[daemon-and-clients]].

`authorize`, `check-auth` and `login` are the exception -- account setup opens
its own short-lived connection and writes nothing to the store.

## Commands

```
birdman accounts
birdman folders [--account ID|prefix]
birdman ls [--folder NAME] [--unread] [--attachments] [--limit N]
birdman search <text>
birdman read <id> [--peek]     fetches the body if it is not cached
birdman attachments <id> [--save DIR]
birdman contacts [--limit N]

birdman sync [--folder NAME]
birdman mark <id> read|unread
birdman flag / unflag <id>
birdman move <id> <folder>
birdman archive <id>
birdman delete <id>            into Trash; see [[folder-and-uid-sync]]
birdman send --to A --subject S
birdman reply <id> [--all]
birdman forward <id> --to A
birdman watch                  the event stream
birdman log [--lines N] [--follow]

birdman login <account>        store a password in the keyring
birdman authorize <account>    OAuth2 consent
birdman check-auth <account>   resolve a credential and try the login
birdman daemon status|stop|restart
--json on every read
```

Account setup lives here rather than in the app. The desktop used to own the
password prompt, which meant the *app* had to be running -- and be the first
thing you ran -- before mail could sync. The daemon now serves whoever is
configured, and a missing credential is a sync failure the desktop reports with
a pointer at `birdman login`.

`ls` with no folder means every inbox -- the same merged view the desktop calls
"All accounts", falling out of `Query::Messages` taking a folder *set*.

`--account` resolves by id or by a case-insensitive prefix of the email or
display name. Typing a number found in another command's output is not an
interface.

## What building it proved

**The contract held.** All eight queries covered a second client with nothing
added. That was the point of building the read half before committing to a
transport: a gap discovered now is cheap, a gap discovered after socket framing
is not.

**Folder ordering being server-side paid off immediately.** `birdman folders`
and the desktop sidebar list folders in the same order because neither decides
it. Had `is_default_folder` stayed in the UI, the CLI would have re-implemented
it and drifted.

## Writes resolve names to ids, and nothing more

A write command's whole job is turning `birdman move 84288 Archive` into
`MoveMessage { message, to_folder }`. Two things worth knowing:

**`SetFlags` replaces the whole set.** Sending only `flagged` would silently
mark the message unread. `flag` reads the current flags and flips one.

**Archive means `\Archive`, or `\All` if the server has none.** That fallback
is what makes it work on Gmail, which exposes no `\Archive` -- archiving there
means dropping the INBOX label, which over IMAP is a move into All Mail.

**A special-use folder wins a name tie.** On Gmail an account can hold both a
user folder literally called `Trash` and `[Google Mail]/Trash`, and
`birdman move 42 Trash` means the second. Matching the name exactly is not the
same as matching the intent.

## Composing without a compose window

`reply` and `forward` build their draft with `birdman_backend::reply_draft` /
`forward_draft`, from a `ParsedMessage` reconstructed out of the stored envelope
by `birdman_backend::parsed_from_summary`.

That reconstruction used to live in `birdman-ui` as a private method. Moving it
into `birdman-backend` is what makes CLI and desktop replies identical rather than
merely similar -- reply-all membership and honouring `Reply-To` are contract,
not presentation, and two front ends deriving them separately is how one of them
ends up answering a `no-reply@` box. There is a test in `birdman-backend` pinning
the whole round trip from stored columns to draft.

Written text goes *above* the quoted original, from `--body` or stdin.

**The signing name comes from config, never from `display_name`.** `send` used
`display_name` and so mailed people as `From: Gmail <you@gmail.com>` -- the same
bug the desktop had and fixed. The store has no column for a signing name, so
the CLI reads `name` out of the account's config section.

## The gaps it exposed

**A client holding only ids could not act on anything.** `flag` needs the
current flags, `move` needs the account -- and the list queries answer "what is
in this folder", not "what is this thing". `Query::Message` was added for the
CLI and is what any future client will need first.

**The store's `text` column is not reliably text.** `mail-parser` reports the
HTML part as the text body when a message has no `text/plain` alternative, so
`get_message_body` can hand back markup in the text slot.

The desktop never noticed -- it renders in a webview either way. A terminal
prints tags.

`birdman read` therefore checks rather than trusts (`looks_like_html`) and
flattens markup for display: dropping `style`/`script`/`head`, turning block
boundaries into line breaks, decoding entities, squeezing runs of blank lines.
A single blank line between paragraphs survives, because that is paragraph
separation and removing it makes prose harder to read.

The check is a prefix test, and a prefix test is not enough on its own: real
mail puts an XML prolog in front of the opening tag, which reported plain text
and printed a wall of markup. It now steps over a prolog and also scans the
first 2KB for `<html>`.

**`--save` sanitises the filename it writes.** A sender chooses that string, and
`attachments --save .` is the one place it reaches the caller's filesystem, so
only the last path component survives and control characters are dropped. The
store applies the same rule when materialising; see [[attachment-pipeline]].

That flattening is correctly a *client* concern -- the terminal is not a
browser -- but the underlying column being misnamed is worth knowing about.

## `--json` is a contract

Written by hand rather than derived. The shapes are a handful of flat objects,
and keeping them in one file makes the promise to whatever parses them visible
in one place instead of spread across derive attributes.

Absent values are `null`, never `""`: a consumer must be able to tell "no
subject" from an empty one.

## Auth commands moved here

They lived in the desktop binary, which meant a GUI application shipped a
command-line interface nobody could discover. They are CLI-shaped: they print,
they block on a browser, and their failure modes are read rather than clicked.

`AuthConfig::adapter()` and `KEYRING_SERVICE` moved to `birdman-config` in the
process, so both clients resolve credentials identically. A CLI writing a token
the desktop could not read would be a silent and baffling failure.
