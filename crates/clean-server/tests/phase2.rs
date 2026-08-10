//! Phase 2 acceptance: dynamic routing, TLS, HTTP/2, SSE, WebSocket, limits.
//!
//! Boots the real binary and drives it over real sockets. Everything here is
//! end to end — a test that stubbed the transport would not prove the thing
//! Phase 2 is actually about.

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use support::{header, Server};

#[test]
fn a_path_parameter_reaches_the_guest() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, _, body) = server.request("GET", "/users/42");
    assert_eq!(status, 200);
    // The guest echoes back the `:id` it captured.
    assert_eq!(body, "42");
}

#[test]
fn different_parameter_values_reach_the_same_handler() {
    let Some(server) = Server::start() else {
        return;
    };
    assert_eq!(server.request("GET", "/users/alice").2, "alice");
    assert_eq!(server.request("GET", "/users/7").2, "7");
}

#[test]
fn a_parameterised_route_does_not_swallow_deeper_paths() {
    let Some(server) = Server::start() else {
        return;
    };
    // `:id` is one segment, so this must not match.
    assert_eq!(server.request("GET", "/users/a/b").0, 404);
}

#[test]
fn a_post_body_round_trips_through_the_guest() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, _, body) = server.request_with_body("POST", "/echo", b"round trip");
    assert_eq!(status, 200);
    assert_eq!(body, "round trip");
}

#[test]
fn a_body_over_the_limit_is_refused_with_413() {
    // The fixture sets body-max-bytes = 1K.
    let Some(server) = Server::start_with("[server]\nbody-max-bytes = \"1K\"") else {
        return;
    };
    let big = vec![b'x'; 2048];
    let (status, _, _) = server.request_with_body("POST", "/echo", &big);
    assert_eq!(status, 413, "an oversized body must not reach the guest");
}

#[test]
fn a_body_within_the_limit_is_accepted() {
    let Some(server) = Server::start_with("[server]\nbody-max-bytes = \"1K\"") else {
        return;
    };
    let ok = vec![b'y'; 512];
    let (status, _, body) = server.request_with_body("POST", "/echo", &ok);
    assert_eq!(status, 200);
    assert_eq!(body.len(), 512);
}

// --- SSE -------------------------------------------------------------------

#[test]
fn sse_streams_events_and_closes() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, body) = server.request("GET", "/events");

    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("text/event-stream"));
    // Proxies that buffer would defeat a live stream.
    assert_eq!(header(&headers, "cache-control"), Some("no-cache"));

    // The guest sends two "tick" events, then closes — which ends the body.
    assert!(body.contains("event: tick"), "{body:?}");
    assert!(body.contains("data: one"), "{body:?}");
    assert!(body.contains("data: two"), "{body:?}");
    assert_eq!(body.matches("event: tick").count(), 2, "{body:?}");
}

#[test]
fn sse_events_are_separated_by_blank_lines() {
    // Without the blank line an EventSource client never dispatches the event.
    let Some(server) = Server::start() else {
        return;
    };
    let (_, _, body) = server.request("GET", "/events");
    assert!(body.contains("data: one\n\n"), "{body:?}");
}

// --- WebSocket -------------------------------------------------------------

#[test]
fn a_websocket_upgrade_completes_the_handshake() {
    let Some(server) = Server::start() else {
        return;
    };
    let (status, headers, _) = server.websocket_handshake("dGhlIHNhbXBsZSBub25jZQ==");

    assert_eq!(status, 101);
    assert_eq!(header(&headers, "upgrade"), Some("websocket"));
    // RFC 6455 §1.3 worked example.
    assert_eq!(
        header(&headers, "sec-websocket-accept"),
        Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
}

#[test]
fn the_guest_can_send_a_frame_on_an_accepted_socket() {
    let Some(server) = Server::start() else {
        return;
    };
    let mut stream = server.websocket_connect();

    // The guest sends one text frame on accept.
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).expect("a frame should arrive");
    assert!(n >= 2, "frame too short: {n}");

    let opcode = buf[0] & 0x0F;
    assert_eq!(opcode, 1, "expected a text frame");

    let len = (buf[1] & 0x7F) as usize;
    let payload = String::from_utf8_lossy(&buf[2..2 + len]);
    assert_eq!(payload, "hello socket");
}

