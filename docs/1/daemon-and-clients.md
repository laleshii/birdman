---
id: daemon-and-clients
title: 'birdmand: the daemon, its transport, and its lifecycle'
altitude: 1
topics:
- architecture
- cli
relations:
- type: refines
  target: service-boundary
- type: depends_on
  target: cli-client
summary: One process owns the mailbox; clients speak NDJSON over a Unix socket. Why idle shutdown rather than explicit stop, why two connections, and the failure modes seen so far.
---

# birdmand: the daemon, its transport, and its lifecycle

`crates/birdman-daemon` owns the store, the connectors, the credentials and the sync
engine. `crates/birdman-client` is what everything else holds. The desktop app
and the CLI are both clients; neither opens a database or a mail connection.

## Why a single owner

Reads could be done by any process -- SQLite is in WAL mode -- but writes
cannot. Two processes each holding IMAP connections means two IDLE loops on one
mailbox, two credentials in memory, and two writers racing on the same rows.

Reads go through the daemon too, even though they need not. One code path that
is always exercised beats a fast path that is used and a slow path that is
merely allowed to exist.

## Transport

**NDJSON over a Unix domain socket.** One JSON object per line, both
directions.

A Unix socket needs no port and no port-collision story, and its access control
is filesystem permissions -- the mailbox is readable by exactly the user who
owns it, enforced by the kernel rather than by a token Birdman would have to
invent and store. It is also debuggable with `socat`, which is how the protocol
was tested before either client could speak it.

Requests carry an id because the connection is multiplexed; events carry none,
which is what distinguishes them.

`BIRDMAN_SOCKET` overrides the path, which is what makes a second daemon
possible for tests without disturbing a running one.

## Lifecycle

**Clients start it.** Any client that finds nothing listening spawns `birdmand`
and waits -- the arrangement `ssh-agent` and `gpg-agent` use. There is no
install step and no service manager; the daemon is whichever process got there
first.

**Idle shutdown, not explicit stop.** Configured under `[daemon]`
(`auto_stop`, default true; `idle_timeout`, default 60s). When the last client
disconnects, the daemon waits and exits.

An explicit "stop" from the desktop was the obvious alternative and is wrong:
it races with whatever else is connected. A CLI command in flight when the
window closes would have the socket pulled out from under it. Idle expresses
the real condition -- *nobody needs this* -- without either client knowing
about the other.

The watchdog **polls** rather than arming a timer on the last disconnect,
because the condition that matters is "*still* idle a moment later": a desktop
restart or a second CLI command inside the window should keep the daemon alive,
and a disconnect-armed timer would have to be cancelled and re-armed to say
that.

**Stale sockets are detected by connecting**, never by whether the file exists.
A socket whose daemon is alive must not be removed; a file whose daemon is gone
must not block every future start.

**The daemon is found beside the running binary** before `PATH`, so a
development build talks to *its own* daemon. A stale installed `birdmand` would
otherwise produce a version mismatch that looks like a protocol bug.

## Client shape

**Blocking reads, futures for writes.** `query` blocks, which sounds wrong on a
UI thread until you notice what it replaced: the desktop already read SQLite
synchronously, contending for the same mutex the sync engine holds. A socket
round trip is tens of microseconds and the daemon answers the same query
against the same database, so the latency profile is unchanged -- and every
call site kept working without an async refactor.

**Two connections, not one multiplexer.** Requests on one, events on another.
The protocol supports interleaving; a client that never does needs no
correlation logic.

**A command gets its own connection**, so awaiting one never queues behind
another in flight. Commands are clicks and keystrokes; a connection each costs
nothing.

**One silent retry on transport failure.** The daemon stops when idle, so a
client holding an unused connection can legitimately find it closed. Without
the retry, every command after a pause fails once.

## Two bugs the daemon introduced, and their fixes

**Clients could not learn their initial state.** Events are deltas and are
never replayed, so a desktop that connected after the initial sync had finished
missed `SyncIdle` and sat on a placeholder status forever. The status line was
*assuming* a state it had no way to know.

`Query::SyncStatus` fixes it: the daemon tracks per-account state from the same
events it publishes, and a client asks on connect. The state is recorded
*before* the event is sent, so a client querying immediately after receiving
one cannot see something older than the event it just got.

The general rule the protocol was missing: **events are deltas; there must be a
query for current state.** Anything else forces clients to guess.

**Client commands announced nothing.** Only the sync supervisor published
events. That was harmless when the only client was the one issuing the command
-- it knew what it had just done. With several clients, a change nobody
announces is a change nobody else sees: a message moved from the CLI would
never appear in the desktop.

`Service::execute` now resolves the affected folders **before** running the
command -- a move or delete makes its own folder unfindable afterwards -- and
publishes on success. A move announces both folders, since the source is only
knowable while the message is still in it.

## Version skew, and the handshake that names it

A rebuild leaves the old daemon running and the next client speaks a protocol
it does not know. Observed before the fix: `unknown variant Message`, from a
daemon predating `Query::Message` -- legible, but only if you know the protocol.

`Hello { version }` is now the first thing a client sends, and
`PROTOCOL_VERSION` is bumped whenever a client and daemon of different builds
could misunderstand each other. A mismatch is refused up front with a sentence
naming the fix, rather than failing later on whichever field happens to differ.

`birdman daemon status | stop | restart` covers the rest.

**`stop` deliberately skips the handshake.** A daemon old enough to need
stopping may be too old to understand `Shutdown` -- which is precisely the
situation after a protocol change, and precisely when you most want to stop it.
So it asks politely, waits, and falls back to `SIGTERM` via a pid file written
beside the socket.

