//! NVR — Network Video Recorder
//!
//! Usage:
//!   nvr record --config config.toml        # start recording all cameras
//!   nvr status --config config.toml        # print status
//!   nvr list   --config config.toml --camera cam1

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use nvr::config::Config;
use nvr::manager::RecordingManager;
use nvr::storage::chunk_pool::ChunkPool;

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
        /// Path to the TOML configuration file.
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// Print a brief status snapshot and exit.
    Status {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// List recorded segments for a camera.
    List {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        /// Camera ID to list segments for.
        #[arg(long)]
        camera: String,
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

    let manager = match RecordingManager::new(cfg) {
        Ok(m) => m,
        Err(e) => {
            error!(error = %e, "Failed to start recording manager");
            std::process::exit(1);
        }
    };

    // Wait for CTRL+C.
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            info!("Received CTRL+C, shutting down…");
        }
        Err(e) => {
            error!(error = %e, "Signal error");
        }
    }

    manager.shutdown();
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
            println!("=== NVR Status ===");
            println!("Pool files  : {}", cfg.storage.max_pools);
            println!("Pool size   : {} MB each", cfg.storage.chunk_size_mb);
            println!(
                "Active pool : pool_{:03}.bin  ({:.1}% full)",
                idx,
                (used as f64 / cap as f64) * 100.0
            );
            println!("Cameras     : {}", cfg.cameras.len());
            for cam in &cfg.cameras {
                println!("  {} ({}): {}", cam.id, cam.name, cam.url);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn run_list(config_path: PathBuf, camera_id: &str) {
    let _cfg = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to load config");
            std::process::exit(1);
        }
    };

    // Note: without a running writer, the in-memory index is empty.
    // In a production system, the index would be persisted to disk.
    println!("Note: segment listing requires a running NVR instance.");
    println!("To list segments, start with `nvr record` first.");
    println!(
        "Camera: {} — index not available in offline mode",
        camera_id
    );
}
