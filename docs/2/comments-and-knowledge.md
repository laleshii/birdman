---
id: comments-and-knowledge
title: 'Where knowledge lives: engrym, not code comments'
altitude: 2
topics:
- engineering/practices
relations:
- type: references
  target: testing-approach
summary: 'The repo''s rule for comments: code speaks for itself, only non-recoverable facts stay inline, and everything else belongs in this knowledge base.'
---

# Where knowledge lives: engrym, not code comments

This repo keeps its reasoning in engrym and its code close to comment-free. That
is a deliberate reversal: the codebase was once about 25% comments by line
(5,708 of 23,166), much of it prose that duplicated what these documents already
said, in less detail and with no way to find it unless you happened to open that
file.

## The rule

A comment earns its place only when it records something a competent reader
**cannot recover from the code in front of them**, and would break if they did
not know it:

- A workaround for a specific upstream bug, platform quirk or library
  constraint — name the thing being worked around (`wry#1763`, `CVE-2026-26312`,
  "GDK panics rather than erroring").
- A non-obvious ordering, locking or lifetime requirement that looks safe to
  change but is not (`// Registry only. Never held across an await.`).
- A deliberate deviation from the obvious implementation, where the obvious one
  was tried and failed (`// Not boxed: serialized to a line either way.`).
- A protocol or spec citation that justifies an otherwise odd branch (RFC 4315,
  RFC 6154, RFC 7636 appendix B).

Everything else — architecture, design decisions, subsystem overviews,
cross-cutting flows, historical context, "why we do it this way" — belongs in a
document here. Prose in a source file is read by whoever opens that file; the
same prose in engrym is retrievable by anyone asking the question it answers.

## What this looks like in practice

Doc comments that only restate a signature are removed. A module doc that
narrates a subsystem is replaced by a pointer, or by nothing when a document
already covers it. A long "why" paragraph is compressed to the one sentence that
stops someone undoing it, with the full account moved here.

Comments that survive are usually short and imperative, and often phrased as a
prohibition, because that is what a future reader needs:

```rust
// Do not add BODYSTRUCTURE to the FETCH above.
// **Taller, never shorter**: a view briefly a pixel short shows a line of
// whatever is underneath.
// `color` is matched **exactly**, never by prefix.
```

## Why comments drift and documents do not

Duplicated prose rots in a specific way this repo has now seen several times: an
edit adds a new doc block above a function without removing the old one, and the
two stack. The function that used to own the first doc silently loses it, and
the stale text migrates onto whatever item happens to sit below.

A sweep of the workspace found this in `birdman-mime`, `birdman-client`,
`birdman-imap::sync` (four times, one of them describing a single append where the
code loops), `birdman-store`, `birdman-ui::state` and `birdman-ui::root`. Every instance
was invisible to the compiler and to the tests. Less prose in the code means
fewer places for that to happen.

## Checking for it

Two things worth running after a large documentation change:

- A scan for a doc line ending in `.` followed immediately by another doc line
  opening a fresh summary sentence. That is the signature of two stacked blocks.
- `engrym lint --strict`, then `engrym index`.

## Scope

This is a user-wide preference, not only a rule for this repo — it is recorded
in the global agent instructions. In a repo with no engrym knowledge base, the
README or an existing docs directory takes engrym's place.
