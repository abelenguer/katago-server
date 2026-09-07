//! Multiplexed, final-per-turn analysis over a single WebSocket connection.
//! Envelopes retain `snake_case` for existing clients; `payload`/`data` use HTTP `camelCase`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::{Instant, timeout};
use tracing::{debug, warn};
use utoipa::ToSchema;

use super::AppState;
use super::problem::ApiError;
use super::types::{AnalysisRequest, AnalysisResponse};
use super::validate::{self, MAX_REQUEST_ID_LEN};
use crate::engine::{AnalysisEngine, Query};
use crate::error::EngineError;

const PATH: &str = "/api/v1/analysis/ws";
const MAX_ACTIVE_REQUESTS: usize = 32;
const QUEUE_CAPACITY: usize = 128;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Client-to-server WebSocket envelopes. The envelope id overrides `payload.requestId`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Start an analysis, returning each requested turn as soon as it finishes.
    Analyze {
        request_id: String,
        payload: Box<AnalysisRequest>,
    },
    /// Cancel one active request belonging to this connection.
    Cancel { request_id: String },
    /// Application-level keepalive, answered with `pong`.
    Ping,
}

/// The terminal outcome of an accepted analysis request.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CompletionStatus {
    /// All requested final results were delivered to the connection's output queue.
    #[serde(rename = "completed")]
    Ok,
    /// The client cancelled this request.
    Cancelled,
    /// The analysis exceeded its deadline.
    Timeout,
    /// The engine failed or the analysis was terminated without a result.
    Error,
}

/// Server-to-client envelopes. Results from different requests may interleave.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// The query has been written to KataGo, not necessarily started by it.
    Accepted {
        request_id: String,
        connection_id: String,
    },
    /// One final result, using the same data schema as HTTP analysis.
    Analysis {
        request_id: String,
        turn_number: u32,
        data: Box<AnalysisResponse>,
    },
    /// No more results will be delivered for this execution of the request id.
    Completed {
        request_id: String,
        status: CompletionStatus,
        expected: usize,
        received: usize,
    },
    /// A protocol, validation, or engine error. Invalid envelopes use id `unknown`.
    Error {
        request_id: String,
        code: String,
        message: String,
    },
    /// Reply to the application-level `ping`.
    Pong,
}

/// Upgrade to the multiplexed analysis protocol described in `docs/api.md`.
#[utoipa::path(
    get,
    path = "/api/v1/analysis/ws",
    tag = "analysis",
    responses(
        (status = 101, description = "WebSocket upgrade; exchanges ClientMessage and ServerMessage envelopes"),
        (status = 400, description = "Invalid WebSocket upgrade request", body = super::problem::ProblemDetails, content_type = "application/problem+json"),
        (status = 403, description = "WebSocket Origin is not allowed", body = super::problem::ProblemDetails, content_type = "application/problem+json"),
    )
)]
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let ws = ws.map_err(|error| {
        ApiError::new(
            error.status(),
            "invalid-request",
            "Invalid WebSocket Upgrade",
            error.body_text(),
        )
        .with_instance(PATH)
    })?;
    let origins = &state.config.server.cors_allowed_origins;
    if let Some(origin) = headers.get(header::ORIGIN)
        && !origins
            .iter()
            .any(|allowed| allowed == "*" || origin.to_str().is_ok_and(|origin| origin == allowed))
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin-not-allowed",
            "Origin Not Allowed",
            "this origin is not allowed to open an analysis WebSocket",
        )
        .with_instance(PATH));
    }
    let max_size = state.config.server.max_body_bytes;
    let session = state.ws_sessions.token();
    Ok(ws
        .max_message_size(max_size)
        .max_frame_size(max_size)
        .on_upgrade(move |socket| async move {
            let _session = session;
            serve(socket, state.engine).await;
        }))
}

struct ActiveRequest {
    internal_id: String,
    task: AbortHandle,
    expected: usize,
    received: usize,
    started: Instant,
}

struct QueryEvent {
    request_id: String,
    internal_id: String,
    message: QueryMessage,
}

enum QueryMessage {
    Accepted,
    Analysis(Box<AnalysisResponse>),
    Finished(Result<(), Failure>),
}

struct Failure {
    code: &'static str,
    message: String,
}

