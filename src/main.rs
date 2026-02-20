// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! NVR — Network Video Recorder
//!
//! Usage:
//!   nvr record --config config.toml
//!   nvr status --config config.toml
//!   nvr list   --config config.toml --camera cam1
//!   nvr export --config config.toml --camera cam1 --from "2026-02-19T14:00:00" --to "2026-02-19T15:00:00" -o output.ts

use std::path::PathBuf;

use chrono::NaiveDateTime;
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use nvr::api;
use nvr::config::Config;
use nvr::manager::RecordingManager;
use nvr::playback;
use nvr::storage::chunk_pool::ChunkPool;
use nvr::storage::index::SegmentIndex;

#[derive(Parser)]
#[command(name = "nvr", about = "Network Video Recorder", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start recording all configured cameras.
    Record {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// Print a brief status snapshot and exit.
    Status {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// List recorded segments for a camera (scanned from pool files).
    List {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        /// Camera ID to list segments for.
        #[arg(long)]
        camera: String,
    },
    /// Export recorded video for a camera in a time range to a .ts file.
    Export {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        /// Camera ID.
        #[arg(long)]
        camera: String,
        /// Start time (local), e.g. "2026-02-19T14:00:00"
        #[arg(long)]
        from: String,
        /// End time (local), e.g. "2026-02-19T15:00:00"
        #[arg(long)]
        to: String,
        /// Output file path (default: export.ts)
        #[arg(short, long, default_value = "export.ts")]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Record { config } => {
            run_record(config).await;
        }
        Command::Status { config } => {
            run_status(config);
        }
        Command::List { config, camera } => {
            run_list(config, &camera);
        }
        Command::Export { config, camera, from, to, output } => {
            run_export(config, &camera, &from, &to, &output);
        }
    }
}

async fn run_record(config_path: PathBuf) {
    let cfg = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    info!(
        cameras = cfg.cameras.len(),
        base_path = ?cfg.storage.base_path,
        pool_size_mb = cfg.storage.chunk_size_mb,
        max_pools = cfg.storage.max_pools,
        segment_secs = cfg.storage.segment_duration_secs,
        "Starting NVR"
    );

    let manager = match RecordingManager::new(cfg.clone()) {
        Ok(m) => m,
        Err(e) => {
            error!(error = %e, "Failed to start recording manager");
            std::process::exit(1);
        }
    };

    let manager = std::sync::Arc::new(parking_lot::Mutex::new(manager));

    // Start HTTP API if enabled.
    if cfg.api.enabled {
        let state = std::sync::Arc::new(api::AppState {
            index: {
                let mgr = manager.lock();
                mgr.index.clone()
            },
            config: cfg.clone(),
            read_counters: {
                let mgr = manager.lock();
                mgr.read_counters.clone()
            },
            manager: manager.clone(),
        });
        let port = cfg.api.port;
        tokio::spawn(async move {
            api::start_server(state, port).await;
        });
    }

    // Wait for CTRL+C.
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            info!("Received CTRL+C, shutting down…");
        }
        Err(e) => {
            error!(error = %e, "Signal error");
        }
    }

    match std::sync::Arc::try_unwrap(manager) {
        Ok(mutex) => mutex.into_inner().shutdown(),
        Err(_arc) => {
            // Other references still held (API server); force shutdown via lock.
            warn!("Forcing shutdown while API still holds references");
            // Can't call shutdown() without ownership, but workers are aborted
            // when the process exits anyway.
        }
    }
}

fn run_status(config_path: PathBuf) {
    let cfg = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    let pool_bytes = cfg.storage.chunk_size_mb * 1024 * 1024;
    match ChunkPool::open(&cfg.storage.base_path, pool_bytes, cfg.storage.max_pools) {
        Ok(pool) => {
            let (idx, used, cap) = pool.status();
            let records = pool.scan_all_pools().unwrap_or_default();
            println!("=== NVR Status ===");
            println!("Pool files  : {}", cfg.storage.max_pools);
            println!("Pool size   : {} MB each", cfg.storage.chunk_size_mb);
            println!(
                "Active pool : pool_{:03}.bin  ({:.1}% full)",
                idx,
                (used as f64 / cap as f64) * 100.0
            );
            println!("Segments    : {}", records.len());
            println!("Cameras     : {}", cfg.cameras.len());
            for cam in &cfg.cameras {
                let cam_segs = records.iter().filter(|r| r.camera_id == cam.id).count();
                println!("  {} ({}): {} — {} segments", cam.id, cam.name, cam.url, cam_segs);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn run_list(config_path: PathBuf, camera_id: &str) {
    let cfg = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    let pool_bytes = cfg.storage.chunk_size_mb * 1024 * 1024;
    let pool = match ChunkPool::open(&cfg.storage.base_path, pool_bytes, cfg.storage.max_pools) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Rebuild index from pools.
    let records = pool.scan_all_pools().unwrap_or_default();
    let mut index = SegmentIndex::new();
    index.rebuild_from_scanned(records);

    let segments = index.segments_for_camera(camera_id);
    if segments.is_empty() {
        println!("No segments found for camera '{}'", camera_id);
        return;
    }

    println!("=== Segments for camera '{}' ===", camera_id);
    println!("{:<6} {:<24} {:<24} {:<10} {:<8}", "ID", "Start", "End", "Pool", "Size");
    println!("{}", "-".repeat(76));
    for seg in &segments {
        let size_kb = (seg.location.record_size - 40) / 1024; // subtract header
        println!(
            "{:<6} {:<24} {:<24} pool_{:03}   {} KB",
            seg.segment_id,
            seg.start_ts.format("%Y-%m-%d %H:%M:%S"),
            seg.end_ts.format("%Y-%m-%d %H:%M:%S"),
            seg.location.pool_idx,
            size_kb,
        );
    }
    println!("\nTotal: {} segments", segments.len());
}

fn run_export(config_path: PathBuf, camera_id: &str, from: &str, to: &str, output: &PathBuf) {
    let cfg = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    // Parse timestamps.
    let from_naive = match NaiveDateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            eprintln!("Invalid --from timestamp '{}': {}", from, e);
            eprintln!("Expected format: 2026-02-19T14:00:00");
            std::process::exit(1);
        }
    };
    let to_naive = match NaiveDateTime::parse_from_str(to, "%Y-%m-%dT%H:%M:%S") {
        Ok(dt) => dt,
        Err(e) => {
            eprintln!("Invalid --to timestamp '{}': {}", to, e);
            eprintln!("Expected format: 2026-02-19T15:00:00");
            std::process::exit(1);
        }
    };

    let from_utc = from_naive.and_utc();
    let to_utc = to_naive.and_utc();

    // Open pool and rebuild index.
    let pool_bytes = cfg.storage.chunk_size_mb * 1024 * 1024;
    let pool = match ChunkPool::open(&cfg.storage.base_path, pool_bytes, cfg.storage.max_pools) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error opening pool: {e}");
            std::process::exit(1);
        }
    };

    let records = pool.scan_all_pools().unwrap_or_default();
    let mut index = SegmentIndex::new();
    index.rebuild_from_scanned(records);

    // Export.
    match playback::export_range(&pool, &index, camera_id, from_utc, to_utc, output) {
        Ok(count) => {
            println!(
                "Exported {} segments for camera '{}' → {}",
                count,
                camera_id,
                output.display()
            );
        }
        Err(e) => {
            eprintln!("Export failed: {e}");
            std::process::exit(1);
        }
    }
}
