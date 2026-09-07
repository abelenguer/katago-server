//! Caller-owned analysis subscriptions; no per-query worker task is spawned.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout_at};

use super::{AnalysisEngine, PendingGuard, Query, classify_message};
use crate::api::types::AnalysisResponse;
use crate::error::{EngineError, Result};

/// Final results in arrival order, one per requested turn. Dropping an unfinished
/// stream unregisters it and asks KataGo to terminate the query (best effort).
#[derive(Debug)]
pub struct AnalysisStream {
    guard: Option<PendingGuard>,
    receiver: mpsc::UnboundedReceiver<Value>,
    turns: HashMap<u32, bool>,
    remaining: usize,
    silence_timeout: Duration,
    deadline: Instant,
    timed_out: bool,
}

impl AnalysisEngine {
    /// Registers and sends a query using its unique internal id. The returned
    /// stream owns the subscription, independently of `self` and `query`.
    ///
    /// Cancelling this future unregisters the query. If cancelled mid-write,
    /// stdin is closed and its child is killed to avoid corrupting subsequent
    /// queries. Other in-flight work fails as the supervisor restarts KataGo.
    pub async fn analyze_stream(&self, query: &Query) -> Result<AnalysisStream> {
        let turns: HashMap<_, _> =
            if let Some(turns) = query.analyze_turns.as_ref().filter(|t| !t.is_empty()) {
                turns.iter().map(|&turn| (turn, false)).collect()
            } else {
                let turn = u32::try_from(query.moves.len())
                    .map_err(|_| EngineError::Parse("too many moves for a turn number".into()))?;
                HashMap::from([(turn, false)])
            };
        let value = serde_json::to_value(query)?;
        let (guard, receiver) = self.inner.register(&query.id)?;
        self.inner.send(&value).await?;
        let silence_timeout = Duration::from_secs(self.inner.config.move_timeout_secs);
        Ok(AnalysisStream {
            guard: Some(guard),
            receiver,
            remaining: turns.len(),
            turns,
            silence_timeout,
            deadline: Instant::now() + silence_timeout,
            timed_out: false,
        })
    }
}

