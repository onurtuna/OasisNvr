// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! HTTP API — runs alongside the recording process.
//!
//! Endpoints:
//!   GET    /api/status                                → system status (JSON)
//!   GET    /api/list?camera=cam1                      → segment list (JSON)
//!   GET    /api/export?camera=cam1&from=...&to=...    → download .ts
//!   GET    /api/hls/{camera}/live.m3u8                → LL-HLS live playlist
//!   GET    /api/hls/{camera}/vod.m3u8?from=...&to=... → VOD playlist
//!   GET    /api/cameras                               → list active cameras
//!   POST   /api/cameras                               → add camera (hot)
//!   DELETE /api/cameras/{id}                          → remove camera (hot)

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::Router;
use chrono::NaiveDateTime;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use crate::config::{CameraConfig, Config};
use crate::hls;
use crate::manager::RecordingManager;
use crate::storage::chunk_pool::{ChunkPool, PoolReadCounters};
use crate::storage::index::SegmentIndex;

/// Shared state passed to all handlers.
pub struct AppState {
    pub index: Arc<RwLock<SegmentIndex>>,
    pub config: std::sync::Arc<std::sync::RwLock<Config>>,
    pub config_path: std::path::PathBuf,
    pub read_counters: Arc<PoolReadCounters>,
    pub manager: Arc<Mutex<RecordingManager>>,
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

#[derive(Deserialize)]
pub struct LoginParams {
    username: String,
    password: String,
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

use tower_http::services::ServeDir;

/// Build the axum router.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/status", get(handle_status))
        .route("/api/list", get(handle_list))
        .route("/api/export", get(handle_export))
        // HLS endpoints
        .route("/api/hls/{camera_id}/live.m3u8", get(handle_hls_live))
        .route("/api/hls/{camera_id}/vod.m3u8", get(handle_hls_vod))
        .route("/api/hls/{camera_id}/segment/ts/{segment_id}", get(handle_hls_segment))
        .route("/api/hls/{camera_id}/player", get(handle_hls_player))
        .route("/api/hls/{camera_id}/vod/player", get(handle_vod_player))
        // Camera management
        .route("/api/cameras", get(handle_list_cameras).post(handle_add_camera))
        .route("/api/cameras/{camera_id}", delete(handle_remove_camera))
        // Authentication
        .route("/api/login", axum::routing::post(handle_login))
        // Serve static frontend files
        .fallback_service(ServeDir::new("frontend"))
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

async fn handle_login(
    State(state): State<Arc<AppState>>,
    axum::Json(params): axum::Json<LoginParams>,
) -> impl IntoResponse {
    let cfg = state.config.read().unwrap();
    if params.username == cfg.api.username && params.password == cfg.api.password {
        (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "token": "oasis_logged_in" })),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "error": "Invalid username or password" })),
        )
    }
}

