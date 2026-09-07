//! Caller-owned streaming and cancellation against the fake KataGo.

mod common;

use std::time::Duration;

use katago_server::api::validate::build_query;
use katago_server::engine::Query;
use katago_server::error::EngineError;
use serde_json::{Value, json};
use tokio::time::timeout;

use common::{FakeOptions, TestServer};

fn query(server: &TestServer, body: Value) -> Query {
    build_query(
        &serde_json::from_value(body).unwrap(),
        server.engine.config(),
        false,
    )
    .unwrap()
    .query
}

#[tokio::test]
async fn stream_preserves_arrival_order_and_internal_id_while_analyze_sorts() {
    let server = TestServer::start().await;
    server.wait_ready().await;
    let body = json!({"moves": ["D4", "Q16"], "requestId": "client-id"});
    let mut streamed = query(&server, body.clone());
    streamed.analyze_turns = Some(vec![2, 2, 0, 1]);
    let mut stream = server.engine.analyze_stream(&streamed).await.unwrap();
    let mut turns = Vec::new();
    while let Some(result) = stream.next().await {
        let result = result.unwrap();
        assert_eq!(result.id, streamed.id);
        assert_ne!(result.id, "client-id");
        turns.push(result.turn_number);
    }
    assert_eq!(turns, [2, 0, 1]);

    let mut collected = query(&server, body);
    collected.analyze_turns = Some(vec![2, 2, 0, 1]);
    let results = server.engine.analyze(&collected).await.unwrap();
    assert_eq!(
        results.iter().map(|r| r.turn_number).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    let sent: Vec<_> = server
        .queries()
        .into_iter()
        .filter(|q| q.get("moves").is_some())
        .collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["id"], streamed.id);
    assert_eq!(sent[1]["id"], collected.id);
    server.engine.shutdown().await;
}

#[tokio::test]
async fn dropping_one_stream_terminates_only_that_query_without_blocking_another() {
    let server = TestServer::start().await;
    server.wait_ready().await;
    let slow = query(&server, json!({"overrideSettings": {"fakeDelayMs": 4000}}));
    let abandoned = server.engine.analyze_stream(&slow).await.unwrap();
    assert!(matches!(
        server.engine.analyze_stream(&slow).await,
        Err(EngineError::Parse(_))
    ));
    let fast = query(&server, json!({"moves": ["D4"]}));
    let mut active = server.engine.analyze_stream(&fast).await.unwrap();
    drop(abandoned);
    let result = timeout(Duration::from_secs(2), active.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.id, fast.id);
    assert!(active.next().await.is_none());
    timeout(Duration::from_secs(2), async {
        loop {
            if server
                .queries()
                .iter()
                .any(|q| q["action"] == "terminate" && q["terminateId"] == slow.id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let sent = server.queries();
    assert_eq!(sent.iter().filter(|q| q["id"] == slow.id).count(), 1);
    assert!(
        !sent
            .iter()
            .any(|q| q["action"] == "terminate" && q["terminateId"] == fast.id)
    );
    server.engine.shutdown().await;
}

#[tokio::test]
async fn process_death_fails_streams_but_shutdown_waiters_survive_restart() {
    let server = TestServer::start().await;
    server.wait_ready().await;
    let slow = query(&server, json!({"overrideSettings": {"fakeDelayMs": 4000}}));
    let mut pending = server.engine.analyze_stream(&slow).await.unwrap();
    let crash = query(&server, json!({"overrideSettings": {"fakeCrash": true}}));
    let mut crashing = server.engine.analyze_stream(&crash).await.unwrap();
    for stream in [&mut pending, &mut crashing] {
        assert!(matches!(
            timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap(),
            Some(Err(EngineError::ProcessDied))
        ));
        assert!(stream.next().await.is_none());
    }
    assert!(
        timeout(
            Duration::from_millis(50),
            server.engine.shutdown_requested()
        )
        .await
        .is_err()
    );
    server.wait_ready().await;
    let recovered = query(&server, json!({}));
    let mut stream = server.engine.analyze_stream(&recovered).await.unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert!(stream.next().await.is_none());
    server.engine.shutdown().await;
    timeout(Duration::from_secs(1), server.engine.shutdown_requested())
        .await
        .unwrap();
    assert!(matches!(
        server.engine.analyze_stream(&recovered).await,
        Err(EngineError::ShuttingDown)
    ));
}

#[tokio::test]
async fn cancelled_mid_write_kills_the_old_transport_and_recovers() {
    let server = TestServer::start_with(
        FakeOptions {
            env: &[("FAKE_KATAGO_STARTUP_DELAY", "3")],
        },
        |_| {},
    )
    .await;
    let large = query(
        &server,
        json!({
            "overrideSettings": {"padding": "x".repeat(4 * 1024 * 1024)}
        }),
    );
    let mut sending = Box::pin(server.engine.analyze_stream(&large));
    assert!(
        timeout(Duration::from_millis(100), &mut sending)
            .await
            .is_err()
    );
    drop(sending);
    assert!(!server.engine.status().ready);
    timeout(Duration::from_secs(1), server.wait_dead())
        .await
        .expect("cancellation kills before the old process starts reading stdin");
    server.wait_ready().await;
    assert_eq!(server.engine.status().restarts, 1);
    let recovered = query(&server, json!({}));
    assert!(server.engine.analyze(&recovered).await.is_ok());
    server.engine.shutdown().await;
}
