// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Recording manager: orchestrates global writer, all camera workers, and the
//! shared segment index.
//!
//! Supports dynamic camera add/remove at runtime via `add_camera()` and
//! `remove_camera()`.

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::{CameraConfig, Config};
use crate::error::{NvrError, Result};
use crate::ingestion::CameraWorker;
use crate::storage::chunk_pool::{ChunkPool, PoolReadCounters};
use crate::storage::global_writer::{self, SharedIndex, WriteRequest};

/// Top-level manager.
pub struct RecordingManager {
    /// Per-camera worker handles, keyed by camera ID.
    workers: HashMap<String, WorkerEntry>,
    /// Global writer task handle.
    writer_handle: JoinHandle<()>,
    /// Shared index for status / listing.
    pub index: SharedIndex,
    /// Shared pool reader counters for safe reads.
    pub read_counters: Arc<PoolReadCounters>,
    /// Global shared pool reference.
    pub pool: Arc<RwLock<ChunkPool>>,
    /// Channel sender — cloned to each new camera worker.
    writer_tx: mpsc::Sender<WriteRequest>,
    /// Segment duration used when spawning new workers.
    segment_duration: Duration,
}

struct WorkerEntry {
    config: CameraConfig,
    handle: JoinHandle<()>,
}

impl RecordingManager {
    /// Create the manager from a validated [`Config`].
    pub fn new(config: Config) -> Result<Self> {
        let base = &config.storage.base_path;
        std::fs::create_dir_all(base)
            .map_err(|e| NvrError::Storage(format!("Cannot create base_path: {e}")))?;

        let pool_bytes = config.storage.chunk_size_mb * 1024 * 1024;
        let segment_dur = Duration::from_secs(config.storage.segment_duration_secs);

        // Open the global chunk pool.
        let pool = ChunkPool::open(base, pool_bytes, config.storage.max_pools)?;
        let read_counters = pool.read_counters.clone();
        let shared_pool = Arc::new(RwLock::new(pool));

        // Spawn the single global writer.
        let (writer_tx, index, writer_handle) =
            global_writer::spawn_writer(shared_pool.clone(), config.storage.writer_queue_size);

        info!(
            pools = config.storage.max_pools,
            pool_size_mb = config.storage.chunk_size_mb,
            queue = config.storage.writer_queue_size,
            "Global chunk writer started"
        );

        // Spawn one CameraWorker per camera, all sharing writer_tx.
        let mut workers = HashMap::new();
        for cam_cfg in &config.cameras {
            let worker = CameraWorker::new(cam_cfg.id.clone(), writer_tx.clone());
            let handle = worker.spawn(cam_cfg.clone(), segment_dur);
            info!(camera = cam_cfg.id, name = cam_cfg.name, "Camera registered");
            workers.insert(cam_cfg.id.clone(), WorkerEntry {
                config: cam_cfg.clone(),
                handle,
            });
        }

        Ok(RecordingManager {
            workers,
            writer_handle,
            index,
            read_counters,
            pool: shared_pool,
            writer_tx,
            segment_duration: segment_dur,
        })
    }

    /// Add a new camera at runtime. Returns an error if the ID already exists.
    pub fn add_camera(&mut self, cam_cfg: CameraConfig) -> Result<()> {
        if self.workers.contains_key(&cam_cfg.id) {
            return Err(NvrError::Config(format!(
                "Camera '{}' already exists", cam_cfg.id
            )));
        }

        let worker = CameraWorker::new(cam_cfg.id.clone(), self.writer_tx.clone());
        let handle = worker.spawn(cam_cfg.clone(), self.segment_duration);
        info!(camera = cam_cfg.id, name = cam_cfg.name, "Camera added (hot)");

        self.workers.insert(cam_cfg.id.clone(), WorkerEntry {
            config: cam_cfg,
            handle,
        });
        Ok(())
    }

    /// Remove a camera at runtime. Aborts the worker task.
    pub fn remove_camera(&mut self, camera_id: &str) -> bool {
        if let Some(entry) = self.workers.remove(camera_id) {
            entry.handle.abort();
            info!(camera = camera_id, "Camera removed (hot)");
            true
        } else {
            warn!(camera = camera_id, "Camera not found for removal");
            false
        }
    }

    /// List currently active cameras.
    pub fn list_cameras(&self) -> Vec<&CameraConfig> {
        self.workers.values().map(|e| &e.config).collect()
    }

    /// Gracefully abort all workers and the writer. Called on shutdown.
    pub fn shutdown(self) {
        info!("NVR shutting down…");
        for (id, entry) in self.workers {
            entry.handle.abort();
            info!(camera = id, "Worker aborted");
        }
        drop(self.writer_tx);
        self.writer_handle.abort();
        info!("Global writer stopped");
    }
}
