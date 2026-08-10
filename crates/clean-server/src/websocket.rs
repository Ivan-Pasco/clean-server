//! WebSocket upgrade and the per-socket connection task (§1.3.1, §1.5.2).
//!
//! The server owns the wire. The guest decides — during an ordinary request —
//! whether to accept the upgrade; if it does, this module completes the RFC
//! 6455 handshake, takes over the connection, and runs a task that drains the
//! socket's outbound queue.
//!
//! Fan-out across nodes is the realtime bridge's concern, not this module's.
//! What lives here is exactly what only the process holding the TCP connection
//! can do.

use base64::Engine as _;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::{Request, Response, StatusCode};
use sha1::{Digest, Sha1};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame, Role};
use tokio_tungstenite::tungstenite::Message;

use crate::listener::{empty_body, text};
use crate::sockets::{Outbound, Registry};

/// The RFC 6455 handshake GUID.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Does this request carry a valid WebSocket upgrade?
///
/// Checked before the guest runs so `websocket.accept` can answer honestly
/// rather than failing at the wire after the guest already committed.
pub fn is_upgrade_request<B>(req: &Request<B>) -> bool {
    let has_token = |name: &hyper::header::HeaderName, wanted: &str| {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(false)
    };

    req.method() == hyper::Method::GET
        && has_token(&UPGRADE, "websocket")
        && has_token(&CONNECTION, "upgrade")
        && req
            .headers()
            .get(hyper::header::SEC_WEBSOCKET_VERSION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim() == "13")
            .unwrap_or(false)
        && req.headers().contains_key(hyper::header::SEC_WEBSOCKET_KEY)
}

/// The `Sec-WebSocket-Accept` value for a given key.
pub fn accept_key(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

/// Build the 101 response and spawn the task that owns the socket.
pub fn upgrade_response(
    req: Request<()>,
    socket_id: u64,
    receiver: UnboundedReceiver<Outbound>,
    registry: Registry,
) -> Response<crate::listener::ResponseBody> {
    let Some(key) = req
        .headers()
        .get(hyper::header::SEC_WEBSOCKET_KEY)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        registry.remove(socket_id);
        return text(StatusCode::BAD_REQUEST, "400 bad request\n");
    };

    let accept = accept_key(&key);

    // hyper hands us the upgraded connection once the 101 has been written.
    let mut req = req;
    tokio::spawn(async move {
        match hyper::upgrade::on(&mut req).await {
            Ok(upgraded) => {
                let io = hyper_util::rt::TokioIo::new(upgraded);
                let socket =
                    tokio_tungstenite::WebSocketStream::from_raw_socket(io, Role::Server, None)
                        .await;
                run_socket(socket, socket_id, receiver, registry).await;
            }
            Err(e) => {
                tracing::debug!(
                    target: "clean_server::websocket",
                    error = %e,
                    "upgrade failed"
                );
                registry.remove(socket_id);
            }
        }
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "Upgrade")
        .header(hyper::header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(empty_body())
        .expect("handshake response is well-formed")
}

/// Drain the outbound queue onto the socket until either side closes.
///
/// Inbound frames are read but not yet routed to the guest: delivering them
/// needs the realtime bridge's subscription model, which is Phase 3. Reading
/// them anyway is required, not optional — it is what answers pings and
/// notices a client close.
async fn run_socket<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    socket_id: u64,
    mut receiver: UnboundedReceiver<Outbound>,
    registry: Registry,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};

    let (mut writer, mut reader) = socket.split();

    loop {
        tokio::select! {
            outbound = receiver.recv() => {
                match outbound {
                    Some(Outbound::Text(text)) => {
                        let len = text.len();
                        if writer.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                        registry.on_written(socket_id, len);
                    }
                    Some(Outbound::Binary(bytes)) => {
                        let len = bytes.len();
                        if writer.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                        registry.on_written(socket_id, len);
                    }
                    Some(Outbound::Close { code, reason }) => {
                        let frame = CloseFrame {
                            code: CloseCode::from(code),
                            reason: reason.into(),
                        };
                        let _ = writer.send(Message::Close(Some(frame))).await;
                        break;
                    }
                    // Every sender dropped: nothing more will be queued.
                    None => break,
                }
            }
            inbound = reader.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {
                        // Ping/pong is handled inside tungstenite; data frames
                        // are dropped until Phase 3 gives them somewhere to go.
                    }
                    Some(Err(e)) => {
                        tracing::debug!(
                            target: "clean_server::websocket",
                            error = %e,
                            "socket read failed"
                        );
                        break;
                    }
                }
            }
        }
    }

    let _ = writer.close().await;
    registry.remove(socket_id);
    tracing::debug!(
        target: "clean_server::websocket",
        socket = socket_id,
        "socket closed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upgrade_request() -> Request<()> {
        Request::builder()
            .method("GET")
            .uri("/ws")
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "Upgrade")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap()
    }

    #[test]
    fn a_well_formed_upgrade_is_recognised() {
        assert!(is_upgrade_request(&upgrade_request()));
    }

    #[test]
    fn a_plain_get_is_not_an_upgrade() {
        let req = Request::builder().method("GET").uri("/").body(()).unwrap();
        assert!(!is_upgrade_request(&req));
    }

    #[test]
    fn a_post_is_never_an_upgrade() {
        let req = Request::builder()
            .method("POST")
            .uri("/ws")
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "Upgrade")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        assert!(!is_upgrade_request(&req));
    }

    #[test]
    fn a_wrong_protocol_version_is_refused() {
        let req = Request::builder()
            .method("GET")
            .uri("/ws")
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "Upgrade")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "8")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        assert!(!is_upgrade_request(&req));
    }

    #[test]
    fn a_missing_key_is_refused() {
        let req = Request::builder()
            .method("GET")
            .uri("/ws")
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "Upgrade")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .body(())
            .unwrap();
        assert!(!is_upgrade_request(&req));
    }

    #[test]
    fn a_multi_token_connection_header_still_counts() {
        // Browsers behind proxies commonly send `keep-alive, Upgrade`.
        let req = Request::builder()
            .method("GET")
            .uri("/ws")
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "keep-alive, Upgrade")
            .header(hyper::header::SEC_WEBSOCKET_VERSION, "13")
            .header(hyper::header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        assert!(is_upgrade_request(&req));
    }

    #[test]
    fn the_accept_key_matches_the_rfc_6455_example() {
        // RFC 6455 §1.3 worked example.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
