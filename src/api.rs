use crate::analysis_engine::AnalysisEngine;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

pub type AppState = Arc<AnalysisEngine>;

/// A move can be either a simple coordinate or an explicit [color, coordinate] pair
/// This allows clients to specify exact colors for handicap games where alternation
/// doesn't match the actual game (e.g., White plays first in handicap games)
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MoveInput {
    /// Simple coordinate (e.g., "D4") - color inferred from position/alternation
    Simple(String),
    /// Explicit color and coordinate (e.g., ["W", "D4"] or ["B", "Q16"])
    WithColor([String; 2]),
}

impl MoveInput {
    /// Get the coordinate from the move
    pub fn coord(&self) -> &str {
        match self {
            MoveInput::Simple(coord) => coord,
            MoveInput::WithColor([_, coord]) => coord,
        }
    }

    /// Get explicit color if provided, None for simple moves
    pub fn color(&self) -> Option<&str> {
        match self {
            MoveInput::Simple(_) => None,
            MoveInput::WithColor([color, _]) => Some(color),
        }
    }
}

// ============================================================================
// New V1 API Types
// ============================================================================

/// Comprehensive analysis request supporting all KataGo features
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Some fields reserved for future enhancements
pub struct AnalysisRequest {
    /// Moves played so far - can be simple coordinates (e.g., ["D4", "Q16"]) or
    /// explicit color pairs (e.g., [["W", "D4"], ["B", "Q16"]]) for handicap games
    pub moves: Vec<MoveInput>,

    /// Game rules: "tromp-taylor", "chinese", "japanese", "korean", "aga", etc.
    #[serde(default)]
    pub rules: Option<String>,

    /// Komi value for the game
    #[serde(default)]
    pub komi: Option<f32>,

    /// Board width (typically 19)
    #[serde(default = "default_board_size")]
    pub board_x_size: u8,

    /// Board height (typically 19)
    #[serde(default = "default_board_size")]
    pub board_y_size: u8,

    /// Initial stones for handicap games
    #[serde(default)]
    pub initial_stones: Option<Vec<(String, String)>>,

    /// Player to move at turn 0
    #[serde(default)]
    pub initial_player: Option<String>,

    /// Which turns to analyze (defaults to final position)
    #[serde(default)]
    pub analyze_turns: Option<Vec<u32>>,

    // Analysis control parameters
    /// Override config file visit limit
    #[serde(default)]
    pub max_visits: Option<u32>,

    /// Temperature for root policy (>1 = more exploration)
    #[serde(default)]
    pub root_policy_temperature: Option<f32>,

    /// FPU reduction for exploration
    #[serde(default)]
    pub root_fpu_reduction_max: Option<f32>,

    /// Length of principal variation to return
    #[serde(default)]
    pub analysis_pv_len: Option<u32>,

    // Data request flags
    /// Include territory ownership predictions
    #[serde(default)]
    pub include_ownership: Option<bool>,

    /// Include ownership standard deviation
    #[serde(default)]
    pub include_ownership_stdev: Option<bool>,

    /// Include ownership for each move candidate
    #[serde(default)]
    pub include_moves_ownership: Option<bool>,

    /// Include raw neural network policy
    #[serde(default)]
    pub include_policy: Option<bool>,

    /// Include visit counts in principal variations
    #[serde(default)]
    pub include_pv_visits: Option<bool>,

    // Move filtering
    /// Moves to avoid considering
    #[serde(default)]
    pub avoid_moves: Option<Vec<MoveFilter>>,

    /// Only consider these moves
    #[serde(default)]
    pub allow_moves: Option<Vec<MoveFilter>>,

    // Advanced settings
    /// Override search parameters
    #[serde(default)]
    pub override_settings: Option<serde_json::Value>,

    /// Report partial results during search (seconds)
    #[serde(default)]
    pub report_during_search_every: Option<f32>,

    /// Query priority
    #[serde(default)]
    pub priority: Option<i32>,

    /// Optional request identifier
    #[serde(default)]
    pub request_id: Option<String>,
}

