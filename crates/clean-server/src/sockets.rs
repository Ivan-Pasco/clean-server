//! Live-connection state: WebSocket sockets and SSE streams.
//!
//! The server owns the wire (§1.5.2, realtime-bridge §12.4): the guest — and
//! later the realtime bridge, through `clean:realtime/sockets` — hands over
//! messages, and this module decides what actually reaches the socket.
//!
//! ## Backpressure
//!
//! Each socket has an outbound queue bounded by `[server] socket-queue-max`
//! (default 1 MB or 64 messages, whichever binds first). When the queue is
//! full the send is refused with `socket-slow` rather than blocking, so one
//! slow client cannot stall a guest handler holding a pooled instance.
//!
//! The realtime spec's `drop-oldest` policy belongs to the *bridge*, which
//! applies it after seeing `socket-slow`. The host's job is only to report the
//! condition honestly — silently dropping here would hide it from the policy
//! that is supposed to decide.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// Default message-count bound, per §1.5.2.
const DEFAULT_QUEUE_MESSAGES: usize = 64;

/// Why a send or close failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketError {
    NotAnUpgrade,
    SocketSlow,
    Closed,
}

impl SocketError {
    /// The WIT enum discriminant name.
    pub fn as_wit(self) -> &'static str {
        match self {
            Self::NotAnUpgrade => "not-an-upgrade",
            Self::SocketSlow => "socket-slow",
            Self::Closed => "closed",
        }
    }
}

/// One outbound WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

impl Outbound {
    fn byte_len(&self) -> usize {
        match self {
            Self::Text(s) => s.len(),
            Self::Binary(b) => b.len(),
            Self::Close { reason, .. } => reason.len(),
        }
    }
}

/// One SSE event, already framed as it will appear on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
    pub id: String,
    pub retry_millis: Option<u32>,
}

impl SseEvent {
    /// Render to the `text/event-stream` framing.
    ///
    /// Multi-line data becomes one `data:` line per line, which is what the
    /// EventSource spec requires — a raw newline would end the event early.
    pub fn frame(&self) -> String {
        let mut out = String::new();
        if !self.id.is_empty() {
            out.push_str(&format!("id: {}\n", sanitize(&self.id)));
        }
        if !self.event_type.is_empty() {
            out.push_str(&format!("event: {}\n", sanitize(&self.event_type)));
        }
        if let Some(retry) = self.retry_millis {
            out.push_str(&format!("retry: {retry}\n"));
        }
        if self.data.is_empty() {
            out.push_str("data:\n");
        } else {
            for line in self.data.split('\n') {
                out.push_str(&format!("data: {}\n", line.trim_end_matches('\r')));
            }
        }
        out.push('\n');
        out
    }
}

/// Keep a value on one line so it cannot inject extra SSE fields.
///
/// Newlines become spaces rather than being deleted: deleting them would glue
/// the following text onto the value (`"1\ndata: x"` -> `"1data: x"`), which is
/// a silent corruption rather than a visible one.
fn sanitize(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

/// A queue shared between the guest (producer) and the connection task
/// (consumer).
struct Queue {
    sender: mpsc::UnboundedSender<Outbound>,
    /// Bytes queued but not yet written. Tracked alongside the channel because
    /// an unbounded channel cannot report it, and the byte bound is what
    /// `socket-queue-max` actually specifies.
    queued_bytes: usize,
    queued_messages: usize,
    max_bytes: usize,
    max_messages: usize,
    closed: bool,
}

impl Queue {
    fn try_push(&mut self, message: Outbound) -> Result<(), SocketError> {
        if self.closed {
            return Err(SocketError::Closed);
        }

        // A close frame is control traffic: it must get through even when the
        // queue is full, or a slow client could never be disconnected.
        let is_close = matches!(message, Outbound::Close { .. });
        let len = message.byte_len();

        if !is_close
            && (self.queued_bytes + len > self.max_bytes
                || self.queued_messages + 1 > self.max_messages)
        {
            return Err(SocketError::SocketSlow);
        }

        self.sender.send(message).map_err(|_| {
            // The receiving connection task is gone.
            self.closed = true;
            SocketError::Closed
        })?;

        self.queued_bytes += len;
        self.queued_messages += 1;
        if is_close {
            self.closed = true;
        }
        Ok(())
    }

    /// Called by the connection task once bytes have left the queue.
    fn on_written(&mut self, bytes: usize) {
        self.queued_bytes = self.queued_bytes.saturating_sub(bytes);
        self.queued_messages = self.queued_messages.saturating_sub(1);
    }
}

/// All live connections owned by this server.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
    next_id: Arc<AtomicU64>,
    max_bytes: usize,
    max_messages: usize,
}