impl From<EngineError> for Failure {
    fn from(error: EngineError) -> Self {
        let code = match &error {
            EngineError::Timeout(_) => "timeout",
            EngineError::Rejected { .. } => "bad_request",
            EngineError::ProcessDied | EngineError::ProcessStartFailed(_) => "process_died",
            EngineError::ShuttingDown => "shutting_down",
            EngineError::Parse(_) | EngineError::Json(_) => "parse_error",
            EngineError::Katago(_) | EngineError::Io(_) => "katago_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

struct Session {
    connection_id: String,
    engine: AnalysisEngine,
    active: HashMap<String, ActiveRequest>,
    tasks: JoinSet<()>,
    events: mpsc::Sender<QueryEvent>,
    outgoing: mpsc::Sender<Message>,
    overloaded: Arc<Notify>,
}

impl Session {
    fn send(&self, message: &ServerMessage) -> Result<(), ()> {
        let text = serde_json::to_string(message).map_err(|error| {
            warn!(%error, "could not serialize WebSocket response");
        })?;
        self.outgoing
            .try_send(Message::Text(text.into()))
            .map_err(|_| ())
    }

    fn error(&self, request_id: String, code: &str, message: String) -> Result<(), ()> {
        self.send(&ServerMessage::Error {
            request_id,
            code: code.to_owned(),
            message,
        })
    }

    fn receive(&mut self, message: Message) -> Result<(), ()> {
        match message {
            Message::Text(text) => match serde_json::from_str(&text) {
                Ok(command) => self.command(command),
                Err(error) => self.error("unknown".into(), "bad_request", error.to_string()),
            },
            Message::Binary(_) => self.error(
                "unknown".into(),
                "bad_request",
                "binary messages are not supported; send a JSON text message".into(),
            ),
            Message::Ping(data) => self.outgoing.try_send(Message::Pong(data)).map_err(|_| ()),
            Message::Pong(_) => Ok(()),
            Message::Close(_) => Err(()),
        }
    }

    fn command(&mut self, command: ClientMessage) -> Result<(), ()> {
        let request_id = match &command {
            ClientMessage::Analyze { request_id, .. } | ClientMessage::Cancel { request_id } => {
                request_id
            }
            ClientMessage::Ping => return self.send(&ServerMessage::Pong),
        };
        if request_id.trim().is_empty() || request_id.len() > MAX_REQUEST_ID_LEN {
            return self.error(
                request_id.clone(),
                "bad_request",
                format!("request_id must be nonblank and at most {MAX_REQUEST_ID_LEN} bytes"),
            );
        }
        match command {
            ClientMessage::Analyze {
                request_id,
                mut payload,
            } => {
                if self.active.contains_key(&request_id) {
                    return self.error(
                        request_id,
                        "duplicate_request_id",
                        "request_id is already active on this connection".into(),
                    );
                }
                if self.active.len() >= MAX_ACTIVE_REQUESTS {
                    return self.error(
                        request_id,
                        "too_many_requests",
                        format!(
                            "at most {MAX_ACTIVE_REQUESTS} requests may be active per connection"
                        ),
                    );
                }
                // The envelope id is echoed exactly; only the fresh UUID reaches KataGo.
                payload.request_id = None;
                let prepared = match validate::build_ws_query(&payload, self.engine.config()) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return self.error(request_id, "bad_request", error.to_problem().detail);
                    }
                };
                let internal_id = prepared.query.id.clone();
                let expected = prepared.query.expected_results();
                let task = self.tasks.spawn(run_query(
                    self.engine.clone(),
                    prepared.query,
                    request_id.clone(),
                    self.events.clone(),
                    Arc::clone(&self.overloaded),
                ));
                self.active.insert(
                    request_id,
                    ActiveRequest {
                        internal_id,
                        task,
                        expected,
                        received: 0,
                        started: Instant::now(),
                    },
                );
                Ok(())
            }
            ClientMessage::Cancel { request_id } => {
                let Some(active) = self.active.remove(&request_id) else {
                    return self.error(
                        request_id,
                        "not_found",
                        "request_id is not active on this connection".into(),
                    );
                };
                active.task.abort();
                self.complete(request_id, &active, CompletionStatus::Cancelled)
            }
            ClientMessage::Ping => unreachable!("ping handled before id validation"),
        }
    }

    fn event(&mut self, event: QueryEvent) -> Result<(), ()> {
        let Some(active) = self.active.get_mut(&event.request_id) else {
            return Ok(());
        };
        // A cancelled execution may still have queued events when its id is reused.
        if active.internal_id != event.internal_id {
            return Ok(());
        }
        match event.message {
            QueryMessage::Accepted => self.send(&ServerMessage::Accepted {
                request_id: event.request_id,
                connection_id: self.connection_id.clone(),
            }),
            QueryMessage::Analysis(mut data) => {
                active.received += 1;
                data.id.clone_from(&event.request_id);
                self.send(&ServerMessage::Analysis {
                    request_id: event.request_id,
                    turn_number: data.turn_number,
                    data,
                })
            }
            QueryMessage::Finished(outcome) => {
                let active = self
                    .active
                    .remove(&event.request_id)
                    .expect("active request");
                let status = match outcome {
                    Ok(()) => CompletionStatus::Ok,
                    Err(error) => {
                        self.error(event.request_id.clone(), error.code, error.message)?;
                        if error.code == "timeout" {
                            CompletionStatus::Timeout
                        } else {
                            CompletionStatus::Error
                        }
                    }
                };
                self.complete(event.request_id, &active, status)
            }
        }
    }

    fn complete(
        &self,
        request_id: String,
        active: &ActiveRequest,
        status: CompletionStatus,
    ) -> Result<(), ()> {
        let label = match status {
            CompletionStatus::Ok => "ok",
            CompletionStatus::Cancelled => "cancelled",
            CompletionStatus::Timeout => "timeout",
            CompletionStatus::Error => "error",
        };
        metrics::counter!("katago_ws_requests_total", "outcome" => label).increment(1);
        metrics::histogram!("katago_ws_duration_seconds", "outcome" => label)
            .record(active.started.elapsed().as_secs_f64());
        self.send(&ServerMessage::Completed {
            request_id,
            status,
            expected: active.expected,
            received: active.received,
        })
    }

    fn shutdown(&mut self) -> Result<(), ()> {
        for (id, active) in std::mem::take(&mut self.active) {
            active.task.abort();
            self.error(
                id.clone(),
                "shutting_down",
                "the server is shutting down".into(),
            )?;
            self.complete(id, &active, CompletionStatus::Error)?;
        }
        Ok(())
    }
}

async fn serve(socket: WebSocket, engine: AnalysisEngine) {
    let (mut sink, mut source) = socket.split();
    let (outgoing, mut output) = mpsc::channel(QUEUE_CAPACITY);
    let (events, mut input) = mpsc::channel(QUEUE_CAPACITY);
    let overloaded = Arc::new(Notify::new());
    let mut session = Session {
        connection_id: uuid::Uuid::new_v4().to_string(),
        engine: engine.clone(),
        active: HashMap::new(),
        tasks: JoinSet::new(),
        events,
        outgoing,
        overloaded: Arc::clone(&overloaded),
    };
    debug!(connection_id = %session.connection_id, "WebSocket connected");
    let writer = async move {
        while let Some(message) = output.recv().await {
            if !matches!(timeout(WRITE_TIMEOUT, sink.send(message)).await, Ok(Ok(()))) {
                return;
            }
        }
        let _ = timeout(WRITE_TIMEOUT, sink.close()).await;
    };
    tokio::pin!(writer);
    let mut writer_finished = false;
    loop {
        tokio::select! {
            biased;
            () = engine.shutdown_requested() => {
                let _ = session.shutdown();
                break;
            }
            () = overloaded.notified() => break,
            () = &mut writer => {
                writer_finished = true;
                break;
            }
            Some(event) = input.recv() => {
                if session.event(event).is_err() {
                    break;
                }
            }
            done = session.tasks.join_next(), if !session.tasks.is_empty() => {
                if let Some(Err(error)) = done
                    && !error.is_cancelled()
                {
                    warn!(%error, "WebSocket analysis task failed");
                    break;
                }
            }
            message = source.next() => {
                let Some(Ok(message)) = message else { break };
                if session.receive(message).is_err() {
                    break;
                }
            }
        }
    }
    session.tasks.shutdown().await;
    debug!(connection_id = %session.connection_id, "WebSocket disconnected");
    drop(session);
    if !writer_finished {
        let _ = timeout(WRITE_TIMEOUT, &mut writer).await;
    }
}

async fn run_query(
    engine: AnalysisEngine,
    query: Query,
    request_id: String,
    events: mpsc::Sender<QueryEvent>,
    overloaded: Arc<Notify>,
) {
    let emit = |message| {
        events
            .try_send(QueryEvent {
                request_id: request_id.clone(),
                internal_id: query.id.clone(),
                message,
            })
            .map_err(|_| {
                overloaded.notify_one();
                Failure {
                    code: "slow_client",
                    message: "WebSocket response queue is full or closed".into(),
                }
            })
    };
    let work = async {
        let mut stream = engine.analyze_stream(&query).await.map_err(Failure::from)?;
        emit(QueryMessage::Accepted)?;
        while let Some(response) = stream.next().await {
            let response = response.map_err(Failure::from)?;
            if response.no_results {
                return Err(Failure {
                    code: "analysis_terminated",
                    message: "the search was terminated before it produced all results".into(),
                });
            }
            emit(QueryMessage::Analysis(Box::new(response)))?;
        }
        Ok(())
    };
    let wait = engine.config().move_timeout_secs;
    let outcome = timeout(Duration::from_secs(wait), work)
        .await
        .unwrap_or_else(|_| Err(Failure::from(EngineError::Timeout(wait))));
    let _ = emit(QueryMessage::Finished(outcome));
}
