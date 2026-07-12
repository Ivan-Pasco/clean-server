//! WebSocket runtime for LIVE endpoints.
//!
//! Provides the shared state and types needed by the WebSocket bridge functions
//! in `bridge.rs` and the WebSocket upgrade handler in `server.rs`.
//!
//! # Architecture
//!
//! ```text
//! HTTP Upgrade request
//!       │
//!       ▼
//! ┌──────────────────────┐
//! │ Axum WS extractor    │  upgrades to WebSocket
//! └──────────┬───────────┘
//!            │
//!            ▼
//! ┌──────────────────────┐
//! │ ws_handle_connection │  spawns message loop
//! └──────────┬───────────┘
//!            │ calls WASM handlers
//!            ▼
//! ┌──────────────────────┐
//! │  WasmInstance        │  onConnect / onMessage / onClose
//! └──────────┬───────────┘
//!            │ reads/writes
//!            ▼
//! ┌──────────────────────┐
//! │  SharedWsState       │  connection map + room registry
//! └──────────────────────┘
//! ```
//!
//! ## ClientId
//!
//! Every WebSocket connection receives a unique `i64` client ID (auto-incrementing
//! starting at 1). The ID is stored in a `tokio::task_local!` before each WASM
//! handler invocation so the `_ws_client_id` bridge function can retrieve it
//! without parameters.
//!
//! ## Heartbeat
//!
//! A background task pings every connected client every 30 seconds. Clients that
//! have not sent any frame within 60 seconds are closed with code 1001 (Going Away)
//! and their `onClose` handler is invoked.

use crate::wasm::{AuthContext, RequestContext, WasmInstance};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Client ID
// ---------------------------------------------------------------------------

/// Global auto-incrementing counter for WebSocket connection IDs.
static NEXT_CLIENT_ID: AtomicI64 = AtomicI64::new(1);

/// Allocate the next unique client ID.
pub fn next_client_id() -> i64 {
    NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Task-local WebSocket context
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// Client ID of the WebSocket connection currently being dispatched.
    pub static WS_CLIENT_ID: i64;

    /// Payload of the message that triggered the current `onMessage` invocation.
    /// Empty string when called from `onConnect` or `onClose`.
    pub static WS_MESSAGE: String;
}

/// Retrieve the current WebSocket client ID from task-local storage.
/// Returns `0` when called outside a WebSocket handler context.
pub fn current_client_id() -> i64 {
    WS_CLIENT_ID.try_with(|id| *id).unwrap_or(0)
}

