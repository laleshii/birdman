---
id: dependency-log-ceiling
title: Dependencies are capped at info so their debug output cannot drown ours
altitude: 3
topics:
- engineering/practices
relations:
- type: refines
  target: gpui-application
---

# Dependencies are capped at info so their debug output cannot drown ours

`crates/birdman-config/src/logging.rs`.

## What went wrong

`html5ever` logs every token it parses. Sanitising one email body produced 9,776
log lines in 0.59s, each a synchronous write on the render path. At `debug` —
which is the **default for a dev build** — that is what every message you open
costs, and 2MB of it trips `MAX_LOG_BYTES` within minutes.

The second cost is worse than the speed: the log is truncated, not rotated, so
the parser narrating its own work silently erased the diagnostic history of
whatever was being investigated. That happened twice during one debugging
session before it was noticed.

## The rule

`FileLogger::enabled` judges a record against the configured level when its
target starts with `birdman`, and against a ceiling of `info` otherwise. A
dependency can still report a problem; it cannot narrate. `set_max_level` gets
the looser of the two, or the `log` macros would drop records before `enabled`
ever saw them.

Measured: a debug run went from 12,054 lines to 87, keeping all 62 of our own.

## The escape hatch matters

`BIRDMAN_LOG_DEPS` lifts the cap. Some genuinely useful facts are only visible
through dependencies — the TLS handshake through `rustls`, the OAuth2 refresh
through `ureq` — and both were needed to work out where launch time went. The
cap defaults to `min(workspace_level, info)`, so asking for a quieter workspace
quietens dependencies too.

## Related trap

Both the daemon and the client append to the same `birdman.log`, with no locking
between processes, so lines from concurrent writers can interleave mid-line and
a session boundary is not a file boundary. Segment by the process's startup lines
before counting anything in it.
