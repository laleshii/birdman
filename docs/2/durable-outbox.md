---
id: durable-outbox
title: 'Durable outgoing mail: queue, retry, and Sent filing'
altitude: 2
topics:
- architecture
- cli
relations:
- type: part_of
  target: daemon-and-clients
summary: Outgoing mail is committed to SQLite before clients get success; the daemon claims due rows atomically, retries bounded attempts, and optionally files the same message identity in Sent.
---

# Durable outgoing mail: queue, retry, and Sent filing

Sending is a daemon job rather than the lifetime of the composing client.
`Service::queue_send` serializes the message into SQLite and returns its outbox
id; a background worker claims due rows, delivers them, and records the result.
The UI therefore says "Queued", not "Sent". The CLI exposes list, retry, and
cancel operations for the durable state.

Claims are conditional state transitions. Reading a due row and later updating
it unconditionally leaves a cancellation race where cancel reports success but
the stale in-memory row is still sent. `mark_outgoing_sending` returns whether
the queued/failed row was actually claimed, and delivery proceeds only on true.

Each queued message receives its Message-ID and Date before serialization.
Retries and the independently rendered Sent copy therefore retain the same
identity instead of looking like unrelated duplicate messages. Bcc recipients
remain only in the SMTP envelope and never enter the rendered RFC822 bytes.

Automatic attempts use exponential backoff, stop after eight failures, and
bound one SMTP attempt to 60 seconds so a dead connection cannot block every
later row. A manual retry resets the attempt budget. Rows left in `sending` by a
dead daemon are returned to the queue when the store reopens.

Sent filing is controlled per account by `save_to_sent`: `auto` avoids APPEND
for Gmail because Gmail archives SMTP submissions itself, while `yes` and `no`
force the behavior. APPEND uses the server's SPECIAL-USE Sent folder and marks
the copy `\\Seen`. Delivery success and Sent filing are separate: APPEND is
currently best effort and an IMAP failure does not redeliver mail that SMTP has
already accepted.