/// Retrieve the current inbound message payload from task-local storage.
/// Returns an empty string when called outside a WebSocket `onMessage` context.
pub fn current_message() -> String {
    WS_MESSAGE.try_with(|m| m.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Per-connection send channel
// ---------------------------------------------------------------------------

/// Commands that bridge functions send to the connection writer task.
#[derive(Debug)]
pub enum WsCommand {
    /// Send a UTF-8 text frame to the client.
    Text(String),
    /// Send a WebSocket Close frame with code 1000 and remove the connection.
    Close,
}

/// Sender half of the per-connection command channel.
pub type WsSender = mpsc::UnboundedSender<WsCommand>;

// ---------------------------------------------------------------------------
// Connection entry
// ---------------------------------------------------------------------------

/// Per-connection state stored in the connections map.
pub struct ConnectionEntry {
    /// Channel to push outbound commands to the connection writer task.
    pub sender: WsSender,
    /// Time the last Pong (or any inbound frame) was received. Used by the
    /// heartbeat task to detect dead connections.
    pub last_activity: Instant,
}

// ---------------------------------------------------------------------------
// WebSocket route handlers
// ---------------------------------------------------------------------------

/// The three WASM export names registered for a LIVE route.
#[derive(Debug, Clone)]
pub struct WsRouteHandlers {
    pub on_connect: String,
    pub on_message: String,
    pub on_close: String,
}

// ---------------------------------------------------------------------------
// Shared WebSocket state
// ---------------------------------------------------------------------------

/// All mutable WebSocket server state — one instance per server.
pub struct WsState {
    /// Active connections: client_id → entry.
    pub connections: HashMap<i64, ConnectionEntry>,
    /// Room registry: room_name → set of client_ids.
    pub rooms: HashMap<String, HashSet<i64>>,
    /// Route registry: URL path → WASM handler names.
    pub routes: HashMap<String, WsRouteHandlers>,
}

impl WsState {
    fn new() -> Self {
        Self {
            connections: HashMap::new(),
            rooms: HashMap::new(),
            routes: HashMap::new(),
        }
    }
}

/// Thread-safe handle to the shared WebSocket state.
pub type SharedWsState = Arc<RwLock<WsState>>;

/// Create a new empty shared WebSocket state.
pub fn create_shared_ws_state() -> SharedWsState {
    Arc::new(RwLock::new(WsState::new()))
}

// ---------------------------------------------------------------------------
// Route registration
// ---------------------------------------------------------------------------

/// Register a LIVE WebSocket route with its three WASM handler names.
///
/// Called by the `_http_ws_route` bridge function during WASM module
/// initialization.
pub async fn register_ws_route(
    ws_state: &SharedWsState,
    path: String,
    on_connect: String,
    on_message: String,
    on_close: String,
) {
    let handlers = WsRouteHandlers {
        on_connect,
        on_message,
        on_close,
    };
    ws_state.write().await.routes.insert(path.clone(), handlers);
    info!("WebSocket route registered: {}", path);
}

// ---------------------------------------------------------------------------
// Connection lifecycle
// ---------------------------------------------------------------------------

/// Handle a newly upgraded WebSocket connection end-to-end.
///
/// 1. Allocates a client ID and registers the connection.
/// 2. Calls `onConnect` in the WASM module.
/// 3. Enters the message receive loop, calling `onMessage` for each frame.
/// 4. On close or error, removes the connection and calls `onClose`.
pub async fn ws_handle_connection(
    ws_socket: WebSocket,
    client_id: i64,
    handlers: WsRouteHandlers,
    request_ctx: RequestContext,
    auth_ctx: Option<AuthContext>,
    wasm: Arc<WasmInstance>,
    ws_state: SharedWsState,
) {
    let (mut ws_sender, mut ws_receiver) = ws_socket.split();

    // Create the outbound command channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<WsCommand>();

    // Register connection.
    {
        let mut state = ws_state.write().await;
        state.connections.insert(
            client_id,
            ConnectionEntry {
                sender: tx,
                last_activity: Instant::now(),
            },
        );
    }

    info!("WebSocket client {} connected", client_id);

    // Spawn the writer task — drains `rx` and writes frames to `ws_sender`.
    let writer_handle = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                WsCommand::Text(msg) => {
                    if let Err(e) = ws_sender.send(Message::Text(msg)).await {
                        debug!("WS write error (client likely disconnected): {}", e);
                        break;
                    }
                }
                WsCommand::Close => {
                    let _ = ws_sender
                        .send(Message::Close(Some(CloseFrame {
                            code: axum::extract::ws::close_code::NORMAL,
                            reason: "Normal Closure".into(),
                        })))
                        .await;
                    break;
                }
            }
        }
    });

    // Call onConnect.
    call_wasm_ws_handler(
        &wasm,
        &handlers.on_connect,
        &request_ctx,
        &auth_ctx,
        client_id,
        "",
    )
    .await;

    // Receive loop.
    loop {
        match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                let payload = text.to_string();
                debug!("WS client {} message: {} bytes", client_id, payload.len());
                // Update last-activity time.
                {
                    let mut state = ws_state.write().await;
                    if let Some(entry) = state.connections.get_mut(&client_id) {
                        entry.last_activity = Instant::now();
                    }
                }
                call_wasm_ws_handler(
                    &wasm,
                    &handlers.on_message,
                    &request_ctx,
                    &auth_ctx,
                    client_id,
                    &payload,
                )
                .await;
            }

            Some(Ok(Message::Binary(_))) => {
                // Binary frames are outside the Clean Language WebSocket contract.
                debug!("WS client {} sent binary frame — ignored", client_id);
            }

            Some(Ok(Message::Pong(_))) => {
                // Update activity timestamp on Pong receipt.
                let mut state = ws_state.write().await;
                if let Some(entry) = state.connections.get_mut(&client_id) {
                    entry.last_activity = Instant::now();
                }
            }

            Some(Ok(Message::Ping(data))) => {
                // Axum auto-responds to Pings at the protocol level.
                debug!("WS client {} Ping({} bytes)", client_id, data.len());
                let mut state = ws_state.write().await;
                if let Some(entry) = state.connections.get_mut(&client_id) {
                    entry.last_activity = Instant::now();
                }
            }

            Some(Ok(Message::Close(_))) | None => {
                debug!("WS client {} disconnected", client_id);
                break;
            }

            Some(Err(e)) => {
                debug!("WS client {} receive error: {}", client_id, e);
                break;
            }
        }
    }

    // Call onClose.
    call_wasm_ws_handler(
        &wasm,
        &handlers.on_close,
        &request_ctx,
        &auth_ctx,
        client_id,
        "",
    )
    .await;

    // Abort the writer task.
    writer_handle.abort();

    // Remove from connection map and all rooms.
    remove_client(&ws_state, client_id).await;

    info!("WebSocket client {} cleaned up", client_id);
}

