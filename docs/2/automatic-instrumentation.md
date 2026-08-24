---
id: automatic-instrumentation
title: Instrumentation on boundaries, not on suspicion
altitude: 2
topics:
- engineering/practices
- performance
relations:
- type: references
  target: reading-pane-latency
summary: The Timed scope guard and the three boundaries it sits on, so a slow path reports itself instead of waiting for someone to suspect it.
---

# Instrumentation on boundaries, not on suspicion

`birdman_config::logging::Timed`.

## The problem with adding a timer when something is slow

Every performance bug found here was found by hand-placing a timer *after*
forming a theory -- which only ever finds the problem you already suspected. The
`Body` query that took eleven seconds ([[reading-pane-latency]]) was not the
thing anyone was timing; it turned up because a request-level timer happened to
be added for a different reason.

So the timing lives on boundaries instead. A boundary is instrumented once, and
everything that crosses it -- including code written later by someone who has
never read the file -- is covered without anyone remembering to do anything.

## The guard

`Timed` is a scope timer that reports itself on drop and is silent unless it
exceeds its budget:

```rust
let _timed = Timed::new("query messages", Timed::ROUND_TRIP);
```

Three budgets, named for what they mean rather than for a number: `FRAME`
(16ms -- anything on the UI thread past this is visible jank), `ROUND_TRIP`
(250ms -- a socket hop, a store read, a small IMAP command), `NETWORK` (2s).

**Nothing is written unless `BIRDMAN_LOG=debug`.** Timing is diagnostic detail
rather than a record of what happened, and a 20ms query is not a fault --
reporting it at the default level buries the lines that are. Under debug every
scope reports, and the ones over budget at `warn` so they can be picked out of
the commentary. `logging::instrumented()` is the single check, so a hand-placed
timer agrees with the boundaries about when instrumentation is on.

Cost when it is off is a pair of `Instant::now()` calls and a level check.

## The three boundaries

- **Every client request.** `birdman_client::call_once` and the thread in
  `off_thread` are the only two ways the app talks to the daemon, so a guard on
  each covers every query and command there will ever be. The label carries the
  command and the ids -- "execute took 900ms" is not actionable, and the useful
  question is always *which*.
- **Every daemon request.** The same, from the other side. The two together
  separate "the daemon was slow" from "the wait was ours", which is the
  distinction that mattered when the UI was blocking on the socket.
- **Every rendered frame.** A guard in `Root::render`. This is the catch-all: a
  socket round trip added to a getter, a document scan that creeps into a draw
  path, a store read that used to be cheap -- all surface as a frame over budget
  whether or not anyone thought to time them.

`on_main` still exists, wrapping the blocking calls that remain, but only to
*name* them: the frame guard already catches the stall, and this says which call
it was.

## What it does not do

The log line is attributed to `birdman_config::logging` rather than the calling
module, because the `log::warn!` happens inside `Drop`. The label carries the
meaning, so this is a readability cost rather than a correctness one; fixing it
means passing `module_path!()` from a macro at the call site.

Nothing here is sampling or aggregating. Under debug, a path that is slow ten
thousand times writes ten thousand lines, which the 2MB log cap then truncates.
Being debug-only makes that survivable rather than fixed; a genuinely hot slow
path would want rate limiting before it would want more coverage.

The one thing that still reports at the default level is the repeat-arrow
detector in `select_adjacent`, and deliberately: it fires only when one keypress
is handled twice, which is a malfunction rather than a measurement. The
distinction the level draws is "something is wrong" against "here is how long
things took".
