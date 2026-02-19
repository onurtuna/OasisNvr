//! HTTP API — runs alongside the recording process.
//!
//! Endpoints:
//!   GET  /api/status                                  → system status (JSON)
//!   GET  /api/list?camera=cam1                        → segment list (JSON)
//!   GET  /api/export?camera=cam1&from=...&to=...      → download .ts
//!   GET  /api/hls/{camera}/live.m3u8                  → LL-HLS live playlist
//!   GET  /api/hls/{camera}/live.m3u8?_HLS_msn=N       → blocking reload
//!   GET  /api/hls/{camera}/vod.m3u8?from=...&to=...   → VOD playlist
//!   GET  /api/hls/{camera}/segment/{id}.ts            → segment data

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::NaiveDateTime;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::config::Config;
use crate::hls;
use crate::storage::chunk_pool::{ChunkPool, PoolReadCounters};
use crate::storage::index::SegmentIndex;

/// Shared state passed to all handlers.
pub struct AppState {
    pub index: Arc<RwLock<SegmentIndex>>,
    pub config: Config,
    pub read_counters: Arc<PoolReadCounters>,
}

// ──────────────── request / response types ────────────────────────────────

#[derive(Deserialize)]
pub struct ListParams {
    camera: String,
}

#[derive(Deserialize)]
pub struct ExportParams {
    camera: String,
    from: String,
    to: String,
}

#[derive(Deserialize)]
pub struct VodParams {
    from: String,
    to: String,
}

#[derive(Serialize)]
struct StatusResponse {
    pool_files: usize,
    pool_size_mb: u64,
    active_pool_idx: usize,
    active_pool_pct: f64,
    total_segments: usize,
    cameras: Vec<CameraStatus>,
}

#[derive(Serialize)]
struct CameraStatus {
    id: String,
    name: String,
    segments: usize,
}

#[derive(Serialize)]
struct SegmentInfo {
    segment_id: u64,
    camera_id: String,
    start: String,
    end: String,
    pool_idx: usize,
    size_bytes: u64,
}

#[derive(Serialize)]
struct ListResponse {
    camera: String,
    segments: Vec<SegmentInfo>,
    total: usize,
}

// ──────────────── router ──────────────────────────────────────────────────

/// Build the axum router.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/status", get(handle_status))
        .route("/api/list", get(handle_list))
        .route("/api/export", get(handle_export))
        // HLS endpoints
        .route("/api/hls/{camera_id}/live.m3u8", get(handle_hls_live))
        .route("/api/hls/{camera_id}/vod.m3u8", get(handle_hls_vod))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Start the HTTP server.
pub async fn start_server(state: Arc<AppState>, port: u16) {
    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    info!(port, "HTTP API listening on http://{}", addr);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, "Failed to bind HTTP server");
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        error!(error = %e, "HTTP server error");
    }
}

// ──────────────── handlers ────────────────────────────────────────────────

async fn handle_status(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let pool_bytes = state.config.storage.chunk_size_mb * 1024 * 1024;
    let pool = match ChunkPool::open(
        &state.config.storage.base_path,
        pool_bytes,
        state.config.storage.max_pools,
    ) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    let (idx, used, cap) = pool.status();
    let index = state.index.read();

    let cameras: Vec<CameraStatus> = state
        .config
        .cameras
        .iter()
        .map(|c| CameraStatus {
            id: c.id.clone(),
            name: c.name.clone(),
            segments: index.segments_for_camera(&c.id).len(),
        })
        .collect();

    let resp = StatusResponse {
        pool_files: state.config.storage.max_pools,
        pool_size_mb: state.config.storage.chunk_size_mb,
        active_pool_idx: idx,
        active_pool_pct: if cap > 0 {
            (used as f64 / cap as f64) * 100.0
        } else {
            0.0
        },
        total_segments: index.len(),
        cameras,
    };

    (StatusCode::OK, axum::Json(serde_json::to_value(resp).unwrap()))
}

async fn handle_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let index = state.index.read();
    let segments = index.segments_for_camera(&params.camera);

    let seg_infos: Vec<SegmentInfo> = segments
        .iter()
        .map(|s| SegmentInfo {
            segment_id: s.segment_id,
            camera_id: s.camera_id.clone(),
            start: s.start_ts.format("%Y-%m-%dT%H:%M:%S").to_string(),
            end: s.end_ts.format("%Y-%m-%dT%H:%M:%S").to_string(),
            pool_idx: s.location.pool_idx,
            size_bytes: s.location.record_size - 40,
        })
        .collect();

    let total = seg_infos.len();
    let resp = ListResponse {
        camera: params.camera,
        segments: seg_infos,
        total,
    };

    (StatusCode::OK, axum::Json(serde_json::to_value(resp).unwrap()))
}