impl AnalysisStream {
    /// Returns the next unique final result, or one terminal error, then `None`.
    /// Warnings and search updates reset the silence timeout but are not emitted.
    /// Cancelling this method does not lose a result or reset the deadline.
    pub async fn next(&mut self) -> Option<Result<AnalysisResponse>> {
        let guard = self.guard.as_ref()?;
        let outcome = async {
            loop {
                if self.timed_out {
                    return Err(EngineError::Timeout(self.silence_timeout.as_secs()));
                }
                let message = match timeout_at(self.deadline, self.receiver.recv()).await {
                    Ok(Some(message)) => message,
                    Ok(None) => return Err(EngineError::ProcessDied),
                    Err(_) => {
                        self.timed_out = true;
                        continue;
                    }
                };
                self.deadline = Instant::now() + self.silence_timeout;
                let Some(message) = classify_message(&guard.id, message)? else {
                    continue;
                };
                if let Some(action) = message.get("action") {
                    return Err(EngineError::Parse(format!(
                        "unexpected action in analysis response: {action}"
                    )));
                }
                let turn = message
                    .get("turnNumber")
                    .and_then(Value::as_u64)
                    .and_then(|turn| u32::try_from(turn).ok())
                    .ok_or_else(|| EngineError::Parse("missing or invalid turnNumber".into()))?;
                let seen = self.turns.get_mut(&turn).ok_or_else(|| {
                    EngineError::Parse(format!("unexpected analysis turnNumber {turn}"))
                })?;
                match message.get("isDuringSearch").and_then(Value::as_bool) {
                    Some(true) => continue,
                    Some(false) => {}
                    None => {
                        return Err(EngineError::Parse(
                            "missing or invalid isDuringSearch".into(),
                        ));
                    }
                }
                let response = serde_json::from_value::<AnalysisResponse>(message)
                    .map_err(|e| EngineError::Parse(e.to_string()))?;
                if *seen {
                    continue;
                }
                *seen = true;
                self.remaining -= 1;
                return Ok(response);
            }
        }
        .await;
        if self.timed_out {
            // Keep the timeout terminal even if next() is cancelled while the
            // terminate write waits. Dropping the stream still cancels normally.
            guard.inner.terminate(&guard.id).await;
        }
        if (outcome.is_err() || self.remaining == 0)
            && let Some(mut guard) = self.guard.take()
        {
            guard.finished = outcome.is_ok() || self.timed_out;
        }
        Some(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::task::{Context, Poll, Waker};

    use serde_json::json;
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use super::*;
    use crate::engine::Inner;

    fn engine() -> AnalysisEngine {
        AnalysisEngine {
            inner: Arc::new(Inner {
                config: crate::config::KatagoConfig::default(),
                stdin: tokio::sync::Mutex::new(None),
                child: tokio::sync::Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                alive: AtomicBool::new(true),
                ready: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                restarts: AtomicU32::new(0),
                started_at: std::time::Instant::now(),
                version: RwLock::new(None),
                exited: Notify::new(),
                shutdown: Notify::new(),
                stderr_tail: Mutex::new(std::collections::VecDeque::new()),
            }),
        }
    }

    fn stream(engine: &AnalysisEngine, turns: &[u32]) -> AnalysisStream {
        let (guard, receiver) = engine.inner.register("query").unwrap();
        let turns: HashMap<_, _> = turns.iter().map(|&turn| (turn, false)).collect();
        AnalysisStream {
            guard: Some(guard),
            receiver,
            remaining: turns.len(),
            turns,
            silence_timeout: Duration::from_secs(60),
            deadline: Instant::now() + Duration::from_secs(60),
            timed_out: false,
        }
    }

    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[tokio::test]
    async fn emits_only_unique_finals_in_arrival_order() {
        let engine = engine();
        let mut stream = stream(&engine, &[0, 1, 2, 2]);
        for message in [
            json!({"id": "query", "warning": "unused", "field": "fakeWarn"}),
            json!({"id": "query", "turnNumber": 0, "isDuringSearch": true}),
            json!({"id": "query", "turnNumber": 2, "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": 2, "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": 0, "isDuringSearch": false}),
        ] {
            engine.inner.route_line(&message.to_string());
        }
        assert_eq!(stream.next().await.unwrap().unwrap().turn_number, 2);
        assert_eq!(stream.next().await.unwrap().unwrap().turn_number, 0);
        assert!(engine.inner.pending.lock().unwrap().contains_key("query"));
        engine
            .inner
            .route_line(r#"{"id":"query","turnNumber":1,"isDuringSearch":false,"noResults":true}"#);
        let result = stream.next().await.unwrap().unwrap();
        assert_eq!(result.turn_number, 1);
        assert!(result.no_results);
        assert!(result.root_info.is_none());
        assert!(stream.next().await.is_none());
        assert!(engine.inner.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn errors_are_classified_before_defaults_or_warnings() {
        for field in [None, Some("moves")] {
            let engine = engine();
            let mut stream = stream(&engine, &[0]);
            engine.inner.route_line(
                &json!({"id": "query", "error": "broken", "field": field, "warning": "unused"})
                    .to_string(),
            );
            let error = stream.next().await.unwrap().unwrap_err();
            match (field, error) {
                (None, EngineError::Katago(message)) => assert_eq!(message, "broken"),
                (Some(_), EngineError::Rejected { message, field }) => {
                    assert_eq!(message, "broken");
                    assert_eq!(field.as_deref(), Some("moves"));
                }
                other => panic!("wrong error classification: {other:?}"),
            }
            assert!(stream.next().await.is_none());
            assert!(engine.inner.pending.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn rejects_actions_unrequested_turns_and_malformed_results() {
        for message in [
            json!({"id": "query", "action": "clear_cache"}),
            json!({"id": "query", "turnNumber": 1, "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": 1, "isDuringSearch": true}),
            json!({"id": "query", "turnNumber": -1, "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": 4_294_967_296_u64, "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": "0", "isDuringSearch": false}),
            json!({"id": "query", "isDuringSearch": false}),
            json!({"id": "query", "turnNumber": 0}),
            json!({"id": "query", "turnNumber": 0, "isDuringSearch": "false"}),
            json!({"id": "query", "turnNumber": 0, "isDuringSearch": false, "rootInfo": {}}),
        ] {
            let engine = engine();
            let mut stream = stream(&engine, &[0]);
            engine.inner.route_line(&message.to_string());
            assert!(matches!(
                stream.next().await,
                Some(Err(EngineError::Parse(_)))
            ));
            assert!(stream.next().await.is_none());
            assert!(engine.inner.pending.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn duplicate_ids_are_rejected_and_stale_guards_cannot_remove_new_subscriptions() {
        let engine = engine();
        let old = stream(&engine, &[0]);
        assert!(matches!(
            engine.inner.register("query"),
            Err(EngineError::Parse(_))
        ));
        engine.inner.pending.lock().unwrap().clear();
        let mut newer = stream(&engine, &[0]);
        drop(old);
        assert!(engine.inner.pending.lock().unwrap().contains_key("query"));
        engine
            .inner
            .route_line(r#"{"id":"query","turnNumber":0,"isDuringSearch":false}"#);
        assert!(newer.next().await.unwrap().is_ok());
        assert!(engine.inner.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn next_cancellation_preserves_silence_deadline_and_messages_reset_it() {
        let engine = engine();
        let mut stream = stream(&engine, &[0, 1]);
        engine
            .inner
            .route_line(r#"{"id":"query","turnNumber":0,"isDuringSearch":false}"#);
        stream.next().await.unwrap().unwrap();
        for message in [
            r#"{"id":"query","warning":"unused"}"#,
            r#"{"id":"query","turnNumber":1,"isDuringSearch":true}"#,
            r#"{"id":"query","turnNumber":0,"isDuringSearch":false}"#,
        ] {
            let later = Instant::now() + Duration::from_secs(3600);
            stream.deadline = later;
            engine.inner.route_line(message);
            assert!(poll(std::pin::pin!(stream.next())).is_pending());
            assert!(stream.deadline < later);
            assert!(stream.deadline > Instant::now());
            let deadline = stream.deadline;
            assert!(poll(std::pin::pin!(stream.next())).is_pending());
            assert_eq!(stream.deadline, deadline);
        }
        stream.deadline = Instant::now() - Duration::from_secs(1);
        assert!(matches!(
            stream.next().await,
            Some(Err(EngineError::Timeout(60)))
        ));
        assert!(stream.next().await.is_none());
        assert!(engine.inner.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn timeout_waits_for_terminate_and_survives_next_cancellation() {
        let engine = engine();
        let mut stream = stream(&engine, &[0]);
        stream.deadline = Instant::now() - Duration::from_secs(1);
        let stdin = engine.inner.stdin.lock().await;
        assert!(poll(std::pin::pin!(stream.next())).is_pending());
        assert!(stream.timed_out);
        assert!(engine.inner.pending.lock().unwrap().contains_key("query"));

        // Cancelling next() during termination must not admit a late success.
        engine
            .inner
            .route_line(r#"{"id":"query","turnNumber":0,"isDuringSearch":false}"#);
        drop(stdin);
        assert!(matches!(
            stream.next().await,
            Some(Err(EngineError::Timeout(60)))
        ));
        assert!(stream.next().await.is_none());
        assert!(engine.inner.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn process_death_closes_streams_without_signalling_shutdown() {
        let engine = engine();
        let mut stream = stream(&engine, &[0]);
        let mut first = std::pin::pin!(engine.shutdown_requested());
        let mut second = std::pin::pin!(engine.shutdown_requested());
        assert!(poll(first.as_mut()).is_pending());
        assert!(poll(second.as_mut()).is_pending());
        engine.inner.on_exit();
        assert!(matches!(
            stream.next().await,
            Some(Err(EngineError::ProcessDied))
        ));
        assert!(poll(first.as_mut()).is_pending());
        assert!(poll(second.as_mut()).is_pending());

        // Hold stdin to prove notification precedes even shutdown's first I/O.
        let stdin = engine.inner.stdin.lock().await;
        let mut shutdown = std::pin::pin!(engine.shutdown());
        assert!(poll(shutdown.as_mut()).is_pending());
        assert!(poll(first.as_mut()).is_ready());
        assert!(poll(second.as_mut()).is_ready());
        assert!(poll(std::pin::pin!(engine.shutdown_requested())).is_ready());
        drop(stdin);
        shutdown.await;
    }

    #[tokio::test]
    async fn cancelling_stream_creation_while_waiting_to_send_unregisters() {
        let engine = engine();
        let request = serde_json::from_value(json!({})).unwrap();
        let query = crate::api::validate::build_query(&request, engine.config(), false)
            .unwrap()
            .query;
        let stdin = engine.inner.stdin.lock().await;
        let mut sending = Box::pin(engine.analyze_stream(&query));
        assert!(poll(sending.as_mut()).is_pending());
        assert!(engine.inner.pending.lock().unwrap().contains_key(&query.id));
        drop(sending);
        assert!(engine.inner.pending.lock().unwrap().is_empty());
        drop(stdin);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_mid_write_kills_an_eof_ignoring_child_and_fails_other_streams() {
        let engine = engine();
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        *engine.inner.stdin.lock().await = child.stdin.take();
        *engine.inner.child.lock().await = Some(child);
        engine.inner.ready.store(true, Ordering::SeqCst);
        let mut reader = tokio::spawn(Inner::read_stdout(Arc::clone(&engine.inner), stdout));
        let mut other = stream(&engine, &[0]);
        let request = serde_json::from_value(json!({
            "overrideSettings": {"padding": "x".repeat(4 * 1024 * 1024)}
        }))
        .unwrap();
        let query = crate::api::validate::build_query(&request, engine.config(), false)
            .unwrap()
            .query;
        let mut sending = Box::pin(engine.analyze_stream(&query));
        assert!(
            timeout(Duration::from_millis(100), &mut sending)
                .await
                .is_err()
        );
        assert!(engine.inner.pending.lock().unwrap().contains_key(&query.id));
        drop(sending);
        assert!(!engine.inner.pending.lock().unwrap().contains_key(&query.id));
        assert!(!engine.status().ready);
        assert!(engine.inner.stdin.lock().await.is_none());
        let stopped = timeout(Duration::from_secs(2), &mut reader).await;
        let status = engine.inner.reap(Duration::ZERO).await.unwrap();
        stopped
            .expect("broken child must not keep running after stdin closes")
            .unwrap();
        assert!(!status.success());
        assert!(!engine.status().alive);
        assert!(matches!(
            other.next().await,
            Some(Err(EngineError::ProcessDied))
        ));
        assert!(engine.inner.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn administrative_collector_still_accepts_actions_and_ignores_warnings() {
        let engine = engine();
        let (mut guard, mut receiver) = engine.inner.register("admin").unwrap();
        engine
            .inner
            .route_line(r#"{"id":"admin","warning":"unused"}"#);
        engine
            .inner
            .route_line(r#"{"id":"admin","action":"clear_cache"}"#);
        let results = engine
            .inner
            .collect("admin", &mut receiver, 1, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["action"], "clear_cache");
        guard.finished = true;
    }
}