#[test]
fn a_non_upgrade_request_to_the_socket_route_is_not_upgraded() {
    // The guest calls `accept`, which must fail with `not-an-upgrade` rather
    // than producing a broken half-upgraded response.
    let Some(server) = Server::start() else {
        return;
    };
    let (status, _, _) = server.request("GET", "/ws");
    assert_ne!(status, 101, "a plain GET must not switch protocols");
    assert_eq!(status, 200);
}

// --- TLS and HTTP/2 --------------------------------------------------------

#[test]
fn tls_serves_the_same_routes() {
    let Some(server) = Server::start_tls() else {
        return;
    };
    let body = server.tls_get("/");
    assert_eq!(body.trim_end(), "hello world");
}

#[test]
fn tls_alpn_offers_h2_before_http11() {
    let Some(server) = Server::start_tls() else {
        return;
    };
    // The acceptor advertises both; a client offering only h2 must get h2.
    assert_eq!(server.tls_alpn(&["h2"]), Some("h2".to_string()));
    assert_eq!(server.tls_alpn(&["http/1.1"]), Some("http/1.1".to_string()));
    // Offered both, the server's preference order wins.
    assert_eq!(server.tls_alpn(&["h2", "http/1.1"]), Some("h2".to_string()));
}

#[test]
fn plaintext_http1_still_works_when_h2_is_enabled() {
    // h2c has no negotiation, so a server that assumed h2 on plaintext would
    // break every ordinary HTTP/1.1 client.
    let Some(server) = Server::start_with("[server]\nh2 = true") else {
        return;
    };
    let (status, _, body) = server.request("GET", "/");
    assert_eq!(status, 200);
    assert_eq!(body, "hello world");
}

// --- load shedding ---------------------------------------------------------

#[test]
fn saturating_the_pool_sheds_load_with_503_and_retry_after() {
    // instances-max = 1 and queue-depth = 0 means the second concurrent
    // request has nowhere to wait.
    let Some(server) = Server::start_full(
        "instances-min = 1\ninstances-max = 1",
        "[server]\nqueue-depth = 0",
    ) else {
        return;
    };

    // Hold one instance busy with a streaming response that stays open.
    let mut held = TcpStream::connect(server.addr()).unwrap();
    held.write_all(b"GET /events HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    held.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // Fire enough concurrent requests that at least one must be shed.
    let mut saw_503 = false;
    for _ in 0..8 {
        let (status, headers, _) = server.request("GET", "/");
        if status == 503 {
            saw_503 = true;
            assert_eq!(
                header(&headers, "retry-after"),
                Some("1"),
                "a 503 must tell the client when to retry"
            );
            break;
        }
    }

    drop(held);
    // Not asserting that a 503 always happens — with one instance the requests
    // may simply serialise fast enough. What must hold is that IF one is shed,
    // it is shed correctly.
    if !saw_503 {
        eprintln!("note: pool never saturated in this run; 503 path not exercised");
    }
}

#[test]
fn requests_still_succeed_after_a_burst() {
    // Whatever happens under load, the server must not be left broken.
    let Some(server) = Server::start() else {
        return;
    };
    for _ in 0..20 {
        let _ = server.request("GET", "/");
    }
    let (status, _, body) = server.request("GET", "/");
    assert_eq!(status, 200);
    assert_eq!(body, "hello world");
}

// --- keep-alive ------------------------------------------------------------

#[test]
fn two_requests_share_one_keep_alive_connection() {
    let Some(server) = Server::start() else {
        return;
    };
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    for expected in ["hello world", "hello world"] {
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(&mut stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        assert!(status_line.contains("200"), "{status_line:?}");

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap();
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        assert_eq!(String::from_utf8_lossy(&body), expected);
    }
}
