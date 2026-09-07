//! WebSocket contract tests against the shared router and concurrent fake KataGo.

mod common;

use std::collections::HashSet;
use std::time::Duration;

use axum::http::{StatusCode, header};
use futures_util::{SinkExt as _, StreamExt as _};
use katago_server::config::Config;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use common::{FakeOptions, TestServer};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const WAIT: Duration = Duration::from_secs(8);

struct WsServer {
    http: TestServer,
    url: String,
    task: JoinHandle<()>,
}

impl WsServer {
    async fn start() -> Self {
        let server = Self::start_with(FakeOptions::default(), |_| {}).await;
        server.http.wait_ready().await;
        server
    }

    async fn start_with(options: FakeOptions<'_>, tweak: impl FnOnce(&mut Config)) -> Self {
        let http = TestServer::start_with(options, tweak).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/api/v1/analysis/ws", listener.local_addr().unwrap());
        let app = http.app.clone();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { http, url, task }
    }

    async fn connect(&self) -> Socket {
        timeout(WAIT, connect_async(&self.url))
            .await
            .expect("WebSocket upgrade timed out")
            .expect("WebSocket upgrade succeeds")
            .0
    }

    async fn query(&self, matches: impl Fn(&Value) -> bool) -> Value {
        timeout(WAIT, async {
            loop {
                if let Some(query) = self.http.queries().into_iter().find(&matches) {
                    return query;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "expected engine command not logged: {:?}",
                self.http.queries()
            )
        })
    }

    async fn stop(mut self) {
        timeout(WAIT, self.http.engine.shutdown())
            .await
            .expect("engine shutdown timed out");
        self.http.ws_sessions.close();
        timeout(WAIT, self.http.ws_sessions.wait())
            .await
            .expect("upgraded sessions did not finish during shutdown");
        self.task.abort();
        let result = (&mut self.task).await;
        assert!(result.is_ok() || result.unwrap_err().is_cancelled());
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn send(socket: &mut Socket, value: Value) {
    timeout(WAIT, socket.send(Message::Text(value.to_string().into())))
        .await
        .expect("WebSocket send timed out")
        .expect("WebSocket remains writable");
}

async fn analyze(socket: &mut Socket, id: &str, payload: Value) {
    send(
        socket,
        json!({"type": "analyze", "request_id": id, "payload": payload}),
    )
    .await;
}

async fn recv(socket: &mut Socket) -> Value {
    let message = timeout(WAIT, socket.next())
        .await
        .expect("WebSocket receive timed out")
        .expect("WebSocket closed before its response")
        .expect("WebSocket receive failed");
    let Message::Text(text) = message else {
        panic!("expected a JSON text envelope, got {message:?}");
    };
    let value: Value = serde_json::from_str(&text).expect("valid JSON envelope");
    assert!(
        value
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.bytes().any(|byte| byte.is_ascii_uppercase()))
    );
    value
}

async fn accepted(socket: &mut Socket, id: &str) -> String {
    let value = recv(socket).await;
    assert_eq!(value["type"], "accepted", "{value}");
    assert_eq!(value["request_id"], id, "{value}");
    let connection = value["connection_id"]
        .as_str()
        .expect("connection_id is a string");
    assert!(!connection.is_empty());
    connection.to_owned()
}

async fn result(socket: &mut Socket, id: &str, turn: u32, visits: u64) -> Value {
    let value = recv(socket).await;
    assert_eq!(value["type"], "analysis", "{value}");
    assert_eq!(value["request_id"], id, "{value}");
    assert_eq!(value["turn_number"], turn, "{value}");
    let data = &value["data"];
    assert_eq!(data["id"], id, "{value}");
    assert_eq!(data["turnNumber"], turn, "{value}");
    assert!(data.get("turn_number").is_none(), "{value}");
    assert_eq!(data["isDuringSearch"], false, "{value}");
    assert_eq!(data["rootInfo"]["visits"], visits, "{value}");
    assert!(data.get("noResults").is_none(), "{value}");
    serde_json::from_value::<katago_server::api::types::AnalysisResponse>(data.clone())
        .expect("WS data uses the main AnalysisResponse schema");
    data.clone()
}

async fn completed(socket: &mut Socket, id: &str, status: &str, expected: u32, received: u32) {
    assert_eq!(
        recv(socket).await,
        json!({"type": "completed", "request_id": id, "status": status,
               "expected": expected, "received": received})
    );
}

fn assert_error(value: &Value, id: Option<&str>, code: &str) {
    assert_eq!(value["type"], "error", "{value}");
    if let Some(id) = id {
        assert_eq!(value["request_id"], id, "{value}");
    }
    assert_eq!(value["code"], code, "{value}");
    assert!(
        !value["message"].as_str().unwrap().trim().is_empty(),
        "{value}"
    );
}

async fn failed(socket: &mut Socket, id: &str, code: &str, expected: u32, received: u32) {
    assert_error(&recv(socket).await, Some(id), code);
    let status = if code == "timeout" {
        "timeout"
    } else {
        "error"
    };
    completed(socket, id, status, expected, received).await;
}

async fn closed(socket: &mut Socket) {
    timeout(WAIT, async {
        loop {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {} // Drain frames already buffered before the close.
            }
        }
    })
    .await
    .expect("server did not close the WebSocket");
}

#[tokio::test]
async fn streams_distinct_finals_in_arrival_order_before_all_turns_finish() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    analyze(
        &mut socket,
        "review",
        json!({
            "moves": ["D4", "Q16"], "analyzeTurns": [0, 1, 2],
            "overrideSettings": {"fakeTurnOrder": [2, 0, 1], "fakeDelayMs": [0, 3000, 0],
                                 "fakeDuplicate": true, "fakeDuringSearch": true}
        }),
    )
    .await;
    // The other finals cannot exist yet: this must not be a buffered game response.
    timeout(Duration::from_secs(2), async {
        accepted(&mut socket, "review").await;
        result(&mut socket, "review", 2, 10).await;
    })
    .await
    .expect("first final was withheld until the whole query finished");
    result(&mut socket, "review", 0, 10).await;
    result(&mut socket, "review", 1, 10).await;
    completed(&mut socket, "review", "completed", 3, 3).await;
    send(&mut socket, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn accepts_pre_merge_game_envelopes_with_camel_case_analysis_payloads() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    let turns: Vec<_> = (0..=50).step_by(2).collect();
    send(
        &mut socket,
        json!({
            "type": "analyze",
            "request_id": "game-26-turns",
            "payload": {
                "moves": [
                    "Q16", "D4", "R4", "D16", "F3", "P4", "P3", "O3", "Q3", "O4",
                    "R6", "H3", "C3", "D3", "C4", "D5", "D2", "E2", "C2", "F2",
                    "C6", "O17", "R14", "Q17", "R17", "R18", "P17", "Q18", "P16", "P18",
                    "O16", "N17", "M3", "L4", "M4", "M5", "L3", "K4", "N5", "N4",
                    "L5", "M6", "K3", "J3", "G3", "G2", "J4", "K5", "J5", "K6"
                ],
                "komi": 0.5, "rules": "Japanese", "boardXSize": 19, "boardYSize": 19,
                "includeOwnership": false, "maxVisits": 30, "analyzeTurns": turns
            }
        }),
    )
    .await;
    accepted(&mut socket, "game-26-turns").await;
    for &turn in &turns {
        result(&mut socket, "game-26-turns", turn, 30).await;
    }
    // Dogyo requires this success status even when all requested turns were received.
    completed(&mut socket, "game-26-turns", "completed", 26, 26).await;
    let query = server.query(|q| q.get("moves").is_some()).await;
    assert_eq!(query["analyzeTurns"], json!(turns));
    assert_eq!(query["moves"].as_array().unwrap().len(), 50);
    assert_eq!(query["rules"], "japanese");
    assert_eq!(query["komi"], 0.5);
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn multiplexes_ids_on_one_socket_and_isolates_other_sockets_and_http() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 20;
    })
    .await;
    server.http.wait_ready().await;
    let mut first = server.connect().await;
    let mut second = server.connect().await;
    analyze(
        &mut first,
        "shared",
        json!({
            "moves": ["D4"], "maxVisits": 11, "overrideSettings": {"fakeDelayMs": 30000}
        }),
    )
    .await;
    let first_connection = accepted(&mut first, "shared").await;
    analyze(
        &mut first,
        "fast",
        json!({"moves": ["D4", "Q16"], "maxVisits": 22}),
    )
    .await;
    assert_eq!(accepted(&mut first, "fast").await, first_connection);
    analyze(&mut second, "shared", json!({"moves": [], "maxVisits": 33})).await;
    assert_ne!(accepted(&mut second, "shared").await, first_connection);

    let http_payload = json!({"requestId": "shared", "moves": ["D4"], "maxVisits": 44});
    let (http, (), ()) = tokio::join!(
        server.http.post_json("/api/v1/analysis", &http_payload),
        async {
            result(&mut first, "fast", 2, 22).await;
            completed(&mut first, "fast", "completed", 1, 1).await;
        },
        async {
            result(&mut second, "shared", 0, 33).await;
            completed(&mut second, "shared", "completed", 1, 1).await;
        }
    );
    assert_eq!(http.status(), StatusCode::OK);
    let http = common::json(http).await;
    assert_eq!(http["id"], "shared");
    assert_eq!(http["rootInfo"]["visits"], 44);

    send(
        &mut first,
        json!({"type": "cancel", "request_id": "shared"}),
    )
    .await;
    completed(&mut first, "shared", "cancelled", 1, 0).await;
    let ids: HashSet<_> = server
        .http
        .queries()
        .into_iter()
        .filter(|q| q.get("moves").is_some())
        .map(|q| uuid::Uuid::parse_str(q["id"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        4,
        "every HTTP and WS execution has its own internal UUID"
    );
    drop((first, second));
    server.stop().await;
}

#[tokio::test]
async fn duplicate_ids_and_32_active_limit_are_per_socket_and_cancel_releases_capacity() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 20;
    })
    .await;
    server.http.wait_ready().await;
    let mut socket = server.connect().await;
    for index in 0..32 {
        let id = format!("active-{index}");
        analyze(
            &mut socket,
            &id,
            json!({"overrideSettings": {"fakeDelayMs": 30000}}),
        )
        .await;
        accepted(&mut socket, &id).await;
        if index == 0 {
            analyze(&mut socket, &id, json!({})).await;
            assert_error(&recv(&mut socket).await, Some(&id), "duplicate_request_id");
        }
    }
    analyze(&mut socket, "overflow", json!({})).await;
    assert_error(
        &recv(&mut socket).await,
        Some("overflow"),
        "too_many_requests",
    );
    let mut other = server.connect().await;
    analyze(&mut other, "active-0", json!({})).await;
    accepted(&mut other, "active-0").await;
    result(&mut other, "active-0", 0, 10).await;
    completed(&mut other, "active-0", "completed", 1, 1).await;

    send(
        &mut socket,
        json!({"type": "cancel", "request_id": "active-0"}),
    )
    .await;
    completed(&mut socket, "active-0", "cancelled", 1, 0).await;
    analyze(&mut socket, "overflow", json!({})).await;
    accepted(&mut socket, "overflow").await;
    result(&mut socket, "overflow", 0, 10).await;
    completed(&mut socket, "overflow", "completed", 1, 1).await;
    for index in 1..32 {
        let id = format!("active-{index}");
        send(&mut socket, json!({"type": "cancel", "request_id": id})).await;
        completed(&mut socket, &id, "cancelled", 1, 0).await;
    }
    assert_eq!(
        server
            .http
            .queries()
            .iter()
            .filter(|q| q.get("moves").is_some())
            .count(),
        34
    );
    drop((socket, other));
    server.stop().await;
}