fn default_board_size() -> u8 {
    19
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Reserved for future move filtering support
pub struct MoveFilter {
    pub player: String,
    pub moves: Vec<String>,
    pub until_depth: u32,
}

const MAX_WS_ACTIVE_REQUESTS_PER_CONNECTION: usize = 32;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WsClientMessage {
    Analyze {
        request_id: String,
        payload: AnalysisRequest,
    },
    Cancel {
        request_id: String,
    },
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WsServerMessage {
    Accepted {
        request_id: String,
        connection_id: String,
    },
    Analysis {
        request_id: String,
        turn_number: u32,
        data: AnalysisResponse,
    },
    Error {
        request_id: String,
        code: String,
        message: String,
    },
    Completed {
        request_id: String,
        status: String,
        expected: usize,
        received: usize,
    },
    Pong,
}

struct ActiveWsRequest {
    internal_request_id: String,
    cancel_tx: oneshot::Sender<()>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResponse {
    pub id: String,
    pub turn_number: u32,
    pub is_during_search: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_infos: Option<Vec<MoveInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_info: Option<RootInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership_stdev: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<Vec<f32>>,
    /// Human SL model policy predictions (requires human model and includePolicy=true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_policy: Option<Vec<f32>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveInfo {
    pub move_coord: String,
    pub visits: u32,
    pub winrate: f32,
    pub score_mean: f32,
    pub score_stdev: f32,
    pub score_lead: f32,
    pub utility: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utility_lcb: Option<f32>,
    pub lcb: f32,
    pub prior: f32,
    /// Human SL model prior for this move (requires human model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_prior: Option<f32>,
    pub order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pv_visits: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<Vec<f32>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub winrate: f32,
    pub score_lead: f32,
    pub utility: f32,
    pub visits: u32,
    pub current_player: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_winrate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_score_mean: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_st_score_error: Option<f32>,
    // Human SL model fields (requires human model and humanSLProfile in overrideSettings)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_winrate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_score_mean: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_score_stdev: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub server: ServerVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub katago: Option<KatagoVersion>,
    pub model: ModelInfo,
}

#[derive(Debug, Serialize)]
pub struct ServerVersion {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KatagoVersion {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_hash: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CacheClearResponse {
    pub status: String,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<u64>,
}

// RFC 7807 Problem Details
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemDetail {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

// Custom error type for API responses with RFC 7807 support
pub struct ApiError {
    problem: ProblemDetail,
}

impl ApiError {
    pub fn new(status: StatusCode, title: &str, detail: &str) -> Self {
        Self {
            problem: ProblemDetail {
                problem_type: format!(
                    "https://katago-server/problems/{}",
                    title.to_lowercase().replace(' ', "-")
                ),
                title: title.to_string(),
                status: status.as_u16(),
                detail: detail.to_string(),
                instance: None,
                request_id: None,
            },
        }
    }

    pub fn with_request_id(mut self, request_id: String) -> Self {
        self.problem.request_id = Some(request_id);
        self
    }

    #[allow(dead_code)] // May be useful for future error context
    pub fn with_instance(mut self, instance: String) -> Self {
        self.problem.instance = Some(instance);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error!("API error: {}", self.problem.detail);
        let status =
            StatusCode::from_u16(self.problem.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut headers = HeaderMap::new();
        headers.insert("Content-Type", "application/problem+json".parse().unwrap());

        (status, headers, Json(self.problem)).into_response()
    }
}

impl From<crate::error::KatagoError> for ApiError {
    fn from(err: crate::error::KatagoError) -> Self {
        use crate::error::KatagoError;
        match err {
            KatagoError::Timeout(secs) => ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "Analysis Timeout",
                &format!("KataGo analysis timed out after {} seconds", secs),
            ),
            KatagoError::ProcessDied => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                "KataGo process has died unexpectedly",
            ),
            KatagoError::ParseError(msg) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Parse Error",
                &format!("Failed to parse KataGo response: {}", msg),
            ),
            KatagoError::ProcessStartFailed(msg) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                &format!("Failed to start KataGo: {}", msg),
            ),
            KatagoError::InvalidCommand(msg) => ApiError::new(
                StatusCode::BAD_REQUEST,
                "Invalid Request",
                &format!("Invalid command: {}", msg),
            ),
            KatagoError::ResponseError(msg) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "KataGo Error",
                &format!("KataGo returned error: {}", msg),
            ),
            KatagoError::IoError(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                &format!("IO error: {}", err),
            ),
            KatagoError::JsonError(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "JSON Error",
                &format!("JSON parsing error: {}", err),
            ),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Error",
            &err.to_string(),
        )
    }
}

pub fn create_router(engine: AppState) -> Router {
    Router::new()
        .route("/api/v1/analysis", post(v1_analysis))
        .route("/api/v1/analysis/ws", get(v1_analysis_ws))
        .route("/api/v1/health", get(v1_health))
        .route("/api/v1/version", get(v1_version))
        .route("/api/v1/cache/clear", post(v1_cache_clear))
        .with_state(engine)
}

fn validate_analyze_turns(request: &AnalysisRequest) -> std::result::Result<usize, String> {
    let Some(turns) = request.analyze_turns.as_ref() else {
        return Ok(1);
    };

    if turns.is_empty() {
        return Err("analyzeTurns cannot be an empty array".to_string());
    }

    let max_turn = request.moves.len() as u32;
    let mut seen = HashSet::with_capacity(turns.len());
    for turn in turns {
        if *turn > max_turn {
            return Err(format!(
                "analyzeTurns contains out-of-range turn {} (valid range: 0..={})",
                turn, max_turn
            ));
        }
        if !seen.insert(*turn) {
            return Err(format!("analyzeTurns contains duplicate turn {}", turn));
        }
    }

    Ok(turns.len())
}

// ============================================================================
// V1 API Handlers
// ============================================================================

#[axum::debug_handler]
async fn v1_analysis(
    State(engine): State<AppState>,
    Json(request): Json<AnalysisRequest>,
) -> std::result::Result<Json<AnalysisResponse>, ApiError> {
    let request_id = request
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Err(message) = validate_analyze_turns(&request) {
        return Err(
            ApiError::new(StatusCode::BAD_REQUEST, "Invalid Request", &message)
                .with_request_id(request_id),
        );
    }

    // Use JSON analysis engine for full move analysis
    let response = engine
        .analyze(&request)
        .await
        .map_err(|e| ApiError::from(e).with_request_id(request_id.clone()))?;

    Ok(Json(response))
}

async fn v1_analysis_ws(ws: WebSocketUpgrade, State(engine): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_analysis_ws(socket, engine))
}

async fn handle_analysis_ws(mut socket: WebSocket, engine: AppState) {
    let connection_id = uuid::Uuid::new_v4().to_string();
    info!(connection_id, "WebSocket analysis connection opened");

    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<WsServerMessage>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<String>();
    let mut active_requests: HashMap<String, ActiveWsRequest> = HashMap::new();

    loop {
        tokio::select! {
            maybe_outgoing = outgoing_rx.recv() => {
                match maybe_outgoing {
                    Some(message) => {
                        match serde_json::to_string(&message) {
                            Ok(json) => {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                error!(connection_id, "Failed to serialize WS message: {}", e);
                            }
                        }
                    }
                    None => break,
                }
            }
            maybe_done = done_rx.recv() => {
                if let Some(request_id) = maybe_done {
                    active_requests.remove(&request_id);
                } else {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsClientMessage>(&text) {
                            Ok(WsClientMessage::Ping) => {
                                let _ = outgoing_tx.send(WsServerMessage::Pong);
                            }
                            Ok(WsClientMessage::Cancel { request_id }) => {
                                if let Some(active) = active_requests.remove(&request_id) {
                                    let _ = active.cancel_tx.send(());
                                } else {
                                    let _ = outgoing_tx.send(WsServerMessage::Error {
                                        request_id,
                                        code: "not_found".to_string(),
                                        message: "requestId is not active for this connection".to_string(),
                                    });
                                }
                            }
                            Ok(WsClientMessage::Analyze { request_id, payload }) => {
                                if request_id.trim().is_empty() {
                                    let _ = outgoing_tx.send(WsServerMessage::Error {
                                        request_id,
                                        code: "bad_request".to_string(),
                                        message: "requestId cannot be empty".to_string(),
                                    });
                                    continue;
                                }

                                if active_requests.contains_key(&request_id) {
                                    let _ = outgoing_tx.send(WsServerMessage::Error {
                                        request_id,
                                        code: "duplicate_request_id".to_string(),
                                        message: "requestId is already active on this connection".to_string(),
                                    });
                                    continue;
                                }

                                if active_requests.len() >= MAX_WS_ACTIVE_REQUESTS_PER_CONNECTION {
                                    let _ = outgoing_tx.send(WsServerMessage::Error {
                                        request_id,
                                        code: "too_many_requests".to_string(),
                                        message: format!(
                                            "max active requests per connection is {}",
                                            MAX_WS_ACTIVE_REQUESTS_PER_CONNECTION
                                        ),
                                    });
                                    continue;
                                }

                                let expected = match validate_analyze_turns(&payload) {
                                    Ok(expected) => expected,
                                    Err(message) => {
                                        let _ = outgoing_tx.send(WsServerMessage::Error {
                                            request_id,
                                            code: "bad_request".to_string(),
                                            message,
                                        });
                                        continue;
                                    }
                                };

                                let internal_request_id = format!("ws:{}:{}", connection_id, request_id);
                                let mut request = payload;
                                request.request_id = Some(internal_request_id.clone());
                                let timeout_secs = engine.move_timeout_secs();

                                let stream_rx = engine.register_stream_waiter(&internal_request_id);
                                if let Err(e) = engine.send_analysis_request_with_id(&request, internal_request_id.clone()) {
                                    engine.remove_stream_waiter(&internal_request_id);
                                    let _ = outgoing_tx.send(WsServerMessage::Error {
                                        request_id: request_id.clone(),
                                        code: "katago_error".to_string(),
                                        message: e.to_string(),
                                    });
                                    let _ = outgoing_tx.send(WsServerMessage::Completed {
                                        request_id,
                                        status: "error".to_string(),
                                        expected,
                                        received: 0,
                                    });
                                    continue;
                                }

                                let _ = outgoing_tx.send(WsServerMessage::Accepted {
                                    request_id: request_id.clone(),
                                    connection_id: connection_id.clone(),
                                });

                                let (cancel_tx, cancel_rx) = oneshot::channel();
                                active_requests.insert(
                                    request_id.clone(),
                                    ActiveWsRequest {
                                        internal_request_id: internal_request_id.clone(),
                                        cancel_tx,
                                    },
                                );

                                let engine_clone = Arc::clone(&engine);
                                let outgoing_clone = outgoing_tx.clone();
                                let done_clone = done_tx.clone();
                                tokio::spawn(async move {
                                    stream_request_task(
                                        engine_clone,
                                        stream_rx,
                                        request_id,
                                        internal_request_id,
                                        expected,
                                        timeout_secs,
                                        cancel_rx,
                                        outgoing_clone,
                                        done_clone,
                                    )
                                    .await;
                                });
                            }
                            Err(e) => {
                                let _ = outgoing_tx.send(WsServerMessage::Error {
                                    request_id: "unknown".to_string(),
                                    code: "bad_request".to_string(),
                                    message: format!("invalid WS message: {}", e),
                                });
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {
                        let _ = outgoing_tx.send(WsServerMessage::Error {
                            request_id: "unknown".to_string(),
                            code: "bad_request".to_string(),
                            message: "binary WS messages are not supported".to_string(),
                        });
                    }
                    Some(Err(e)) => {
                        warn!(connection_id, "WebSocket receive error: {}", e);
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    for active in active_requests.into_values() {
        let _ = active.cancel_tx.send(());
        engine.remove_stream_waiter(&active.internal_request_id);
    }
    info!(connection_id, "WebSocket analysis connection closed");
}

#[allow(clippy::too_many_arguments)]
async fn stream_request_task(
    engine: AppState,
    mut stream_rx: mpsc::UnboundedReceiver<String>,
    client_request_id: String,
    internal_request_id: String,
    expected: usize,
    timeout_secs: u64,
    mut cancel_rx: oneshot::Receiver<()>,
    outgoing_tx: mpsc::UnboundedSender<WsServerMessage>,
    done_tx: mpsc::UnboundedSender<String>,
) {
    let mut seen_turns = HashSet::new();
    let mut received = 0usize;
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut cancel_rx => {
                let _ = outgoing_tx.send(WsServerMessage::Completed {
                    request_id: client_request_id.clone(),
                    status: "cancelled".to_string(),
                    expected,
                    received,
                });
                break;
            }
            _ = &mut timeout => {
                let _ = outgoing_tx.send(WsServerMessage::Error {
                    request_id: client_request_id.clone(),
                    code: "timeout".to_string(),
                    message: format!("analysis timed out after {} seconds", timeout_secs),
                });
                let _ = outgoing_tx.send(WsServerMessage::Completed {
                    request_id: client_request_id.clone(),
                    status: "timeout".to_string(),
                    expected,
                    received,
                });
                break;
            }
            maybe_raw = stream_rx.recv() => {
                match maybe_raw {
                    Some(raw) => {
                        match AnalysisEngine::parse_stream_response(&raw, client_request_id.clone()) {
                            Ok(response) => {
                                if !seen_turns.insert(response.turn_number) {
                                    debug!("Duplicate turnNumber {} for requestId {}", response.turn_number, client_request_id);
                                    continue;
                                }
                                received += 1;
                                let turn_number = response.turn_number;
                                let _ = outgoing_tx.send(WsServerMessage::Analysis {
                                    request_id: client_request_id.clone(),
                                    turn_number,
                                    data: response,
                                });
                                if received >= expected {
                                    let _ = outgoing_tx.send(WsServerMessage::Completed {
                                        request_id: client_request_id.clone(),
                                        status: "ok".to_string(),
                                        expected,
                                        received,
                                    });
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = outgoing_tx.send(WsServerMessage::Error {
                                    request_id: client_request_id.clone(),
                                    code: "parse_error".to_string(),
                                    message: e.to_string(),
                                });
                                let _ = outgoing_tx.send(WsServerMessage::Completed {
                                    request_id: client_request_id.clone(),
                                    status: "error".to_string(),
                                    expected,
                                    received,
                                });
                                break;
                            }
                        }
                    }
                    None => {
                        let _ = outgoing_tx.send(WsServerMessage::Error {
                            request_id: client_request_id.clone(),
                            code: "process_died".to_string(),
                            message: "analysis stream closed unexpectedly".to_string(),
                        });
                        let _ = outgoing_tx.send(WsServerMessage::Completed {
                            request_id: client_request_id.clone(),
                            status: "error".to_string(),
                            expected,
                            received,
                        });
                        break;
                    }
                }
            }
        }
    }

    engine.remove_stream_waiter(&internal_request_id);
    let _ = done_tx.send(client_request_id);
}

#[axum::debug_handler]
async fn v1_health(
    State(engine): State<AppState>,
) -> std::result::Result<Json<HealthResponse>, (axum::http::StatusCode, Json<HealthResponse>)> {
    use chrono::Utc;

    let is_alive = engine.is_alive();
    let status = if is_alive { "healthy" } else { "unhealthy" };

    let response = HealthResponse {
        status: status.to_string(),
        timestamp: Some(Utc::now().to_rfc3339()),
        uptime: None,
    };

    if is_alive {
        Ok(Json(response))
    } else {
        Err((axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(response)))
    }
}

#[axum::debug_handler]
async fn v1_version(
    State(engine): State<AppState>,
) -> std::result::Result<Json<VersionResponse>, ApiError> {
    // Get model name (filename only, not full path for security)
    let model_name = std::path::Path::new(engine.model_path())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Query KataGo version from the analysis engine
    let katago_info = engine
        .query_version()
        .await
        .ok()
        .map(|(version, git_hash)| KatagoVersion { version, git_hash });

    Ok(Json(VersionResponse {
        server: ServerVersion {
            name: "katago-server".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        katago: katago_info,
        model: ModelInfo { name: model_name },
    }))
}

#[axum::debug_handler]
async fn v1_cache_clear(
    State(engine): State<AppState>,
) -> std::result::Result<Json<CacheClearResponse>, ApiError> {
    use chrono::Utc;

    engine.clear_cache().await?;

    Ok(Json(CacheClearResponse {
        status: "cleared".to_string(),
        timestamp: Utc::now().to_rfc3339(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_request_deserialization() {
        let json = r#"{
            "moves": ["D4", "Q16"],
            "komi": 7.5,
            "rules": "chinese",
            "includeOwnership": true,
            "includePolicy": false
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.moves.len(), 2);
        assert_eq!(request.moves[0].coord(), "D4");
        assert_eq!(request.moves[1].coord(), "Q16");
        assert!(request.moves[0].color().is_none()); // Simple format
        assert_eq!(request.komi, Some(7.5));
        assert_eq!(request.rules, Some("chinese".to_string()));
        assert_eq!(request.include_ownership, Some(true));
        assert_eq!(request.include_policy, Some(false));
    }

    #[test]
    fn test_analysis_request_with_explicit_colors() {
        let json = r#"{
            "moves": [["W", "D4"], ["B", "Q16"]],
            "komi": 0.5,
            "rules": "chinese"
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.moves.len(), 2);
        assert_eq!(request.moves[0].coord(), "D4");
        assert_eq!(request.moves[0].color(), Some("W"));
        assert_eq!(request.moves[1].coord(), "Q16");
        assert_eq!(request.moves[1].color(), Some("B"));
    }

    #[test]
    fn test_analysis_request_with_defaults() {
        let json = r#"{
            "moves": []
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        assert!(request.moves.is_empty());
        assert_eq!(request.board_x_size, 19);
        assert_eq!(request.board_y_size, 19);
        assert!(request.komi.is_none());
        assert!(request.rules.is_none());
    }

    #[test]
    fn test_analysis_response_serialization() {
        let response = AnalysisResponse {
            id: "test-123".to_string(),
            turn_number: 5,
            is_during_search: false,
            move_infos: Some(vec![MoveInfo {
                move_coord: "D16".to_string(),
                visits: 142,
                winrate: 0.523,
                score_mean: 2.5,
                score_stdev: 8.2,
                score_lead: 2.5,
                utility: 0.031,
                utility_lcb: Some(0.025),
                lcb: 0.515,
                prior: 0.18,
                human_prior: None,
                order: 0,
                pv: Some(vec!["D16".to_string(), "Q4".to_string()]),
                pv_visits: Some(vec![142, 95]),
                ownership: None,
            }]),
            root_info: Some(RootInfo {
                winrate: 0.512,
                score_lead: 1.5,
                utility: 0.015,
                visits: 500,
                current_player: "B".to_string(),
                raw_winrate: Some(0.508),
                raw_score_mean: Some(1.2),
                raw_st_score_error: Some(8.5),
                human_winrate: None,
                human_score_mean: None,
                human_score_stdev: None,
            }),
            ownership: None,
            ownership_stdev: None,
            policy: None,
            human_policy: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"turnNumber\":5"));
        assert!(json.contains("\"moveCoord\":\"D16\""));
        assert!(json.contains("\"winrate\":0.523"));
    }

    #[test]
    fn test_version_response_serialization() {
        let response = VersionResponse {
            server: ServerVersion {
                name: "katago-server".to_string(),
                version: "1.0.0".to_string(),
            },
            katago: Some(KatagoVersion {
                version: "1.15.3".to_string(),
                git_hash: Some("abc123".to_string()),
            }),
            model: ModelInfo {
                name: "kata1-b18c384nbt-s12345.bin.gz".to_string(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"name\":\"katago-server\""));
        assert!(json.contains("\"version\":\"1.0.0\""));
        assert!(json.contains("\"gitHash\":\"abc123\""));
        assert!(json.contains("\"kata1-b18c384nbt-s12345.bin.gz\""));
    }

    #[test]
    fn test_validate_analyze_turns_defaults_to_final_position() {
        let json = r#"{
            "moves": ["D4", "Q16"]
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        assert_eq!(validate_analyze_turns(&request).unwrap(), 1);
    }

    #[test]
    fn test_validate_analyze_turns_rejects_duplicates() {
        let json = r#"{
            "moves": ["D4", "Q16"],
            "analyzeTurns": [0, 2, 2]
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        let err = validate_analyze_turns(&request).unwrap_err();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn test_validate_analyze_turns_rejects_out_of_range_turns() {
        let json = r#"{
            "moves": ["D4", "Q16"],
            "analyzeTurns": [1, 3]
        }"#;
        let request: AnalysisRequest = serde_json::from_str(json).unwrap();
        let err = validate_analyze_turns(&request).unwrap_err();
        assert!(err.contains("out-of-range"));
    }

    #[test]
    fn test_problem_detail_serialization() {
        let problem = ProblemDetail {
            problem_type: "https://katago-server/problems/timeout".to_string(),
            title: "Analysis Timeout".to_string(),
            status: 504,
            detail: "KataGo analysis timed out after 20 seconds".to_string(),
            instance: Some("/api/v1/analysis".to_string()),
            request_id: Some("req-123".to_string()),
        };

        let json = serde_json::to_string(&problem).unwrap();
        assert!(json.contains("\"type\":\"https://katago-server/problems/timeout\""));
        assert!(json.contains("\"status\":504"));
        assert!(json.contains("\"requestId\":\"req-123\""));
    }
}
