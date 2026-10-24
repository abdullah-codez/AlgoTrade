//! Embedded Axum web server and WebSocket broadcaster.
//! Strictly bound to loopback 127.0.0.1:3000 with Host validation.
//! Never exposes private keys or credentials to the browser or logs.

use crate::audit::AuditEvent;
use crate::config::{EngineConfig, ExecutionMode, MarginMode, ProfileConfig};
use crate::engine::{EngineOrchestrator, EngineStatusSnapshot, TradePreview};
use crate::execution::{
    validate_candle_interval, CandleSnapshot, PerpAccountSummary, SignerStatus,
};
use crate::ledger::{BotCapitalSummary, ReallocationDirection};
use crate::state::EngineMode;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{error, info, warn};

pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:3000";

/// Shared state accessible by web routes.
pub struct AppState {
    pub orchestrator: Arc<EngineOrchestrator>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConfigRequest {
    pub profile: String,
    pub margin_mode: String,
    pub execution_mode: String,
    pub trailing_stop: Option<bool>,
    pub initial_seed: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ApproveEntryPayload {
    pub proposal_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RejectEntryPayload {
    pub proposal_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReallocateCapitalRequest {
    pub direction: ReallocationDirection,
    pub amount: Decimal,
}

#[derive(Debug, Deserialize)]
pub struct CandleQuery {
    pub interval: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Host header validation middleware ensuring requests only come to loopback localhost.
async fn validate_host_header(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        let host_clean = host.split(':').next().unwrap_or(host);
        if host_clean == "127.0.0.1" || host_clean == "localhost" {
            return Ok(next.run(request).await);
        }
    }
    warn!("Rejected request with invalid or external Host header");
    Err(StatusCode::FORBIDDEN)
}

/// Build the Axum router with REST endpoints, WebSocket upgrade, and static asset serving.
pub fn create_router(app_state: Arc<AppState>, web_dir: &str) -> Router {
    // Restrictive CORS: only allow local loopback
    let cors = CorsLayer::new()
        .allow_origin([
            "http://127.0.0.1:3000".parse().unwrap(),
            "http://localhost:3000".parse().unwrap(),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::ACCEPT]);

    let api_router = Router::new()
        .route("/status", get(get_status_handler))
        .route("/account", get(get_account_handler))
        .route("/capital", get(get_capital_handler))
        .route("/capital/reallocate", post(reallocate_capital_handler))
        .route("/candles", get(get_candles_handler))
        .route("/preview", get(get_preview_handler))
        .route(
            "/config",
            get(get_config_handler).post(update_config_handler),
        )
        .route("/control/start", post(control_start_handler))
        .route("/control/pause", post(control_pause_handler))
        .route("/control/stop", post(control_stop_handler))
        .route(
            "/control/emergency-flatten",
            post(control_emergency_flatten_handler),
        )
        .route(
            "/control/approve-entry",
            post(control_approve_entry_handler),
        )
        .route("/control/reject-entry", post(control_reject_entry_handler))
        .route("/performance", get(get_performance_handler))
        .route("/history", get(get_history_handler))
        .route("/logs", get(get_logs_handler))
        .route("/ws", get(ws_handler));

    Router::new()
        .nest("/api", api_router)
        .fallback_service(ServeDir::new(web_dir))
        .layer(middleware::from_fn(validate_host_header))
        .layer(cors)
        .with_state(app_state)
}

/// Launch the embedded Axum web server on 127.0.0.1:3000.
pub async fn start_web_server(
    orchestrator: Arc<EngineOrchestrator>,
    bind_addr: &str,
    web_dir: &str,
) -> Result<(), String> {
    // Enforce loopback binding strictly
    let addr: SocketAddr = bind_addr
        .parse()
        .map_err(|e| format!("Invalid bind address '{}': {}", bind_addr, e))?;

    if !addr.ip().is_loopback() {
        return Err(format!(
            "Security invariant violation: Web dashboard must bind exclusively to loopback (127.0.0.1). Attempted: {}",
            addr
        ));
    }

    let state = Arc::new(AppState { orchestrator });
    let app = create_router(state, web_dir);

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind TCP listener to {}: {}", addr, e))?;

    info!("Local Web Dashboard active at http://{}", addr);

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Web server error: {}", e))?;

    Ok(())
}

// ============================================================================
// REST Handlers
// ============================================================================

async fn get_status_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<EngineStatusSnapshot>> {
    let snapshot = state.orchestrator.get_status_snapshot().await;
    Json(ApiResponse::ok(snapshot))
}

async fn get_account_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<PerpAccountSummary>>) {
    match state
        .orchestrator
        .exchange_client
        .get_perp_account_summary(&state.orchestrator.target_account)
        .await
    {
        Ok(summary) => {
            state
                .orchestrator
                .set_signer_status(summary.signer_status)
                .await;
            (StatusCode::OK, Json(ApiResponse::ok(summary)))
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::err(e.to_string())),
        ),
    }
}

async fn get_capital_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<BotCapitalSummary>> {
    let summary = state.orchestrator.capital_ledger.read().await.summary();
    Json(ApiResponse::ok(summary))
}

async fn reallocate_capital_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ReallocateCapitalRequest>,
) -> (StatusCode, Json<ApiResponse<BotCapitalSummary>>) {
    if payload.amount <= Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::err("Reallocation amount must be positive")),
        );
    }
    match state
        .orchestrator
        .reallocate_capital(payload.direction, payload.amount)
        .await
    {
        Ok(summary) => (StatusCode::OK, Json(ApiResponse::ok(summary))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse::err(e))),
    }
}