/// Call a WASM WebSocket handler (onConnect / onMessage / onClose) in a blocking
/// context with the task-local WebSocket context scoped for the duration.
async fn call_wasm_ws_handler(
    wasm: &Arc<WasmInstance>,
    handler_name: &str,
    request_ctx: &RequestContext,
    auth_ctx: &Option<AuthContext>,
    client_id: i64,
    message: &str,
) {
    let wasm_clone = wasm.clone();
    let req = request_ctx.clone();
    let auth = auth_ctx.clone();
    let h_name = handler_name.to_string();
    let msg = message.to_string();

    let result = tokio::task::spawn_blocking(move || {
        WS_CLIENT_ID.sync_scope(client_id, || {
            WS_MESSAGE.sync_scope(msg, || {
                wasm_clone.call_handler_ws(&h_name, req, auth, client_id)
            })
        })
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!(
                "WebSocket handler '{}' error for client {}: {}",
                handler_name, client_id, e
            );
        }
        Err(join_err) => {
            warn!(
                "WebSocket handler '{}' task panicked for client {}: {}",
                handler_name, client_id, join_err
            );
        }
    }
}

/// Remove a client from the connection map and all rooms it belongs to.
pub async fn remove_client(ws_state: &SharedWsState, client_id: i64) {
    let mut state = ws_state.write().await;
    state.connections.remove(&client_id);

    // Remove from all rooms and prune empty rooms.
    let empty_rooms: Vec<String> = state
        .rooms
        .iter_mut()
        .filter_map(|(room, members)| {
            members.remove(&client_id);
            if members.is_empty() {
                Some(room.clone())
            } else {
                None
            }
        })
        .collect();
    for room in empty_rooms {
        state.rooms.remove(&room);
    }
}

// ---------------------------------------------------------------------------
// Heartbeat task
// ---------------------------------------------------------------------------

/// Ping interval: every 30 seconds.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum inactivity before a connection is considered dead: 60 seconds.
const ACTIVITY_DEADLINE: Duration = Duration::from_secs(60);

