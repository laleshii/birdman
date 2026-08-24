---
id: on-demand-folder-sync
title: 'The sync model: INBOX automatic, everything else on demand'
altitude: 2
topics:
- sync
relations:
- type: part_of
  target: imap-sync-engine
- type: references
  target: connector-boundary
summary: Why only INBOX auto-syncs, the five-minute staleness TTL for other folders, and why user actions trigger a resync of the folders they touched.
---

# The sync model: INBOX automatic, everything else on demand

Birdman does not keep every folder continuously synced. The rule:

- **INBOX** syncs on startup and stays live via IDLE ([[sync-supervisor-loop]]).
- **Every other folder** syncs when you click it *and* it has not been synced in
  the last five minutes, or whenever you press Sync.
- **Actions trigger a resync of the folders they affected.**

## Why

On a Gmail account, syncing every folder means syncing "All Mail", which mirrors
the entire account. Doing that on startup made the app unusable while it ran, to
keep folders fresh that the user may never open. INBOX is the only folder the
user is reliably looking at.

## The TTL

`AppState` (`crates/birdman-ui/src/state.rs`) holds
`folder_last_synced: HashMap<FolderId, Instant>` and `FOLDER_SYNC_TTL` of 5
minutes. `sync_folders_if_stale` checks it before dispatching
`Command::SyncFolder`. Clicking a folder repeatedly does not re-sync it;
`sync_now` (the Sync button) clears the map so everything is stale again.

An `Instant` map means the TTL resets when the app restarts. That is
intentional — a fresh launch should re-check.

## Actions resync what they touched

Flagging, archiving and deleting change state on the server, and for a move that
means *two* folders are now wrong: the source and the destination. After the
command resolves, the UI marks the affected folders stale and re-syncs them,
rather than waiting for a TTL to expire or leaving the counts wrong until the
next manual sync.

This is why the commands in `birdman-backend` are per-message rather than batch:
the UI knows exactly which folders a command touched.

## Consequence when adding a feature

Anything that changes server state must answer "which folders does this
invalidate?" and mark them. Skipping it leaves stale counts and a message list
that disagrees with the server until the next manual Sync — a bug that is easy
to miss locally, because the five-minute TTL papers over it if you take too long
to look.