That fallback has one gap by construction: a daemon predating the *pid file*
can be stopped by neither route. It happened once, during the build that
introduced it, and the error says `pkill -f birdmand`.

## An event you caused is still an event you receive

Publishing on every command was the fix for "clients never see each other's
changes". Applied to *every* command it caused a second bug: `OpenMessage`
announced `MessagesChanged`, so every arrow keypress in the desktop refreshed
its own message list -- reacting to an event it had itself caused, telling it
something it already knew. The visible symptom was arrow keys no longer
scrolling.

The fix is to publish only what actually changed:

- `FetchBody` -- nothing a list shows.
- `OpenMessage { mark_read: false }` -- nothing at all.
- `OpenMessage { mark_read: true }` -- announced; the unread count really did
  change.

There is no origin filtering: a client cannot tell its own events from
another's, and adding that would mean threading a client id through the whole
protocol to solve a problem that mostly disappears once commands stop
announcing non-changes.

## Both processes write one log

`birdmand` and every client log to the same file, which is what makes reading it
worth surfacing: a sync failure happens in the *daemon*, where nothing the user
typed can see it.

Both clients expose it. `birdman log [--lines N] [--follow]` tails it; the
desktop's status line opens the last 400 lines when clicked, because that line
has room for one clause of something that can be a paragraph.

`BIRDMAN_LOG=debug` adds the running commentary -- published events, selection
changes. Default is `info`: lifecycle, and nothing per keystroke.

## Bodies stay in the JSON, and that was measured

The obvious worry about NDJSON is a large HTML body as one line. Measured
across 9,278 stored bodies: **largest 846KB, average 41KB, none over 1MB.**

A separate byte channel would be real machinery for a problem this mailbox does
not have. Worth revisiting if bodies ever routinely exceed a few MB; not worth
building on suspicion.

## The access-control claim is now enforced

The sentence above about filesystem permissions was, for a while, a description
of an intention rather than of the code: nothing set a mode on the socket, and
it was created world-connectable at the umask. It is enforced now -- a directory
check before binding, `0600` on the socket, and a peer-uid check on every
accepted connection. See [[security-boundaries]].

## A subscription belongs to one daemon process

`Client::subscribe` opens its own connection and reads events until it ends.
When the daemon exits -- a crash, or `Client::restart_daemon` -- that stream
ends and does not come back on its own.

The desktop's event pump in `crates/birdman-ui/src/main.rs` is therefore a `loop`
around the `while let`, not a bare `while let`. A bare one left the window
silently event-dead after any daemon restart: still drawing, still answering
queries through the reconnecting query path, but never updating again. That is a
much worse failure than an error, because nothing about it looks broken.

Resubscribing is followed by an explicit refresh of folders, messages and sync
status. Events are deltas, so whatever changed while nothing was listening was
never announced and would otherwise stay invisible until the next unrelated
change.

## Restarting from inside the window

`AppState::restart_daemon`, wired to a button and the `R` key in the log panel.

It exists for one specific dead end: a connector builds its auth adapter when
the daemon starts, so a credential added afterwards -- `birdman login`, or an
authorization dialog answered too late -- is invisible to the running process.
Sync then fails permanently against a password that is sitting right there.
Restarting is the fix, and it belongs next to the log that shows the failure.

`Client::restart_daemon` is best-effort about the stop: a daemon too wedged to
answer `Shutdown` is exactly the one worth replacing. It waits for the socket to
stop answering before starting a replacement, because otherwise the new daemon
finds a socket that still connects, concludes another daemon is live, and
refuses to start.

## A client should not act on the echo of its own change

`Command::OpenMessage { mark_read: true }` publishes `MessagesChanged`, which is
correct -- other clients need to know. The client that *caused* it does not: it
applied the seen flag locally at the same time.

Acting on it anyway is what made arrowing through unread mail lurch. Every
keypress opened a message, every open marked it read, every mark published an
event, and every event re-read and replaced the whole list -- two blocking
queries and two hundred rows rebuilt per keystroke, to learn a flag already set.

`AppState::self_marked_read` holds one folder id, set when the desktop issues an
open that will mark something read and consumed by the matching event. One slot
rather than a counter: arrow keys are sequential, each press replaces the
previous expectation, and a missed event is corrected by the next press instead
of leaking a skip forever.

It now covers every mutation, not just marking read: `dispatch` -- the single
path every delete, move, archive and flag goes through -- records the folder
before issuing. All of them already apply to the visible list locally, so the
announcement never carries anything new.

Deleting made the cost obvious. The echo refresh re-ran the folder query, and in
the unread-only view that dropped every message read since the view opened, so
removing one message appeared to remove several.

This is still a narrow fix. The general problem -- a subscriber cannot tell its
own writes from anyone else's -- would want the event to carry the originating
request, which is a protocol change.

## Opening a message is bounded

`OPEN_TIMEOUT` (30s) in `AppState::select_message`. Measured against the daemon,
an `OpenMessage` that has to fetch the body off the server answers in about
0.7s, so anything near the bound is stuck rather than slow. Without it the pane
sits on "Loading message..." indefinitely, with nothing to click and nothing in
the log.

## One account finishing is not "Synced"

`Event::SyncIdle` names the account it is about, and the desktop was ignoring
that and writing "Synced" -- so the label appeared the moment the quicker of
two accounts finished, with the other still downloading.

The handler calls `refresh_sync_status` instead, which reads every account's
state and takes the worst: a failure beats a sync in progress, which beats
idle. That is the only answer one line can honestly give about two mailboxes.

Only on idle. Progress events are frequent, and their optimistic label --
"Syncing X..." -- is already true whichever account it came from.