#[derive(Default)]
struct Inner {
    sockets: HashMap<u64, Queue>,
    streams: HashMap<u64, Queue>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("live", &self.live_count())
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// `socket_queue_max` comes from `[server] socket-queue-max`.
    pub fn new(socket_queue_max: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            next_id: Arc::new(AtomicU64::new(1)),
            max_bytes: socket_queue_max as usize,
            max_messages: DEFAULT_QUEUE_MESSAGES,
        }
    }

    fn register(&self, into_sockets: bool) -> (u64, mpsc::UnboundedReceiver<Outbound>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let queue = Queue {
            sender,
            queued_bytes: 0,
            queued_messages: 0,
            max_bytes: self.max_bytes,
            max_messages: self.max_messages,
            closed: false,
        };

        let mut inner = self.inner.lock().unwrap();
        if into_sockets {
            inner.sockets.insert(id, queue);
        } else {
            inner.streams.insert(id, queue);
        }
        (id, receiver)
    }

    /// Register a WebSocket, returning its id and the receiver the connection
    /// task drains.
    pub fn register_socket(&self) -> (u64, mpsc::UnboundedReceiver<Outbound>) {
        self.register(true)
    }

    /// Register an SSE stream.
    pub fn register_stream(&self) -> (u64, mpsc::UnboundedReceiver<Outbound>) {
        self.register(false)
    }

    pub fn send_socket(&self, id: u64, message: Outbound) -> Result<(), SocketError> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .sockets
            .get_mut(&id)
            .ok_or(SocketError::Closed)?
            .try_push(message)
    }

    pub fn send_stream(&self, id: u64, message: Outbound) -> Result<(), SocketError> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .streams
            .get_mut(&id)
            .ok_or(SocketError::Closed)?
            .try_push(message)
    }

    pub fn queued_bytes(&self, id: u64) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner
            .sockets
            .get(&id)
            .or_else(|| inner.streams.get(&id))
            .map(|q| q.queued_bytes as u64)
            .unwrap_or(0)
    }

    /// Report that `bytes` left a queue, freeing capacity.
    pub fn on_written(&self, id: u64, bytes: usize) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(q) = inner.sockets.get_mut(&id) {
            q.on_written(bytes);
        } else if let Some(q) = inner.streams.get_mut(&id) {
            q.on_written(bytes);
        }
    }

    /// Drop a connection's state once its task has finished.
    pub fn remove(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.sockets.remove(&id);
        inner.streams.remove(&id);
    }

    /// Whether a connection is still open.
    ///
    /// Used by the `clean:realtime/sockets` envelope in Phase 3, which must
    /// answer "is this subscriber still there" before fanning out to it. Tested
    /// now so the semantics cannot drift before that lands.
    #[allow(dead_code)]
    pub fn is_live(&self, id: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.sockets.contains_key(&id) || inner.streams.contains_key(&id)
    }

    /// Live socket count, for diagnostics.
    pub fn live_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.sockets.len() + inner.streams.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        Registry::new(1024)
    }

    #[test]
    fn a_registered_socket_accepts_messages() {
        let r = registry();
        let (id, mut rx) = r.register_socket();
        r.send_socket(id, Outbound::Text("hi".into())).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Outbound::Text("hi".into()));
    }

    #[test]
    fn sending_to_an_unknown_socket_reports_closed() {
        let r = registry();
        assert_eq!(
            r.send_socket(999, Outbound::Text("x".into())),
            Err(SocketError::Closed)
        );
    }

    #[test]
    fn a_full_byte_budget_reports_socket_slow() {
        let r = Registry::new(16);
        let (id, _rx) = r.register_socket();

        r.send_socket(id, Outbound::Text("0123456789".into()))
            .unwrap();
        // 10 queued of 16; another 10 would exceed the budget.
        assert_eq!(
            r.send_socket(id, Outbound::Text("0123456789".into())),
            Err(SocketError::SocketSlow)
        );
    }

    #[test]
    fn a_full_message_budget_reports_socket_slow() {
        // Generous byte budget, so the message count is what binds.
        let r = Registry::new(10 * 1024 * 1024);
        let (id, _rx) = r.register_socket();
        for _ in 0..DEFAULT_QUEUE_MESSAGES {
            r.send_socket(id, Outbound::Text("x".into())).unwrap();
        }
        assert_eq!(
            r.send_socket(id, Outbound::Text("x".into())),
            Err(SocketError::SocketSlow)
        );
    }

    #[test]
    fn draining_the_queue_restores_capacity() {
        let r = Registry::new(16);
        let (id, _rx) = r.register_socket();
        r.send_socket(id, Outbound::Text("0123456789".into()))
            .unwrap();
        assert_eq!(r.queued_bytes(id), 10);

        r.on_written(id, 10);
        assert_eq!(r.queued_bytes(id), 0);
        // Capacity is back.
        r.send_socket(id, Outbound::Text("0123456789".into()))
            .unwrap();
    }

    #[test]
    fn a_close_frame_is_queued_even_when_the_socket_is_saturated() {
        // Otherwise a slow client could never be disconnected — the close
        // would be refused by the very condition it is meant to resolve.
        let r = Registry::new(8);
        let (id, mut rx) = r.register_socket();
        r.send_socket(id, Outbound::Text("12345678".into()))
            .unwrap();
        assert_eq!(
            r.send_socket(id, Outbound::Text("more".into())),
            Err(SocketError::SocketSlow)
        );

        r.send_socket(
            id,
            Outbound::Close {
                code: 1000,
                reason: String::new(),
            },
        )
        .unwrap();

        let _ = rx.try_recv();
        assert_eq!(
            rx.try_recv().unwrap(),
            Outbound::Close {
                code: 1000,
                reason: String::new()
            }
        );
    }

    #[test]
    fn sending_after_close_reports_closed() {
        let r = registry();
        let (id, _rx) = r.register_socket();
        r.send_socket(
            id,
            Outbound::Close {
                code: 1000,
                reason: String::new(),
            },
        )
        .unwrap();
        assert_eq!(
            r.send_socket(id, Outbound::Text("late".into())),
            Err(SocketError::Closed)
        );
    }

    #[test]
    fn removing_a_socket_makes_it_unknown() {
        let r = registry();
        let (id, _rx) = r.register_socket();
        assert!(r.is_live(id));
        r.remove(id);
        assert!(!r.is_live(id));
    }

    #[test]
    fn sockets_and_streams_get_distinct_ids() {
        let r = registry();
        let (a, _x) = r.register_socket();
        let (b, _y) = r.register_stream();
        assert_ne!(a, b);
        assert_eq!(r.live_count(), 2);
    }

    // --- SSE framing -------------------------------------------------------

    #[test]
    fn a_minimal_event_frames_as_data_plus_blank_line() {
        let e = SseEvent {
            event_type: String::new(),
            data: "hello".into(),
            id: String::new(),
            retry_millis: None,
        };
        assert_eq!(e.frame(), "data: hello\n\n");
    }

    #[test]
    fn a_named_event_with_an_id_frames_all_fields() {
        let e = SseEvent {
            event_type: "tick".into(),
            data: "1".into(),
            id: "7".into(),
            retry_millis: Some(3000),
        };
        assert_eq!(e.frame(), "id: 7\nevent: tick\nretry: 3000\ndata: 1\n\n");
    }

    #[test]
    fn multiline_data_becomes_one_data_line_per_line() {
        // A raw newline inside `data:` would terminate the event early.
        let e = SseEvent {
            event_type: String::new(),
            data: "one\ntwo".into(),
            id: String::new(),
            retry_millis: None,
        };
        assert_eq!(e.frame(), "data: one\ndata: two\n\n");
    }

    #[test]
    fn newlines_in_id_or_event_cannot_inject_extra_fields() {
        let e = SseEvent {
            event_type: "a\nevent: injected".into(),
            data: "x".into(),
            id: "1\ndata: nope".into(),
            retry_millis: None,
        };
        let framed = e.frame();
        // Exactly one of each field, and the injected text stays inside the
        // value it came from rather than becoming its own field.
        assert_eq!(
            framed.lines().filter(|l| l.starts_with("event:")).count(),
            1,
            "{framed:?}"
        );
        assert_eq!(
            framed.lines().filter(|l| l.starts_with("data:")).count(),
            1,
            "{framed:?}"
        );
        assert_eq!(
            framed.lines().filter(|l| l.starts_with("id:")).count(),
            1,
            "{framed:?}"
        );
        assert!(framed.contains("id: 1 data: nope"), "{framed:?}");
    }

    #[test]
    fn empty_data_still_emits_a_data_field() {
        let e = SseEvent {
            event_type: String::new(),
            data: String::new(),
            id: String::new(),
            retry_millis: None,
        };
        assert_eq!(e.frame(), "data:\n\n");
    }
}
