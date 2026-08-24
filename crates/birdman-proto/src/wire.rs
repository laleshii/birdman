use serde::{Deserialize, Serialize};

/// Bump on any wire-visible change -- a new `Query` variant, a changed field,
/// a renamed tag. A rebuild leaves the old daemon running, and without a bump
/// the skew surfaces as an opaque `unknown variant` deserialization error.
pub const PROTOCOL_VERSION: u32 = 7;

use crate::{Command, Event, Query, Response};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub kind: RequestKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestKind {
    Query(Query),
    Execute {
        account: birdman_store::AccountId,
        command: Command,
    },
    Send {
        account: birdman_store::AccountId,
        message: Box<birdman_backend::OutgoingMessage>,
    },
    /// Puts a queued send back in play. False from the daemon when the row is
    /// not waiting -- mid-flight, say.
    OutboxRetry {
        id: birdman_store::OutboxId,
    },
    /// Drops a queued send. The composed text is gone; the client asked for
    /// exactly that.
    OutboxCancel {
        id: birdman_store::OutboxId,
    },
    Subscribe,
    Hello {
        version: u32,
    },
    Shutdown,
}

// Not boxed: serialized to a line either way, and a `Box` in the public wire
// vocabulary buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Reply { id: u64, result: WireResult },
    Event(Event),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireResult {
    Response(Response),
    Outcome {
        bodies_fetched: usize,
    },
    Done,
    /// A `Send` was queued, not delivered: the id names the outbox row.
    Queued {
        id: i64,
    },
    /// An outbox retry or cancel. False when the row was not waiting to
    /// begin with.
    Outbox {
        changed: bool,
    },
    VersionMismatch {
        daemon: u32,
        client: u32,
    },
    Error(String),
}

pub fn socket_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    if let Ok(explicit) = std::env::var("BIRDMAN_SOCKET") {
        return explicit.into();
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return std::path::Path::new(&runtime).join("birdman.sock");
    }
    data_dir.join("birdman.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_as_one_line() {
        let request = Request {
            id: 7,
            kind: RequestKind::Query(Query::Accounts),
        };
        let line = serde_json::to_string(&request).unwrap();
        assert!(!line.contains('\n'), "framing depends on this: {line}");

        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        assert!(matches!(back.kind, RequestKind::Query(Query::Accounts)));
    }

    #[test]
    fn an_event_frame_carries_no_id() {
        let frame = Frame::Event(Event::SyncIdle {
            account: birdman_store::AccountId(1),
        });
        let line = serde_json::to_string(&frame).unwrap();
        assert!(!line.contains("\"id\""), "an event answers nothing: {line}");
    }

    #[test]
    fn a_reply_names_the_request_it_answers() {
        let frame = Frame::Reply {
            id: 42,
            result: WireResult::Done,
        };
        let line = serde_json::to_string(&frame).unwrap();
        let back: Frame = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, Frame::Reply { id: 42, .. }));
    }

    #[test]
    fn ids_are_bare_numbers_on_the_wire() {
        let line = serde_json::to_string(&birdman_store::MessageId(12)).unwrap();
        assert_eq!(line, "12");
    }

    #[test]
    fn an_error_is_a_string_not_a_typed_enum() {
        let line = serde_json::to_string(&WireResult::Error("nope".into())).unwrap();
        let back: WireResult = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, WireResult::Error(m) if m == "nope"));
    }

    #[test]
    fn the_socket_path_is_overridable() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("BIRDMAN_SOCKET", "/tmp/birdman-test.sock") };
        assert_eq!(
            socket_path(std::path::Path::new("/unused")).to_str(),
            Some("/tmp/birdman-test.sock")
        );
        unsafe { std::env::remove_var("BIRDMAN_SOCKET") };
    }
}