async fn handle_status(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let pool_guard = {
        let mgr = state.manager.lock();
        mgr.pool.clone()
    };
    
    let (idx, used, cap) = {
        let p = pool_guard.read();
        p.status()
    };
    let index = state.index.read();

    let cameras: Vec<CameraStatus> = {
        let cfg = state.config.read().unwrap();
        cfg.cameras
            .iter()
            .map(|c| CameraStatus {
                id: c.id.clone(),
                name: c.name.clone(),
                segments: index.segments_for_camera(&c.id).len(),
            })
            .collect()
    };

    let cfg = state.config.read().unwrap();
    let resp = StatusResponse {
        pool_files: cfg.storage.max_pools,
        pool_size_mb: cfg.storage.chunk_size_mb,
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
    let pool_bytes = {
        let cfg = state.config.read().unwrap();
        cfg.storage.chunk_size_mb * 1024 * 1024
    };
    let base_path = state.config.read().unwrap().storage.base_path.clone();
    let max_pools = state.config.read().unwrap().storage.max_pools;
    
    let pool = match ChunkPool::open(
        &base_path,
        pool_bytes,
        max_pools,
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
    let seg_dur = state.config.read().unwrap().storage.segment_duration_secs;

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

    let seg_dur = state.config.read().unwrap().storage.segment_duration_secs;
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

/// Inline HLS.js web player — works in all browsers.
async fn handle_hls_player(
    Path(camera_id): Path<String>,
) -> impl IntoResponse {
    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>NVR — {camera_id}</title>
<script src="https://cdn.jsdelivr.net/npm/hls.js@1"></script>
<style>
  * {{ margin:0; padding:0; box-sizing:border-box; }}
  body {{ background:#111; display:flex; flex-direction:column;
         align-items:center; justify-content:center; min-height:100vh;
         font-family:system-ui,sans-serif; color:#eee; }}
  h1 {{ font-size:1.2rem; margin-bottom:12px; opacity:.7; }}
  video {{ width:90vw; max-width:1280px; border-radius:8px;
           background:#000; }}
  #status {{ font-size:.85rem; margin-top:8px; opacity:.5; }}
</style>
</head>
<body>
<h1>📹 {camera_id}</h1>
<video id="v" controls autoplay muted playsinline></video>
<div id="status">Connecting…</div>
<script>
const src = "live.m3u8";
const video = document.getElementById("v");
const status = document.getElementById("status");

if (Hls.isSupported()) {{
  const hls = new Hls({{
    liveSyncDurationCount: 3,
    liveMaxLatencyDurationCount: 6,
    enableWorker: true,
  }});
  hls.loadSource(src);
  hls.attachMedia(video);
  hls.on(Hls.Events.MANIFEST_PARSED, () => {{
    status.textContent = "Playing (HLS.js)";
    video.play().catch(() => {{}});
  }});
  hls.on(Hls.Events.ERROR, (_, data) => {{
    status.textContent = "Error: " + data.details;
    if (data.fatal) {{
      if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {{
        status.textContent += " — retrying…";
        setTimeout(() => hls.startLoad(), 3000);
      }}
    }}
  }});
}} else if (video.canPlayType("application/vnd.apple.mpegurl")) {{
  // Safari native HLS
  video.src = src;
  video.addEventListener("loadedmetadata", () => {{
    status.textContent = "Playing (native)";
    video.play().catch(() => {{}});
  }});
}} else {{
  status.textContent = "HLS not supported in this browser";
}}
</script>
</body>
</html>"#, camera_id = camera_id);

    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
}

/// VOD web player — pass ?from=...&to=... query params.
async fn handle_vod_player(
    Path(camera_id): Path<String>,
    raw_query: axum::extract::RawQuery,
) -> impl IntoResponse {
    let qs = raw_query.0.unwrap_or_default();
    let html = format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>NVR VOD — {camera_id}</title>
<script src="https://cdn.jsdelivr.net/npm/hls.js@1"></script>
<style>
  * {{ margin:0; padding:0; box-sizing:border-box; }}
  body {{ background:#111; display:flex; flex-direction:column;
         align-items:center; justify-content:center; min-height:100vh;
         font-family:system-ui,sans-serif; color:#eee; }}
  h1 {{ font-size:1.2rem; margin-bottom:12px; opacity:.7; }}
  video {{ width:90vw; max-width:1280px; border-radius:8px;
           background:#000; }}
  #status {{ font-size:.85rem; margin-top:8px; opacity:.5; }}
</style>
</head>
<body>
<h1>🎬 {camera_id} — VOD</h1>
<video id="v" controls autoplay muted playsinline></video>
<div id="status">Loading…</div>
<script>
const src = "../vod.m3u8?{qs}";
const video = document.getElementById("v");
const status = document.getElementById("status");

if (Hls.isSupported()) {{
  const hls = new Hls({{ 
    enableWorker: true,
    startFragPrefetch: true
  }});
  hls.loadSource(src);
  hls.attachMedia(video);
  hls.on(Hls.Events.MANIFEST_PARSED, () => {{
    status.textContent = "Playing (HLS.js)";
    video.play().catch(() => {{}});
  }});
  hls.on(Hls.Events.ERROR, (_, data) => {{
    status.textContent = "Error: " + data.details;
  }});
}} else if (video.canPlayType("application/vnd.apple.mpegurl")) {{
  video.src = src;
  video.addEventListener("loadedmetadata", () => {{
    status.textContent = "Playing (native)";
    video.play().catch(() => {{}});
  }});
}} else {{
  status.textContent = "HLS not supported in this browser";
}}
</script>
</body>
</html>"#, camera_id = camera_id, qs = qs);

    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
}

/// Serve a single segment's raw MPEG-TS data by segment_id.
async fn handle_hls_segment(
    State(state): State<Arc<AppState>>,
    Path((camera_id, segment_id)): Path<(String, u64)>,
) -> impl IntoResponse {
    // Find the segment in the index.
    let seg = {
        let idx = state.index.read();
        idx.segments_for_camera(&camera_id)
            .into_iter()
            .find(|s| s.segment_id == segment_id)
            .cloned()
    };

    let seg = match seg {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                [("content-type", "text/plain")],
                Vec::from("Segment not found".as_bytes()),
            ).into_response();
        }
    };

    // Read segment data from pool.
    let pool_guard = {
        let mgr = state.manager.lock();
        mgr.pool.clone()
    };

    let p = pool_guard.read();

    // Acquire read guard to prevent pool rotation during read.
    let _guard = state.read_counters.acquire(seg.location.pool_idx);

    match p.read_segment_data(&seg.location) {
        Ok(data) => (
            StatusCode::OK,
            [("content-type", "video/mp2t")],
            data,
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "text/plain")],
            Vec::from(format!("Read error: {e}").as_bytes()),
        ).into_response(),
    }
}

// ──────────────── camera management handlers ─────────────────────────────

/// List all active and historical cameras.
async fn handle_list_cameras(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mgr = state.manager.lock();
    let active_cameras = mgr.list_cameras();
    
    // Hash map to check currently active ones
    use std::collections::HashSet;
    let mut active_ids = HashSet::new();
    
    let mut list: Vec<serde_json::Value> = active_cameras
        .iter()
        .map(|c| {
            active_ids.insert(c.id.clone());
            serde_json::json!({
                "id": c.id,
                "name": c.name,
                "url": c.url,
                "status": "active"
            })
        })
        .collect();
    drop(mgr);

    // Merge historically recorded cameras from SegmentIndex
    let index = state.index.read();
    let historical_cameras = index.cameras(); // returns Vec<String>
    for cam_id in historical_cameras {
        if !active_ids.contains(&cam_id) {
            list.push(serde_json::json!({
                "id": cam_id,
                "name": format!("Removed: {}", cam_id),
                "url": "",
                "status": "offline"
            }));
        }
    }

    (StatusCode::OK, axum::Json(serde_json::json!({
        "cameras": list,
        "total": list.len(),
    })))
}

/// Add a camera at runtime.
async fn handle_add_camera(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<CameraConfig>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock();
    match mgr.add_camera(body.clone()) {
        Ok(()) => {
            // Update Config in memory and save to file
            let mut cfg = state.config.write().unwrap();
            cfg.cameras.push(body.clone());
            if let Err(e) = cfg.save_to_file(&state.config_path) {
                error!("Failed to save config to toml: {}", e);
            }

            (
                StatusCode::CREATED,
                axum::Json(serde_json::json!({
                    "status": "added",
                    "camera": { "id": body.id, "name": body.name, "url": body.url }
                })),
            )
        },
        Err(e) => (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// Remove a camera at runtime.
async fn handle_remove_camera(
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<String>,
) -> impl IntoResponse {
    let mut mgr = state.manager.lock();
    if mgr.remove_camera(&camera_id) {
        // Update Config in memory and save to file
        let mut cfg = state.config.write().unwrap();
        cfg.cameras.retain(|c| c.id != camera_id);
        if let Err(e) = cfg.save_to_file(&state.config_path) {
            error!("Failed to save config to toml: {}", e);
        }

        (StatusCode::OK, axum::Json(serde_json::json!({
            "status": "removed",
            "camera_id": camera_id,
        })))
    } else {
        (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({
            "error": format!("Camera '{}' not found", camera_id),
        })))
    }
}