async fn handle_export(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExportParams>,
) -> impl IntoResponse {
    // Parse timestamps.
    let from_naive = match NaiveDateTime::parse_from_str(&params.from, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"error": format!("Invalid 'from': {e}. Use format: 2026-02-19T14:00:00")})),
            ).into_response();
        }
    };
    let to_naive = match NaiveDateTime::parse_from_str(&params.to, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"error": format!("Invalid 'to': {e}. Use format: 2026-02-19T15:00:00")})),
            ).into_response();
        }
    };

    let from_utc = from_naive.and_utc();
    let to_utc = to_naive.and_utc();

    // Open pool for reading.
    let pool_bytes = state.config.storage.chunk_size_mb * 1024 * 1024;
    let pool = match ChunkPool::open(
        &state.config.storage.base_path,
        pool_bytes,
        state.config.storage.max_pools,
    ) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": e.to_string()})),
            ).into_response();
        }
    };

    let index = state.index.read();
    let segments = index.segments_in_range(&params.camera, from_utc, to_utc);

    if segments.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": format!("No segments found for camera '{}' in range {} — {}", params.camera, from_utc, to_utc)
            })),
        ).into_response();
    }

    // Read and concatenate all segment data.
    // Acquire read guards on pool(s) to prevent rotation during export.
    let mut body = Vec::new();
    for seg in &segments {
        let _guard = state.read_counters.acquire(seg.location.pool_idx);
        match pool.read_segment_data(&seg.location) {
            Ok(data) => body.extend_from_slice(&data),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({"error": format!("Read error: {e}")})),
                ).into_response();
            }
        }
    }

    info!(
        camera = params.camera,
        segments = segments.len(),
        bytes = body.len(),
        "Export streamed via API"
    );

    // Return as downloadable MPEG-TS.
    let filename = format!(
        "{}_{}_to_{}.ts",
        params.camera,
        params.from.replace(':', "-"),
        params.to.replace(':', "-")
    );

    (
        StatusCode::OK,
        [
            ("content-type", "video/mp2t"),
            ("content-disposition", &format!("attachment; filename=\"{filename}\"")),
        ],
        body,
    ).into_response()
}

// ──────────────── HLS handlers ────────────────────────────────────────────

/// LL-HLS live playlist. Supports `?_HLS_msn=N` for blocking reload.
async fn handle_hls_live(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(camera_id): axum::extract::Path<String>,
    raw_query: axum::extract::RawQuery,
) -> axum::response::Response {
    let seg_dur = state.config.storage.segment_duration_secs;

    // Parse _HLS_msn from raw query string.
    let block_msn: Option<u64> = raw_query.0.as_deref().and_then(|q| {
        q.split('&')
            .find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                if k == "_HLS_msn" { v.parse().ok() } else { None }
            })
    });

    let playlist = if let Some(msn) = block_msn {
        // Blocking reload: poll until the requested MSN appears (max 30s).
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
        loop {
            // Scope the lock guard so it's dropped before .await
            let result = {
                let idx = state.index.read();
                hls::generate_live_playlist(&idx, &camera_id, seg_dur, Some(msn))
            };
            if let Some(pl) = result {
                break pl;
            }
            if tokio::time::Instant::now() >= deadline {
                let idx = state.index.read();
                break hls::generate_live_playlist(&idx, &camera_id, seg_dur, None)
                    .unwrap_or_default();
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    } else {
        let idx = state.index.read();
        hls::generate_live_playlist(&idx, &camera_id, seg_dur, None).unwrap_or_default()
    };

    (
        StatusCode::OK,
        [("content-type", "application/vnd.apple.mpegurl")],
        playlist,
    ).into_response()
}

/// VOD playlist for a time range.
async fn handle_hls_vod(
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<String>,
    Query(params): Query<VodParams>,
) -> impl IntoResponse {
    let from_naive = match NaiveDateTime::parse_from_str(&params.from, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                format!("Invalid 'from': {e}"),
            ).into_response();
        }
    };
    let to_naive = match NaiveDateTime::parse_from_str(&params.to, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                format!("Invalid 'to': {e}"),
            ).into_response();
        }
    };

    let seg_dur = state.config.storage.segment_duration_secs;
    let idx = state.index.read();
    match hls::generate_vod_playlist(
        &idx,
        &camera_id,
        from_naive.and_utc(),
        to_naive.and_utc(),
        seg_dur,
    ) {
        Some(playlist) => (
            StatusCode::OK,
            [("content-type", "application/vnd.apple.mpegurl")],
            playlist,
        ).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            format!("No segments found for camera '{}' in range", camera_id),
        ).into_response(),
    }
}