#[tokio::test]
async fn reuse_after_cancel_and_completion_ignores_delayed_old_results() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 10;
    })
    .await;
    server.http.wait_ready().await;
    let mut socket = server.connect().await;
    analyze(
        &mut socket,
        "reuse",
        json!({
            "moves": ["D4"], "analyzeTurns": [0, 1], "maxVisits": 11,
            "overrideSettings": {"fakeDelayMs": [0, 3000], "fakeIgnoreTerminate": true}
        }),
    )
    .await;
    let connection = accepted(&mut socket, "reuse").await;
    result(&mut socket, "reuse", 0, 11).await;
    send(
        &mut socket,
        json!({"type": "cancel", "request_id": "reuse"}),
    )
    .await;
    timeout(
        Duration::from_secs(2),
        completed(&mut socket, "reuse", "cancelled", 2, 1),
    )
    .await
    .expect("cancel waited for the engine's delayed final");

    analyze(
        &mut socket,
        "reuse",
        json!({
            "moves": ["D4", "Q16"], "maxVisits": 22,
            "overrideSettings": {"fakeDelayMs": 4000, "fakeDuplicate": 1000,
                                 "fakeIgnoreTerminate": true}
        }),
    )
    .await;
    assert_eq!(accepted(&mut socket, "reuse").await, connection);
    // The cancelled execution's turn 1 arrives before this execution's turn 2.
    result(&mut socket, "reuse", 2, 22).await;
    completed(&mut socket, "reuse", "completed", 1, 1).await;
    analyze(
        &mut socket,
        "reuse",
        json!({
            "maxVisits": 33, "overrideSettings": {"fakeDelayMs": 2500}
        }),
    )
    .await;
    assert_eq!(accepted(&mut socket, "reuse").await, connection);
    // The completed execution's delayed duplicate must not satisfy this query.
    result(&mut socket, "reuse", 0, 33).await;
    completed(&mut socket, "reuse", "completed", 1, 1).await;
    let queries: Vec<_> = server
        .http
        .queries()
        .into_iter()
        .filter(|q| q.get("moves").is_some())
        .collect();
    assert_eq!(queries.len(), 3);
    let ids: HashSet<_> = queries
        .iter()
        .map(|q| uuid::Uuid::parse_str(q["id"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    server
        .query(|q| q["action"] == "terminate" && q["terminateId"] == queries[0]["id"])
        .await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn total_timeout_keeps_partial_counts_instead_of_resetting_after_each_result() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 3;
    })
    .await;
    server.http.wait_ready().await;
    let mut socket = server.connect().await;
    analyze(
        &mut socket,
        "partial",
        json!({
            "moves": ["D4", "Q16"], "analyzeTurns": [0, 1, 2],
            "overrideSettings": {"fakeDelayMs": [0, 2000, 2000]}
        }),
    )
    .await;
    accepted(&mut socket, "partial").await;
    result(&mut socket, "partial", 0, 10).await;
    result(&mut socket, "partial", 1, 10).await;
    failed(&mut socket, "partial", "timeout", 3, 2).await;
    let query = server.query(|q| q.get("moves").is_some()).await;
    server
        .query(|q| q["action"] == "terminate" && q["terminateId"] == query["id"])
        .await;
    analyze(&mut socket, "partial", json!({})).await;
    accepted(&mut socket, "partial").await;
    result(&mut socket, "partial", 0, 10).await;
    completed(&mut socket, "partial", "completed", 1, 1).await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn startup_stdin_backpressure_is_inside_the_total_timeout_without_early_acceptance() {
    let server = WsServer::start_with(
        FakeOptions {
            env: &[("FAKE_KATAGO_STARTUP_DELAY", "4")],
        },
        |c| c.katago.move_timeout_secs = 2,
    )
    .await;
    let mut socket = server.connect().await;
    // Larger than the subprocess pipe, but smaller than the configured WS limit.
    analyze(
        &mut socket,
        "startup",
        json!({
            "overrideSettings": {"padding": "x".repeat(256 * 1024)}
        }),
    )
    .await;
    send(&mut socket, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
    timeout(
        Duration::from_secs(3),
        failed(&mut socket, "startup", "timeout", 1, 0),
    )
    .await
    .expect("startup send escaped the total timeout");
    send(&mut socket, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn engine_rejections_errors_no_results_and_invalid_finals_are_terminal_and_reusable() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    let cases = [
        (json!({"moves": ["D4", "Q16", "D4"]}), "bad_request", 1, 0),
        (
            json!({"overrideSettings": {"fakeError": "GPU on fire"}}),
            "katago_error",
            1,
            0,
        ),
        (
            json!({"moves": ["D4"], "analyzeTurns": [0, 1],
                "overrideSettings": {"fakeError": "search failed", "fakeErrorAfterTurns": 1}}),
            "katago_error",
            2,
            1,
        ),
        (
            json!({"overrideSettings": {"fakeNoResults": true}}),
            "analysis_terminated",
            1,
            0,
        ),
        (
            json!({"overrideSettings": {"fakeUnexpectedTurn": 7}}),
            "parse_error",
            1,
            0,
        ),
        (
            json!({"overrideSettings": {"fakeMalformed": true}}),
            "parse_error",
            1,
            0,
        ),
    ];
    for (payload, code, expected, received) in cases {
        analyze(&mut socket, "retry", payload).await;
        accepted(&mut socket, "retry").await;
        if received == 1 {
            result(&mut socket, "retry", 0, 10).await;
        }
        failed(&mut socket, "retry", code, expected, received).await;
    }
    analyze(
        &mut socket,
        "retry",
        json!({"overrideSettings": {"fakeWarn": 1}}),
    )
    .await;
    accepted(&mut socket, "retry").await;
    result(&mut socket, "retry", 0, 10).await;
    completed(&mut socket, "retry", "completed", 1, 1).await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn process_death_fails_other_work_and_both_sockets_survive_restart() {
    let server = WsServer::start().await;
    let mut waiting = server.connect().await;
    let mut crashing = server.connect().await;
    analyze(
        &mut waiting,
        "waiting",
        json!({"overrideSettings": {"fakeDelayMs": 30000}}),
    )
    .await;
    accepted(&mut waiting, "waiting").await;
    analyze(
        &mut crashing,
        "crash",
        json!({"overrideSettings": {"fakeCrash": 1}}),
    )
    .await;
    accepted(&mut crashing, "crash").await;
    tokio::join!(
        failed(&mut waiting, "waiting", "process_died", 1, 0),
        failed(&mut crashing, "crash", "process_died", 1, 0)
    );
    server.http.wait_ready().await;
    assert_eq!(server.http.engine.status().restarts, 1);
    for (socket, id) in [(&mut waiting, "waiting"), (&mut crashing, "crash")] {
        analyze(socket, id, json!({})).await;
        accepted(socket, id).await;
        result(socket, id, 0, 10).await;
        completed(socket, id, "completed", 1, 1).await;
    }
    drop((waiting, crashing));
    server.stop().await;
}

#[tokio::test]
async fn engine_send_failure_emits_error_and_completed_without_accepted() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.max_restart_attempts = 0;
    })
    .await;
    server.http.wait_ready().await;
    let mut socket = server.connect().await;
    let crash = server
        .http
        .post_json(
            "/api/v1/analysis",
            &json!({
                "overrideSettings": {"fakeCrash": 1}
            }),
        )
        .await;
    assert_eq!(crash.status(), StatusCode::SERVICE_UNAVAILABLE);
    server.http.wait_dead().await;
    analyze(&mut socket, "down", json!({})).await;
    failed(&mut socket, "down", "process_died", 1, 0).await;
    send(&mut socket, json!({"type": "cancel", "request_id": "down"})).await;
    assert_error(&recv(&mut socket).await, Some("down"), "not_found");
    send(&mut socket, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn malformed_json_binary_and_camel_case_envelopes_keep_socket_usable() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    for message in [
        Message::Text("{not json".into()),
        Message::Binary(br#"{"type":"ping"}"#.to_vec().into()),
    ] {
        timeout(WAIT, socket.send(message)).await.unwrap().unwrap();
        let error = recv(&mut socket).await;
        assert_error(&error, None, "bad_request");
        assert_eq!(error["request_id"], "unknown", "{error}");
    }
    for message in [
        json!(null),
        json!({"type": "unknown"}),
        json!({"type": "analyze", "requestId": "camel", "payload": {}}),
        json!({"type": "cancel", "requestId": "camel"}),
        json!({"type": "analyze", "request_id": "missing-payload"}),
        json!({"type": "analyze", "request_id": "bad-payload", "payload": null}),
        json!({"type": "analyze", "request_id": "bad-moves", "payload": {"moves": "D4"}}),
    ] {
        send(&mut socket, message).await;
        assert_error(&recv(&mut socket).await, None, "bad_request");
    }
    send(
        &mut socket,
        json!({"type": "cancel", "request_id": "unknown"}),
    )
    .await;
    assert_error(&recv(&mut socket).await, Some("unknown"), "not_found");
    timeout(
        WAIT,
        socket.send(Message::Ping(b"control-ping".to_vec().into())),
    )
    .await
    .unwrap()
    .unwrap();
    let pong = timeout(WAIT, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(pong, Message::Pong(b"control-ping".to_vec().into()));
    send(&mut socket, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
    assert!(
        server
            .http
            .queries()
            .iter()
            .all(|q| q.get("moves").is_none())
    );
    analyze(&mut socket, "valid", json!({})).await;
    accepted(&mut socket, "valid").await;
    result(&mut socket, "valid", 0, 10).await;
    completed(&mut socket, "valid", "completed", 1, 1).await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn request_ids_are_required_byte_limited_and_echoed_without_trimming() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    for id in [
        None,
        Some(json!(null)),
        Some(json!(42)),
        Some(json!("")),
        Some(json!(" \t\n ")),
        Some(json!("x".repeat(129))),
        Some(json!(format!(" {} ", "x".repeat(127)))),
        Some(json!("\u{00e9}".repeat(65))),
    ] {
        for kind in ["analyze", "cancel"] {
            let mut envelope = json!({"type": kind});
            if kind == "analyze" {
                envelope["payload"] = json!({});
            }
            if let Some(id) = &id {
                envelope["request_id"] = id.clone();
            }
            send(&mut socket, envelope).await;
            assert_error(&recv(&mut socket).await, None, "bad_request");
        }
    }
    assert!(
        server
            .http
            .queries()
            .iter()
            .all(|q| q.get("moves").is_none())
    );
    for id in [
        "  exact ID  ".to_owned(),
        "x".repeat(128),
        "\u{00e9}".repeat(64),
    ] {
        analyze(
            &mut socket,
            &id,
            json!({"requestId": "ignored".repeat(100)}),
        )
        .await;
        accepted(&mut socket, &id).await;
        result(&mut socket, &id, 0, 10).await;
        completed(&mut socket, &id, "completed", 1, 1).await;
    }
    analyze(
        &mut socket,
        " padded ",
        json!({"overrideSettings": {"fakeDelayMs": 30000}}),
    )
    .await;
    accepted(&mut socket, " padded ").await;
    send(
        &mut socket,
        json!({"type": "cancel", "request_id": "padded"}),
    )
    .await;
    assert_error(&recv(&mut socket).await, Some("padded"), "not_found");
    send(
        &mut socket,
        json!({"type": "cancel", "request_id": " padded "}),
    )
    .await;
    completed(&mut socket, " padded ", "cancelled", 1, 0).await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn ws_rejects_invalid_turns_and_payloads_without_changing_http_turn_policy() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    for payload in [
        json!({"moves": ["D4"], "analyzeTurns": []}),
        json!({"moves": ["D4"], "analyzeTurns": [0, 0]}),
        json!({"moves": ["D4"], "analyzeTurns": [2]}),
        json!({"boardXSize": 30}),
        json!({"moves": ["Z99"]}),
        json!({"moves": ["D4", ["W", "Q16"]]}),
        json!({"maxVisits": 0}),
        json!({"komi": 7.25}),
    ] {
        analyze(&mut socket, "invalid", payload).await;
        assert_error(&recv(&mut socket).await, Some("invalid"), "bad_request");
    }
    assert!(
        server
            .http
            .queries()
            .iter()
            .all(|q| q.get("moves").is_none())
    );
    for turns in [json!([]), json!([1, 0, 1])] {
        let response = server
            .http
            .post_json(
                "/api/v1/analysis/game",
                &json!({
                    "moves": ["D4"], "analyzeTurns": turns
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = common::json(response).await;
        let numbers: Vec<_> = body["turns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|turn| turn["turnNumber"].as_u64().unwrap())
            .collect();
        assert_eq!(
            numbers,
            [0, 1],
            "HTTP still expands [] and deduplicates turns"
        );
    }
    let response = server
        .http
        .post_json(
            "/api/v1/analysis",
            &json!({
                "moves": ["D4"], "analyzeTurns": []
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::json(response).await["turnNumber"], 1);
    analyze(
        &mut socket,
        "invalid",
        json!({"moves": ["D4"], "analyzeTurns": [1, 0]}),
    )
    .await;
    accepted(&mut socket, "invalid").await;
    let mut turns = HashSet::new();
    for _ in 0..2 {
        let value = recv(&mut socket).await;
        assert_eq!(value["type"], "analysis", "{value}");
        assert_eq!(value["request_id"], "invalid");
        assert!(turns.insert(value["turn_number"].as_u64().unwrap()));
    }
    assert_eq!(turns, HashSet::from([0, 1]));
    completed(&mut socket, "invalid", "completed", 2, 2).await;
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn final_only_defaults_and_all_analysis_options_and_human_fields_use_main_schema() {
    let server = WsServer::start().await;
    let mut socket = server.connect().await;
    analyze(&mut socket, "defaults", json!({"moves": ["D4", "Q16"]})).await;
    accepted(&mut socket, "defaults").await;
    let data = result(&mut socket, "defaults", 2, 10).await;
    completed(&mut socket, "defaults", "completed", 1, 1).await;
    assert!(data.get("humanPolicy").is_none());
    let query = server.query(|q| q.get("moves").is_some()).await;
    assert_eq!(query["moves"], json!([["B", "D4"], ["W", "Q16"]]));
    assert_eq!(query["komi"], 7.5);
    assert_eq!(query["rules"], "chinese");
    assert_eq!(query["boardXSize"], 19);
    assert_eq!(query["boardYSize"], 19);
    assert_eq!(query["maxVisits"], 10);

    let payload = json!({
        "requestId": "payload-id-must-not-win",
        "moves": [["w", "d4"], ["Black", "e5"], ["W", "pass"]],
        "initialStones": [["b", "c5"]], "initialPlayer": "white",
        "rules": {"ko": "SIMPLE", "scoring": "AREA"}, "komi": 0.5,
        "boardXSize": 9, "boardYSize": 13, "maxVisits": 42,
        "rootPolicyTemperature": 1.1, "rootFpuReductionMax": 0.2, "analysisPVLen": 3,
        "includeOwnership": true, "includeOwnershipStdev": true,
        "includeMovesOwnership": true, "includePolicy": true, "includePVVisits": true,
        "avoidMoves": [{"player": "b", "moves": ["c3"], "untilDepth": 2}],
        "allowMoves": [{"player": "white", "moves": ["d4"], "untilDepth": 1}],
        "overrideSettings": {"humanSLProfile": "rank_5k", "fakeHuman": true, "fakeWarn": 1},
        "priority": -3
    });
    analyze(&mut socket, "options", payload.clone()).await;
    accepted(&mut socket, "options").await;
    let data = result(&mut socket, "options", 3, 42).await;
    completed(&mut socket, "options", "completed", 1, 1).await;
    assert_eq!(data["moveInfos"][0]["moveCoord"], "C3");
    assert!(data["moveInfos"][0].get("move").is_none());
    assert_eq!(data["moveInfos"][0]["pvVisits"], json!([42]));
    assert_eq!(data["moveInfos"][0]["pvEdgeVisits"], json!([42]));
    assert_eq!(
        data["moveInfos"][0]["ownership"].as_array().unwrap().len(),
        117
    );
    assert_eq!(data["moveInfos"][0]["humanPrior"], 0.25);
    for field in ["ownership", "ownershipStdev"] {
        assert_eq!(data[field].as_array().unwrap().len(), 117);
    }
    for field in ["policy", "humanPolicy"] {
        assert_eq!(data[field].as_array().unwrap().len(), 118);
    }
    assert_eq!(data["rootInfo"]["currentPlayer"], "B");
    for (field, expected) in [
        ("humanWinrate", 0.55),
        ("humanScoreMean", 1.5),
        ("humanScoreStdev", 9.0),
        ("humanStWrError", 0.02),
        ("humanStScoreError", 0.3),
    ] {
        assert_eq!(data["rootInfo"][field], expected);
    }
    let query = server.query(|q| q["maxVisits"] == 42).await;
    assert_ne!(query["id"], "options");
    assert_ne!(query["id"], payload["requestId"]);
    assert_eq!(
        query["moves"],
        json!([["W", "D4"], ["B", "E5"], ["W", "pass"]])
    );
    assert_eq!(query["initialStones"], json!([["B", "C5"]]));
    assert_eq!(query["initialPlayer"], "W");
    assert_eq!(
        query["avoidMoves"],
        json!([{"player": "B", "moves": ["C3"], "untilDepth": 2}])
    );
    assert_eq!(
        query["allowMoves"],
        json!([{"player": "W", "moves": ["D4"], "untilDepth": 1}])
    );
    for field in [
        "rules",
        "komi",
        "boardXSize",
        "boardYSize",
        "maxVisits",
        "rootPolicyTemperature",
        "rootFpuReductionMax",
        "analysisPVLen",
        "includeOwnership",
        "includeOwnershipStdev",
        "includeMovesOwnership",
        "includePolicy",
        "includePVVisits",
        "overrideSettings",
        "priority",
    ] {
        assert_eq!(query[field], payload[field], "{field} was not forwarded");
    }
    drop(socket);
    server.stop().await;
}

#[tokio::test]
async fn upgrade_enforces_configured_origins_but_allows_originless_cli_and_wildcard() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.server.cors_allowed_origins = vec!["https://goban.app".into()];
    })
    .await;
    for origin in [
        "https://goban.app",
        "https://evil.example",
        "https://goban.app.evil",
        "null",
    ] {
        let mut request = server.url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        let response = timeout(WAIT, connect_async(request)).await.unwrap();
        if origin == "https://goban.app" {
            let (mut socket, _) = response.expect("configured browser origin may upgrade");
            send(&mut socket, json!({"type": "ping"})).await;
            assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
        } else {
            let Err(Error::Http(response)) = response else {
                panic!("unconfigured origin {origin} must get an HTTP rejection");
            };
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    let mut cli = server.connect().await;
    send(&mut cli, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut cli).await, json!({"type": "pong"}));
    drop(cli);
    server.stop().await;

    let wildcard = WsServer::start().await;
    let mut request = wildcard.url.clone().into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, "https://any.example".parse().unwrap());
    let (socket, _) = timeout(WAIT, connect_async(request))
        .await
        .unwrap()
        .unwrap();
    drop(socket);
    wildcard.stop().await;
}

#[tokio::test]
async fn message_and_frame_size_limits_close_connection_and_cancel_owned_work() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.server.max_body_bytes = 1024;
        c.katago.move_timeout_secs = 20;
    })
    .await;
    server.http.wait_ready().await;
    for fragmented in [false, true] {
        let mut socket = server.connect().await;
        let visits = if fragmented { 22 } else { 11 };
        analyze(
            &mut socket,
            "oversize",
            json!({
                "maxVisits": visits, "overrideSettings": {"fakeDelayMs": 30000}
            }),
        )
        .await;
        accepted(&mut socket, "oversize").await;
        let query = server.query(|q| q["maxVisits"] == visits).await;
        let ping = r#"{"type":"ping"}"#;
        let at_limit = format!("{ping}{}", " ".repeat(1024 - ping.len()));
        timeout(WAIT, socket.send(Message::Text(at_limit.into())))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recv(&mut socket).await, json!({"type": "pong"}));
        if fragmented {
            // Each frame fits; only the assembled message exceeds the limit.
            for (opcode, last) in [(Data::Text, false), (Data::Continue, true)] {
                let frame = Frame::message(vec![b' '; 600], OpCode::Data(opcode), last);
                timeout(WAIT, socket.send(Message::Frame(frame)))
                    .await
                    .unwrap()
                    .unwrap();
            }
        } else {
            timeout(WAIT, socket.send(Message::Text(" ".repeat(2048).into())))
                .await
                .unwrap()
                .unwrap();
        }
        closed(&mut socket).await;
        server
            .query(|q| q["action"] == "terminate" && q["terminateId"] == query["id"])
            .await;
    }
    server.stop().await;
}

#[tokio::test]
async fn a_slow_reader_is_disconnected_and_does_not_block_other_clients() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 20;
    })
    .await;
    server.http.wait_ready().await;
    let mut slow = server.connect().await;
    // Enough output to exceed TCP buffering without relying on a particular
    // queue capacity. Do not read this socket until its query is terminated.
    analyze(
        &mut slow,
        "flood",
        json!({
            "moves": vec!["pass"; 1024], "analyzeTurns": (0..=1024).collect::<Vec<_>>(),
            "boardXSize": 25, "boardYSize": 25, "includeOwnership": true,
            "includeOwnershipStdev": true, "includeMovesOwnership": true, "includePolicy": true,
            "overrideSettings": {"fakeHuman": true}
        }),
    )
    .await;
    let query = server.query(|q| q["boardXSize"] == 25).await;
    let mut other = server.connect().await;
    send(&mut other, json!({"type": "ping"})).await;
    assert_eq!(recv(&mut other).await, json!({"type": "pong"}));
    analyze(&mut other, "normal", json!({})).await;
    accepted(&mut other, "normal").await;
    result(&mut other, "normal", 0, 10).await;
    completed(&mut other, "normal", "completed", 1, 1).await;
    server
        .query(|q| q["action"] == "terminate" && q["terminateId"] == query["id"])
        .await;
    closed(&mut slow).await;
    drop((slow, other));
    server.stop().await;
}

#[tokio::test]
async fn close_frames_and_abrupt_disconnects_terminate_only_their_owned_queries() {
    let server = WsServer::start_with(FakeOptions::default(), |c| {
        c.katago.move_timeout_secs = 20;
    })
    .await;
    server.http.wait_ready().await;
    for graceful in [true, false] {
        let mut owner = server.connect().await;
        let mut peer = server.connect().await;
        let owner_visits = if graceful { 11 } else { 33 };
        let peer_visits = owner_visits + 1;
        for (socket, visits) in [(&mut owner, owner_visits), (&mut peer, peer_visits)] {
            analyze(
                socket,
                "same-id",
                json!({
                    "maxVisits": visits, "overrideSettings": {"fakeDelayMs": 30000}
                }),
            )
            .await;
            accepted(socket, "same-id").await;
        }
        let cancelled_query = server.query(|q| q["maxVisits"] == owner_visits).await;
        let foreign = server.query(|q| q["maxVisits"] == peer_visits).await;
        if graceful {
            timeout(WAIT, owner.send(Message::Close(None)))
                .await
                .unwrap()
                .unwrap();
            closed(&mut owner).await;
        }
        drop(owner);
        server
            .query(|q| q["action"] == "terminate" && q["terminateId"] == cancelled_query["id"])
            .await;
        assert!(
            !server
                .http
                .queries()
                .iter()
                .any(|q| { q["action"] == "terminate" && q["terminateId"] == foreign["id"] })
        );
        send(&mut peer, json!({"type": "ping"})).await;
        assert_eq!(recv(&mut peer).await, json!({"type": "pong"}));
        send(
            &mut peer,
            json!({"type": "cancel", "request_id": "same-id"}),
        )
        .await;
        completed(&mut peer, "same-id", "cancelled", 1, 0).await;
        drop(peer);
    }
    server.stop().await;
}

#[tokio::test]
async fn malformed_upgrade_is_an_http_problem() {
    let server = TestServer::start().await;
    let problem = common::assert_problem(
        server.get("/api/v1/analysis/ws").await,
        StatusCode::BAD_REQUEST,
        "invalid-request",
    )
    .await;
    assert_eq!(problem["instance"], "/api/v1/analysis/ws");
    server.engine.shutdown().await;
}

#[tokio::test]
async fn shutdown_terminates_all_socket_jobs_and_new_work_gets_no_acceptance() {
    let server = WsServer::start().await;
    let mut first = server.connect().await;
    let mut second = server.connect().await;
    for socket in [&mut first, &mut second] {
        analyze(
            socket,
            "shutdown",
            json!({"overrideSettings": {"fakeDelayMs": 30000}}),
        )
        .await;
        accepted(socket, "shutdown").await;
    }
    timeout(WAIT, server.http.engine.shutdown())
        .await
        .expect("shutdown must cancel delays");
    assert!(!server.http.engine.status().alive);
    assert!(
        server
            .http
            .queries()
            .iter()
            .any(|q| q["action"] == "terminate_all")
    );
    failed(&mut first, "shutdown", "shutting_down", 1, 0).await;
    failed(&mut second, "shutdown", "shutting_down", 1, 0).await;
    closed(&mut first).await;
    closed(&mut second).await;
    let mut after = server.connect().await;
    closed(&mut after).await;
    drop((first, second, after));
    server.stop().await;
}
