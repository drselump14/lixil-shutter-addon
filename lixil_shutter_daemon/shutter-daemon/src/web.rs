use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::ble::DiscoveredDevice;
use crate::shutter::ShutterManager;
use crate::supervisor;

/// The ingress panel's HTTP port, set in config.yaml's `ingress_port` to
/// match. Only reachable through the Supervisor's ingress proxy (bound to
/// 0.0.0.0 here since that proxy, not a browser, is the direct caller), not
/// exposed to the LAN.
pub const INGRESS_PORT: u16 = 8099;

const SCAN_DURATION: Duration = Duration::from_secs(10);

const INDEX_HTML: &str = include_str!("../assets/ingress.html");

#[derive(Clone)]
struct AppState {
    manager: Arc<ShutterManager>,
}

pub async fn serve(manager: Arc<ShutterManager>) {
    let state = AppState { manager };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/shutters", get(list_shutters).post(add_shutter))
        .route("/api/scan", post(scan))
        .with_state(state);

    let addr = format!("0.0.0.0:{INGRESS_PORT}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(addr, error = %e, "failed to bind ingress web server");
            return;
        }
    };
    info!(addr, "ingress web server listening");
    if let Err(e) = axum::serve(listener, app).await {
        error!(error = %e, "ingress web server stopped");
    }
}

async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], Html(INDEX_HTML))
}

#[derive(Serialize)]
struct ConfiguredShutterJson {
    id: String,
    name: String,
    mac: String,
}

async fn list_shutters(State(state): State<AppState>) -> Json<Vec<ConfiguredShutterJson>> {
    Json(
        state
            .manager
            .configured()
            .into_iter()
            .map(|s| ConfiguredShutterJson {
                id: s.id,
                name: s.name,
                mac: s.mac,
            })
            .collect(),
    )
}

#[derive(Serialize)]
struct ScanResponse {
    devices: Vec<DiscoveredDevice>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Runs one blocking (from the caller's point of view) scan cycle and
/// returns whatever shutter-shaped devices turned up. The frontend calls
/// this after the user has pressed the physical pairing-mode button on the
/// shutter they want to add.
async fn scan(State(state): State<AppState>) -> impl IntoResponse {
    let known_macs: Vec<String> = state.manager.configured().into_iter().map(|s| s.mac).collect();

    // Hold the same adapter-wide lock connect/pair uses (see shutter.rs and
    // ble/connection.rs) so a scan never runs concurrently with a connect
    // attempt on another shutter -- both drive the one physical adapter.
    let radio_lock = state.manager.radio_lock();
    let _radio = radio_lock.lock().await;

    match crate::ble::discover_new(&known_macs, SCAN_DURATION).await {
        Ok(devices) => Json(ScanResponse { devices }).into_response(),
        Err(e) => {
            warn!(error = %e, "scan for new shutters failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e.to_string() }),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct AddShutterRequest {
    id: String,
    name: String,
    mac: String,
}

#[derive(Serialize)]
struct AddShutterResponse {
    restarting: bool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Adds a newly-picked shutter to the add-on's own config via the
/// Supervisor API, then restarts the daemon so `ShutterManager` picks it up
/// -- it's only ever built once, from `AddonConfig::load`, at startup (see
/// main.rs), so there's no in-process way to add a shutter without one.
async fn add_shutter(State(state): State<AppState>, Json(req): Json<AddShutterRequest>) -> impl IntoResponse {
    if !valid_id(&req.id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "id must be lowercase letters, digits and underscores only".into(),
            }),
        )
            .into_response();
    }
    if req.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "name must not be empty".into() }),
        )
            .into_response();
    }
    if state.manager.configured().iter().any(|s| s.id == req.id) {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse { error: format!("shutter id '{}' already exists", req.id) }),
        )
            .into_response();
    }

    if let Err(e) = supervisor::add_shutter(&req.id, &req.name, &req.mac).await {
        error!(error = %e, "failed to save new shutter via Supervisor API");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response();
    }

    info!(id = %req.id, mac = %req.mac, "saved new shutter, restarting daemon to apply it");
    tokio::spawn(async {
        // Give the HTTP response below time to flush through the ingress
        // proxy before the restart kills this process.
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Err(e) = supervisor::restart_self().await {
            error!(error = %e, "failed to restart add-on after saving new shutter");
        }
    });

    Json(AddShutterResponse { restarting: true }).into_response()
}