async fn get_candles_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CandleQuery>,
) -> (StatusCode, Json<ApiResponse<Vec<CandleSnapshot>>>) {
    let interval = query.interval.unwrap_or_else(|| "1m".to_string());
    if let Err(e) = validate_candle_interval(&interval) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::err(e.to_string())),
        );
    }
    let limit = query.limit.unwrap_or(100).min(1000);
    match state.orchestrator.ensure_candles(&interval, limit).await {
        Ok(candles) => (StatusCode::OK, Json(ApiResponse::ok(candles))),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(ApiResponse::err(e))),
    }
}

async fn get_preview_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<TradePreview>>) {
    match state.orchestrator.generate_trade_preview().await {
        Ok(preview) => (StatusCode::OK, Json(ApiResponse::ok(preview))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse::err(e))),
    }
}

async fn get_config_handler(State(state): State<Arc<AppState>>) -> Json<ApiResponse<EngineConfig>> {
    let config = state.orchestrator.config.read().await.clone();
    Json(ApiResponse::ok(config))
}

async fn update_config_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<UpdateConfigRequest>,
) -> (StatusCode, Json<ApiResponse<EngineConfig>>) {
    // 1. Guard against unsafe configuration mutation while trading is active
    {
        let sm = state.orchestrator.state_machine.read().await;
        let is_active = sm.mode() == &EngineMode::Running
            || sm.mode() == &EngineMode::AwaitingConfirmation
            || sm.mode() == &EngineMode::Pausing
            || sm.position() != &crate::state::PositionState::Flat;
        if is_active {
            return (
                StatusCode::CONFLICT,
                Json(ApiResponse::err(
                    "Cannot modify configuration while trading is active. Stop trading first.",
                )),
            );
        }
    }

    // 2. Map profile string to validated profile factory
    let mut profile_config = match payload.profile.trim().to_uppercase().as_str() {
        "NORMAL" => ProfileConfig::normal(),
        "GREEDY" => ProfileConfig::greedy(),
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::err(format!(
                    "Invalid strategy profile '{}'. Must be 'NORMAL' or 'GREEDY'.",
                    other
                ))),
            );
        }
    };

    // 3. Apply trailing stop setting if provided
    if let Some(trailing_stop) = payload.trailing_stop {
        profile_config.trailing_stop_enabled = trailing_stop;
    }

    // 4. Map margin mode
    let margin_mode = match payload.margin_mode.trim().to_uppercase().as_str() {
        "ISOLATED" => MarginMode::Isolated,
        "CROSS" => MarginMode::Cross,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::err(format!(
                    "Invalid margin mode '{}'. Must be 'Isolated' or 'Cross'.",
                    other
                ))),
            );
        }
    };

    // 5. Map execution mode
    let execution_mode = match payload.execution_mode.trim().to_uppercase().as_str() {
        "AUTOMATIC" => ExecutionMode::Automatic,
        "CONFIRMATIONREQUIRED" | "CONFIRMATION" => ExecutionMode::ConfirmationRequired,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::err(format!(
                    "Invalid execution mode '{}'. Must be 'Automatic' or 'ConfirmationRequired'.",
                    other
                ))),
            );
        }
    };

    // 6. Build and validate engine configuration
    let new_config = EngineConfig {
        profile: profile_config,
        margin_mode,
        execution_mode,
        initial_seed: payload.initial_seed,
    };

    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::err(format!("Invalid configuration: {}", e))),
        );
    }

    // 7. Apply configuration and propagate persistence failures
    *state.orchestrator.config.write().await = new_config.clone();
    if let Err(e) = state.orchestrator.persist_active_state().await {
        error!("Failed to persist updated configuration: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::err(format!(
                "Failed to persist configuration to disk: {}",
                e
            ))),
        );
    }

    info!("Updated engine configuration via web dashboard.");
    (StatusCode::OK, Json(ApiResponse::ok(new_config)))
}

