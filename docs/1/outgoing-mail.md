---
id: outgoing-mail
title: 'birdman-smtp: the SMTP sender connector'
altitude: 1
topics:
- sending
- connectors
relations:
- type: refines
  target: birdman-overview
- type: depends_on
  target: mime-parsing
- type: references
  target: connector-boundary
summary: SMTP transport as a MailSender implementation, the crate-name collision with upstream mail-send, and why the draft builders moved to birdman-backend.
---

# birdman-smtp: the SMTP sender connector

`crates/connectors/birdman-smtp` puts outgoing mail on the wire. It is a
**connector**, one of two roles in the boundary — see [[connector-boundary]].

```rust
impl MailSender for SmtpSender {
    fn name(&self) -> &'static str { "smtp" }
    fn send(&self, message: OutgoingMessage) -> SendFuture { ... }
}
```

The trait impl is thin: it wraps the existing `send(&SmtpConfig,
OutgoingMessage)` free function, which is still public and is what the GreenMail
integration test drives directly.

## What moved out, and why

`OutgoingMessage`, `Recipient`, `ComposeDraft`, `reply_draft` and
`forward_draft` used to live here. They now live in `birdman-backend`.

The reason is a dependency direction: once sending went behind `MailSender`, the
contract crate needed the message type in its own signature. A contract cannot
import from one of its implementations. Nothing in those types was
SMTP-specific anyway — a JMAP submission or a Gmail API send needs exactly the
same fields.

What stayed: `SmtpConfig`, `SendError`, the `mail-builder` MIME construction,
and the transport itself.

## The crate-name collision

This crate depends on the crates.io crate that *used* to share its name:

```toml
smtp = { package = "mail-send", version = "0.6" }
```

The workspace crate was renamed `mail-send` -> `birdman-smtp` when it moved under
`connectors/`, which incidentally removed the confusion — but the `smtp` alias
stays, since renaming the import would churn every call site for no gain.

## One connection per send

No pooling. Sending is infrequent and latency-tolerant, unlike IMAP sync, so a
fresh connection per call is the right trade and keeps the connector stateless
between sends.

## Credentials are still resolved for it, not by it

`SmtpConfig` carries a `password: String`, handed over at startup and held for
the life of the app. This is the field the authentication-adapter work removes —
see [[auth-adapter-design]]. Until then, a sender is constructed with an
already-resolved secret and a changed password needs a restart.