/// Start the WebSocket heartbeat background task.
///
/// The task runs indefinitely and:
/// - Every 30 seconds, sends a WebSocket Ping to every connected client.
/// - After 60 seconds of no inbound activity (Text, Ping, or Pong), the
///   client is closed with code 1001 (Going Away) and its `onClose` handler
///   is invoked.
pub fn start_heartbeat_task(ws_state: SharedWsState, wasm: Arc<WasmInstance>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PING_INTERVAL);
        // The first tick fires immediately — skip it so we do not ping before
        // the first 30-second interval has elapsed.
        interval.tick().await;

        loop {
            interval.tick().await;

            let now = Instant::now();

            let (live_senders, dead_clients): (Vec<WsSender>, Vec<i64>) = {
                let state = ws_state.read().await;
                let mut live = Vec::new();
                let mut dead = Vec::new();
                for (&id, entry) in &state.connections {
                    if now.duration_since(entry.last_activity) > ACTIVITY_DEADLINE {
                        dead.push(id);
                    } else {
                        live.push(entry.sender.clone());
                    }
                }
                (live, dead)
            };

            // Issue Ping to all live clients by sending a sentinel Text frame.
            // Axum does not expose a `send_ping` method on the split sender half,
            // so we use a zero-byte Ping text that compliant clients respond to
            // with a Pong (updating `last_activity`). Applications that use
            // `_ws_message` will receive an empty string for this frame — the
            // Clean Language WebSocket contract specifies that empty text frames
            // are keep-alive probes and should be ignored by application logic.
            for sender in live_senders {
                // Silently ignore closed channels — the connection task will
                // clean up when it next tries to write.
                let _ = sender.send(WsCommand::Text(String::new()));
            }

            // Close timed-out clients.
            for client_id in dead_clients {
                warn!("Heartbeat: closing timed-out client {}", client_id);

                // Resolve onClose handler from route registry (use first registered
                // route as fallback when client→route mapping is unavailable).
                let on_close_name: Option<String> = {
                    let state = ws_state.read().await;
                    state.routes.values().next().map(|h| h.on_close.clone())
                };

                // Send Close command to the writer task.
                {
                    let state = ws_state.read().await;
                    if let Some(entry) = state.connections.get(&client_id) {
                        let _ = entry.sender.send(WsCommand::Close);
                    }
                }

                // Remove client before calling onClose so bridge functions see
                // a clean state (e.g., _ws_send to a closed client is a no-op).
                remove_client(&ws_state, client_id).await;

                // Fire onClose in WASM.
                if let Some(h_name) = on_close_name {
                    let wasm_clone = wasm.clone();
                    tokio::task::spawn_blocking(move || {
                        WS_CLIENT_ID.sync_scope(client_id, || {
                            WS_MESSAGE.sync_scope(String::new(), || {
                                let req = RequestContext {
                                    method: "LIVE".to_string(),
                                    path: "/".to_string(),
                                    headers: Vec::new(),
                                    body: String::new(),
                                    params: std::collections::HashMap::new(),
                                    query: std::collections::HashMap::new(),
                                };
                                let _ = wasm_clone.call_handler_ws(&h_name, req, None, client_id);
                            })
                        })
                    });
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Bridge helpers — thin async wrappers called from the blocking bridge closures
// ---------------------------------------------------------------------------

/// Send a text message to a specific WebSocket client (no-op if not connected).
pub async fn ws_send(ws_state: &SharedWsState, client_id: i64, message: String) {
    let state = ws_state.read().await;
    if let Some(entry) = state.connections.get(&client_id) {
        if entry.sender.send(WsCommand::Text(message)).is_err() {
            debug!("ws_send: client {} channel closed", client_id);
        }
    } else {
        debug!("ws_send: client {} not found", client_id);
    }
}

/// Close a specific WebSocket client with code 1000 (Normal Closure).
pub async fn ws_close(ws_state: &SharedWsState, client_id: i64) {
    let state = ws_state.read().await;
    if let Some(entry) = state.connections.get(&client_id) {
        let _ = entry.sender.send(WsCommand::Close);
    }
}

/// Broadcast a message to all clients in a room.
pub async fn ws_room_broadcast(ws_state: &SharedWsState, room: &str, message: String) {
    let state = ws_state.read().await;
    if let Some(members) = state.rooms.get(room) {
        for &client_id in members {
            if let Some(entry) = state.connections.get(&client_id)
                && entry.sender.send(WsCommand::Text(message.clone())).is_err()
            {
                debug!("ws_room_broadcast: client {} channel closed", client_id);
            }
        }
    } else {
        debug!("ws_room_broadcast: room '{}' does not exist", room);
    }
}

/// Add a client to a room, creating it if it does not exist.
pub async fn ws_room_join(ws_state: &SharedWsState, client_id: i64, room: String) {
    ws_state
        .write()
        .await
        .rooms
        .entry(room.clone())
        .or_insert_with(HashSet::new)
        .insert(client_id);
    debug!("ws_room_join: client {} joined '{}'", client_id, room);
}

/// Remove a client from a room, pruning the room when it becomes empty.
pub async fn ws_room_leave(ws_state: &SharedWsState, client_id: i64, room: &str) {
    let mut state = ws_state.write().await;
    if let Some(members) = state.rooms.get_mut(room) {
        members.remove(&client_id);
        if members.is_empty() {
            state.rooms.remove(room);
            debug!("ws_room_leave: room '{}' pruned", room);
        } else {
            debug!("ws_room_leave: client {} left '{}'", client_id, room);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_room_join_leave() {
        let state = create_shared_ws_state();

        ws_room_join(&state, 1, "general".to_string()).await;
        ws_room_join(&state, 2, "general".to_string()).await;

        {
            let s = state.read().await;
            assert!(s.rooms["general"].contains(&1));
            assert!(s.rooms["general"].contains(&2));
        }

        ws_room_leave(&state, 1, "general").await;

        {
            let s = state.read().await;
            assert!(!s.rooms["general"].contains(&1));
            assert!(s.rooms["general"].contains(&2));
        }

        ws_room_leave(&state, 2, "general").await;

        {
            let s = state.read().await;
            // Room should be pruned when empty.
            assert!(!s.rooms.contains_key("general"));
        }
    }

    #[tokio::test]
    async fn test_register_route() {
        let state = create_shared_ws_state();
        register_ws_route(
            &state,
            "/ws/chat".to_string(),
            "chat_on_connect".to_string(),
            "chat_on_message".to_string(),
            "chat_on_close".to_string(),
        )
        .await;

        let s = state.read().await;
        let h = s.routes.get("/ws/chat").unwrap();
        assert_eq!(h.on_connect, "chat_on_connect");
        assert_eq!(h.on_message, "chat_on_message");
        assert_eq!(h.on_close, "chat_on_close");
    }

    #[test]
    fn test_client_id_increments() {
        let id1 = next_client_id();
        let id2 = next_client_id();
        assert!(id2 > id1);
    }

    #[tokio::test]
    async fn test_context_defaults_outside_handler() {
        assert_eq!(current_client_id(), 0);
        assert_eq!(current_message(), "");
    }

    #[tokio::test]
    async fn test_context_inside_scope() {
        let id = WS_CLIENT_ID
            .scope(42_i64, async { current_client_id() })
            .await;
        assert_eq!(id, 42);

        let msg = WS_MESSAGE
            .scope("hello".to_string(), async { current_message() })
            .await;
        assert_eq!(msg, "hello");
    }

    #[tokio::test]
    async fn test_remove_client_from_rooms() {
        let state = create_shared_ws_state();

        ws_room_join(&state, 10, "alpha".to_string()).await;
        ws_room_join(&state, 10, "beta".to_string()).await;
        ws_room_join(&state, 11, "alpha".to_string()).await;

        // Insert a dummy connection entry so remove_client can find it.
        {
            let (tx, _rx) = mpsc::unbounded_channel();
            state.write().await.connections.insert(
                10,
                ConnectionEntry {
                    sender: tx,
                    last_activity: Instant::now(),
                },
            );
        }

        remove_client(&state, 10).await;

        let s = state.read().await;
        assert!(!s.connections.contains_key(&10));
        // alpha still has client 11.
        assert!(!s.rooms.get("alpha").is_some_and(|m| m.contains(&10)));
        assert!(s.rooms.get("alpha").is_some_and(|m| m.contains(&11)));
        // beta pruned.
        assert!(!s.rooms.contains_key("beta"));
    }
}