async fn control_start_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    let signer_st = *state.orchestrator.signer_status.read().await;
    if signer_st != SignerStatus::Active {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::err(format!(
                "Cannot start engine: agent signer is not active (status: {:?}).",
                signer_st
            ))),
        );
    }
    match state.orchestrator.start().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok("Engine started successfully".into())),
        ),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse::err(e))),
    }
}

async fn control_pause_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    match state.orchestrator.pause().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok("Engine paused".into())),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::err(e))),
    }
}

async fn control_stop_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    match state.orchestrator.stop().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok("Engine stopped".into())),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::err(e))),
    }
}

async fn control_emergency_flatten_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    let signer_st = *state.orchestrator.signer_status.read().await;
    if signer_st != SignerStatus::Active {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::err(format!(
                "Cannot execute emergency flatten: agent signer is not active (status: {:?}).",
                signer_st
            ))),
        );
    }

    match state.orchestrator.emergency_flatten().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok(
                "Emergency flatten completed and verified flat on exchange".into(),
            )),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::err(e))),
    }
}

async fn control_approve_entry_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ApproveEntryPayload>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    let signer_st = *state.orchestrator.signer_status.read().await;
    if signer_st != SignerStatus::Active {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::err(format!(
                "Cannot approve entry: agent signer is not active (status: {:?}).",
                signer_st
            ))),
        );
    }
    match state.orchestrator.approve_entry(&payload.proposal_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok("Entry approved and order placed".into())),
        ),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse::err(e))),
    }
}

async fn control_reject_entry_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RejectEntryPayload>,
) -> (StatusCode, Json<ApiResponse<String>>) {
    let reason = payload
        .reason
        .unwrap_or_else(|| "User rejected proposal".into());
    match state
        .orchestrator
        .reject_entry(&payload.proposal_id, &reason)
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::ok("Proposal rejected".into())),
        ),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiResponse::err(e))),
    }
}

async fn get_performance_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<EngineStatusSnapshot>> {
    let snapshot = state.orchestrator.get_status_snapshot().await;
    Json(ApiResponse::ok(snapshot))
}

async fn get_history_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<crate::ledger::MatchedTrade>>> {
    let ledger = state.orchestrator.fill_ledger.read().await;
    Json(ApiResponse::ok(ledger.closed_trades().to_vec()))
}

async fn get_logs_handler(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<AuditEvent>>> {
    let logs = state.orchestrator.audit_logger.recent_events();
    Json(ApiResponse::ok(logs))
}

// ============================================================================
// WebSocket Broadcaster Handler
// ============================================================================

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws_stream(socket, state))
}

async fn handle_ws_stream(mut socket: WebSocket, state: Arc<AppState>) {
    let mut audit_rx = state.orchestrator.audit_logger.subscribe();
    let mut candle_rx = state.orchestrator.subscribe_candles();
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        tokio::select! {
            // 1. Periodically push status snapshot (every 500ms)
            _ = interval.tick() => {
                let snapshot = state.orchestrator.get_status_snapshot().await;
                if let Ok(json) = serde_json::to_string(&snapshot) {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
            // 2. Push real-time audit log events as they occur
            Ok(event) = audit_rx.recv() => {
                if let Ok(json) = serde_json::to_string(&event) {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
            // 3. Push real-time candle update events as they occur
            Ok(candle) = candle_rx.recv() => {
                let msg = json!({
                    "type": "candle_update",
                    "candle": candle,
                });
                if let Ok(json) = serde_json::to_string(&msg) {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
            // 4. Handle messages from client (pings, disconnects)
            msg = socket.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
