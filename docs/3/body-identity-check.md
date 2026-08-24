---
id: body-identity-check
title: 'A uid is meaningless without its mailbox: the wrong-body bug'
altitude: 3
topics:
- sync
- connectors
relations:
- type: part_of
  target: imap-sync-engine
summary: How a body fetch stored one message's mail under another's id, why it was silent and permanent, and the Message-ID check that makes it impossible.
---

# A uid is meaningless without its mailbox: the wrong-body bug

The worst bug this codebase has had. Worth recording in full, because the
shape of it -- silent, permanent, and invisible from the layer that showed the
symptom -- generalises.

## The symptom

The reading pane showed one sender's header above a completely different
sender's mail: a product-announcement newsletter's header above a parcel
delivery notice, in another language, from an unrelated sender.

## The cause

`fetch_message_body(session, store, message_id, uid)` issues
`UID FETCH <uid> (BODY.PEEK[])` and stores the result under `message_id`.

**A uid only means anything relative to the currently selected mailbox**, and
this function cannot see which mailbox that is. It trusts its caller.

When that trust was misplaced -- and two accounts both having a mailbox called
`INBOX` is all it takes -- uid 4102 fetched against the wrong mailbox returned
a *different real message*, which was then stored under this message's id.

The trigger here was a separate bug: `Command::ListFolders` resolved its
account as "the store's first account" rather than the connector's own, so one
account's folders were written under another's id, and a later body fetch
resolved a message to the wrong account's connection.

## Why it was so bad

**Silent.** No error. The server answered a perfectly valid request.

**Permanent.** From the store's point of view the body was present and
correct, so nothing ever re-fetched it. `body_fetched` was set. The corruption
outlived the bug that caused it.

**Misattributed.** Every instinct says "the UI is showing the wrong thing", and
the reading pane has a genuine history of staleness bugs. Three separate UI
mechanisms were investigated and cleared -- the show/hide dedupe, the async
staleness guards, the prepared-document cache -- before anyone asked the
database. One `sqlite3` query settled it immediately.

**The near-miss was already visible.** Logs had been carrying
`body backfill skipped a message: server returned no data (wrong mailbox
selected...)` for a while. Those were the *lucky* cases, where the uid did not
exist in the wrong mailbox. Nobody read them as evidence of the unlucky ones.

## The fix

Check the fetched message against the envelope already stored:

```rust
if expected_message_id != fetched_message_id {
    log::error!("refusing body ... the wrong mailbox was selected");
    return Err(CoreError::MessageMissing);
}
```

One string comparison, and crucially it **does not depend on every caller
having selected correctly**. That is the property worth copying: the check sits
at the point of damage, not at the many points where the mistake can be made.

Messages with no `Message-ID` are exempt -- malformed but real, and never
showing their body is worse than the risk.

## Detecting the damage afterwards

Bodies cannot be verified retroactively against a `Message-ID`, because the
stored body is the parsed *body part* and carries no headers.

What worked: **`messages.preview`**. It comes from the envelope pass, which is
per-folder and correctly scoped, so it is independent evidence of what the
message says. Comparing the preview's opening words against the stored body
found 81 corrupted bodies out of 6,770 -- and the distribution confirmed the
cause, at 5.2% for the account that had been misrouted versus 0.4% for the
other.

Repair is cheap because bodies are a *cache*: delete the row, clear
`body_fetched`, blank the FTS snippet (which was generated from the wrong body
and was making search return the wrong text), and let it re-download verified.

## The generalisable lesson

When one layer's correctness depends on another layer's invisible state --
here, "the right mailbox is selected" -- a check at the boundary is worth more
than care at every call site. And when a display looks wrong, verify what is
*stored* before investigating what is *drawn*.

## The second episode was residue, not a regression

A message showing "Tessa left a review for Monti's World" over a Postmark
invoice -- with the invoice's PDF attached, so both the body and the
attachments came from the wrong message.

The instinct is that the guard failed. It had not. What settled it was the
timestamp: `message_bodies.cached_at` for that row was **before** the guard
landed, and grouping every mismatched body by the hour it was cached showed the
whole population sitting on 08-20 and the morning of 08-21, with nothing after.
The four apparent exceptions were all false positives -- previews holding
`&zwnj;` or a raw URL that a tag-stripper renders differently from the body.

So the guard works, and the first repair simply missed rows.

## Detecting it: compare the body against the preview

`preview` is built during envelope sync from a truncated `BODY.PEEK[TEXT]`, so
it belongs to the *right* message by construction -- which makes it the
reference the body can be checked against long after the fact. Normalise both
(strip tags, unescape entities, collapse whitespace) and ask whether the
preview's first 60 characters appear in the body.

It over-reports. Of 270 flagged, most were formatting drift rather than
corruption. That is the right direction to be wrong in for a *repair*: clearing
a body costs one re-download, and leaving one costs a message that shows someone
else's mail forever. So all 266 pre-guard suspects were cleared -- body,
attachment rows, and `body_fetched` -- and re-fetched through the guarded path
rather than sorted into real and imagined.

Attachments have to go with the body. They were parsed from the same wrong
message, which is how an invoice PDF ended up on a review notification.
