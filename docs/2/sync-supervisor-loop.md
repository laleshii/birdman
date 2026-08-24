---
id: sync-supervisor-loop
title: 'The per-account supervisor loop: connect, sync INBOX, IDLE, back off'
altitude: 2
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
summary: run_account's forever-loop with exponential backoff, event-emission ordering, INBOX-only IDLE and startup sync, and the per-folder body backfill that replaced eager prefetch.
---

# The per-account supervisor loop: connect, sync INBOX, IDLE, back off

`crates/connectors/birdman-imap/src/supervisor.rs`. One task per account, running
forever.

## The outer loop is a crash barrier

`run_account` calls `run_account_once` in a loop. *Any* error — auth failure,
network drop, malformed server response — is caught here, turned into a
`SyncEvent::SyncError`, and followed by a sleep and a reconnect. Backoff is
exponential: `MIN_BACKOFF` 2s, doubling, capped at `MAX_BACKOFF` 300s, reset to
the minimum on a clean pass.

This is why `run_account_once` can use `?` freely and why one account cannot
take down another.

## Startup syncs the folder list and INBOX first

The supervisor lists folders, then syncs **INBOX and nothing else**. It does not
walk every folder's envelopes any more.

That change came from the sync model: only INBOX is downloaded during startup,
because it is the only folder the user is guaranteed to care about immediately.
Walking every folder made Gmail download "All Mail", a mirror of the entire
account, before the app was usable.

After startup, already-synced non-INBOX folders are checked periodically with
`STATUS (MESSAGES UNSEEN UIDVALIDITY UIDNEXT)`. A changed summary triggers a
real folder sync; never-opened folders remain lazy because they have no stored
UIDVALIDITY. Since that sync changes the selected mailbox, the supervisor must
reselect INBOX before entering IDLE again. On-demand sync remains available for
first opens and explicit refreshes; see [[on-demand-folder-sync]].

## Event ordering is load-bearing

`FoldersListed` is emitted **immediately after** `sync_folder_list` (cheap:
`LIST` + upsert, no envelope fetch) and *before* any envelope work. This was a
real fix: waiting for a sync to finish before telling the UI the folder list
existed left the sidebar empty for as long as that took.

`FolderSyncing { folder_name }` gives the status line live progress instead of
one frozen "Syncing…". If you add work to this loop, preserve the property that
the UI learns about cheap things early.

## IDLE on INBOX only

After the startup pass the supervisor finds INBOX
(`eq_ignore_ascii_case("INBOX")` — a reserved name per RFC 3501, not a
`SPECIAL-USE` attribute), `SELECT`s it, and checks `server_supports_idle`:

- IDLE supported → `idle_once` in a loop with `IDLE_WAIT` of 5 minutes. An
  `IdleOutcome::RefreshTimeout` just starts a fresh IDLE (`continue`); only
  `Activity` falls through to a re-sync.
- Not supported → `tokio::time::sleep(POLL_INTERVAL)` instead.

IMAP allows exactly one selected mailbox per connection, so multi-folder IDLE
would need a connection fan-out. `CoreError::NoInbox` is returned if the account
has no INBOX at all.

`IDLE_REFRESH_INTERVAL` is 25 minutes, under the RFC 2177-suggested ~29, because
real servers (Gmail, corporate proxies) drop idle connections around there.
Refreshing proactively beats discovering it through an error.

## Body backfill lives in the sync layer, not the UI

`backfill_folder_bodies(session, store, folder_id, budget) -> usize` fetches
bodies for messages that lack them, newest first, within
`BODY_BACKFILL_MONTHS` (6), in batches of `BODY_BATCH` (20), up to `budget`
messages (`BODY_BUDGET_PER_SYNC` = 200). It runs at the end of
`Command::SyncFolder` because the mailbox is *already selected* on that
connection.

This replaced an earlier `eager_fetch_recent_bodies` that only covered the 30
newest INBOX messages.

**It deliberately lives here rather than in the UI.** An earlier attempt drove
backfill from `AppState`, and it was pathological: the session cache reconnects
whenever the selected mailbox changes, so it reconnected per message; and
failures never marked the row, so the same batch was requeried forever. The
result was 98% CPU and store-mutex starvation that froze typing. Anything that
needs a selected mailbox and a batch belongs in the sync layer.

Two invariants to preserve:

- **Bounded.** One call can never run unboundedly; callers repeat it to make
  progress.
- **Best-effort, with a per-call failure set.** A message moved or deleted
  mid-pass is skipped, not fatal — but it must be *recorded* as failed for the
  duration of the call, or the batch loops forever.

After each success it calls `copy_body_to_siblings`, since Gmail exposes the
same message in several folders and downloading it once is enough.

## The backoff that could never recover

`run_account_once` connects, syncs, and then idles forever -- its last statement
is an infinite loop, so it only ever returns by failing. The caller nonetheless
had a `else { backoff = MIN_BACKOFF }` branch keyed on it returning `Ok`.

That branch was unreachable, which meant the backoff only ever grew. An account
whose connection dropped roughly once an hour -- ordinary for Gmail -- climbed
2s, 4s, 8s ... to the 300s ceiling within a working day and stayed there, so
every subsequent reconnect waited five minutes despite the previous session
having been healthy for the whole hour before it.

Two changes, and the second is the one that stops it coming back:

- The reset is now keyed on **how long the session lasted**, not on how it
  ended. Past `HEALTHY_SESSION` (120s, comfortably past the initial sync) a drop
  is a fresh failure and the backoff starts over. Below it, the connection never
  really worked and escalation is correct.
- `run_account_once` returns `Result<Infallible, CoreError>`. The success case
  is now uninhabited, so a "reset on success" branch does not compile. The
  comment explaining the bug would have rotted; the type cannot.

`advance_backoff` is split out from the loop purely so the escalate-and-recover
sequence can be tested without a mail server or a five-minute test.

## Jitter, and why Gmail lies about credentials

Reconnects are jittered across the second half of their window. Two accounts on
the same host go down together when the network does, and without jitter they
come back in lockstep.

That matters more than it sounds, because **Gmail answers a burst of logins with
`[AUTHENTICATIONFAILED] Invalid credentials (Failure)`** -- the same code it uses
for a genuinely wrong password. A night of logs alternating `connection lost`
with `AUTHENTICATIONFAILED` looked like a broken credential and was actually a
lockstep reconnect being throttled; `birdman check-auth` passed against both
accounts the whole time.

So the status line never reports that code as a bad password. It points at
`birdman check-auth <account>`, which is the thing that distinguishes them.

The jitter is derived from the sub-second component of the wall clock rather
than an RNG, to avoid a dependency for something this coarse.

## Sync errors name the account

`SyncEvent::SyncError` always carried an `account_id`, but both the supervisor's
own log line and the desktop's dropped it. With two accounts configured, every
failure in the log was unattributable -- which is precisely why the above went
undiagnosed for a day. The supervisor logs `config.username` and the desktop
resolves the id to a display name.
