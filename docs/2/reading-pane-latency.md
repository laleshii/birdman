---
id: reading-pane-latency
title: Where the time goes when switching messages
altitude: 2
topics:
- ui
- performance
relations:
- type: references
  target: daemon-and-clients
- type: references
  target: local-message-store
summary: 'Measured latency of selecting a message: the store mutex serialising reads behind sync writes, blocking socket calls on the main thread, and a per-frame document scan.'
---

# Where the time goes when switching messages

Written after "the app is super slow when switching messages", which turned out
to be three unrelated costs stacked on each other. All numbers are from a debug
build on a real 8,800-message mailbox.

## What it was not

Worth recording, because both were the obvious suspects and both were wrong.

**Not a command backlog.** Ten `OpenMessage` commands fired 80ms apart, each
fetching a body off the server, completed in **1.0s wall clock** at ~0.4s each.
They pipeline; they do not queue. `Client::execute` gives every command its own
thread and its own connection precisely so they cannot head-of-line block.

**Not marking read.** Ten `mark_read` round trips took **60-160ms each**, 0.9s
for all ten, which is just the spacing they were sent at.

## Reads were queueing behind the sync writer

The measurement that mattered:

```
execute sync folder for account 1 took 23863ms
query Body { message: MessageId(93115) } took 11388ms
```

A `Body` query is a single indexed row read. It took eleven seconds because
`Service::query` and the IMAP sync engine shared one `Arc<Mutex<Store>>`, and
the sync held it.

SQLite is in **WAL mode**, so a reader and a writer coexist at the database
level. The `Mutex` was the only thing serialising them -- a lock protecting
against a problem the database had already solved.

`Service` now holds a second `Store` on the same file, used only by `query`.
Opening the file twice is what WAL is for, and `Store::open` is idempotent (its
schema and migration steps are all `IF NOT EXISTS` or explicitly checked).
`busy_timeout` is set to 5s so a checkpoint cannot surface as `SQLITE_BUSY` on
an otherwise healthy read.

`a_query_is_answered_while_the_writer_holds_the_store` pins it: the test takes
the write lock and asserts a query still answers.

## The UI was waiting on the socket, on the main thread

`select_message` called `service.body()` and `service.inline_attachments()`
inline. Both are socket round trips, and `Client::query` serialises them behind
one connection -- so a reader moving faster than the daemon answers queued the
whole backlog onto the frame loop. Compounded with the section above, a single
selection could block the window for seconds.

Both now run through `background_spawn`, with the result applied in a
`this.update` that returns early if the selection has moved on.

`on_main` wraps what is left -- the queries in `refresh_messages` -- and warns
when a blocking call exceeds 16ms, because a blocking call on the main thread is
a frame that did not draw. Those measure 16-35ms and fire on sync events rather
than keystrokes, which makes them the next candidate rather than the current
problem.

## A whole-document scan in a render path

`supports_dark_mode` lowercases the entire document. At ~500us on a 100KB
newsletter that is fine once and ruinous per frame -- and `show()` plus the
toolbar's sun/moon were asking it **twice per frame**, allocating a copy of the
message each time.

The answer is now decided once, in `AppState::set_selected_html`, and read from
`selected_supports_dark`. `rendering_from` takes the decided flag;
`rendering_for` still exists for tests.

A hand-rolled allocation-free scan was tried first and measured **three times
slower** -- `str::find` is vectorised and a byte loop is not. The fix was never
to make the function cheaper, it was to stop calling it from a render path.

## The instrumentation itself

- `birdmand` times every request and logs `describe(kind)` with the elapsed ms:
  `debug` normally, `warn` past 250ms. The label includes the command and the
  ids, because "execute took 900ms" is not actionable.
- `AppState` logs each open with the in-flight count, and warns past 400ms.
- `on_main` warns when a main-thread call exceeds a frame.

`BIRDMAN_LOG=debug` for the running commentary; the warnings show at the default
`info`. The log panel reads it newest-first.
