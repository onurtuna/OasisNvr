//! Recording manager: orchestrates global writer, all camera workers, and the
//! shared segment index.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::info;

use crate::config::Config;
use crate::error::{NvrError, Result};
use crate::ingestion::CameraWorker;
use crate::storage::chunk_pool::{ChunkPool, PoolReadCounters};
use crate::storage::global_writer::{self, SharedIndex, WriteRequest};

/// Top-level manager.
pub struct RecordingManager {
    /// Per-camera worker handles.
    worker_handles: Vec<(String, JoinHandle<()>)>,
    /// Global writer task handle.
    writer_handle: JoinHandle<()>,
    /// Shared index for status / listing.
    pub index: SharedIndex,
    /// Shared pool reader counters for safe reads.
    pub read_counters: Arc<PoolReadCounters>,
    /// Keep the sender alive so the writer doesn't shut down prematurely.
    _writer_tx: mpsc::Sender<WriteRequest>,
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

        // Spawn the single global writer.
        let (writer_tx, index, read_counters, writer_handle) =
            global_writer::spawn_writer(pool, config.storage.writer_queue_size);

        info!(
            pools = config.storage.max_pools,
            pool_size_mb = config.storage.chunk_size_mb,
            queue = config.storage.writer_queue_size,
            "Global chunk writer started"
        );

        // Spawn one CameraWorker per camera, all sharing writer_tx.
        let mut worker_handles = Vec::new();
        for cam_cfg in &config.cameras {
            let worker = CameraWorker::new(cam_cfg.id.clone(), writer_tx.clone());
            let handle = worker.spawn(cam_cfg.clone(), segment_dur);
            info!(camera = cam_cfg.id, name = cam_cfg.name, "Camera registered");
            worker_handles.push((cam_cfg.id.clone(), handle));
        }

        Ok(RecordingManager {
            worker_handles,
            writer_handle,
            index,
            read_counters,
            _writer_tx: writer_tx,
        })
    }

    /// Gracefully abort all workers and the writer. Called on shutdown.
    pub fn shutdown(self) {
        info!("NVR shutting down…");
        for (id, handle) in self.worker_handles {
            handle.abort();
            info!(camera = id, "Worker aborted");
        }
        // Drop the sender so the writer loop exits.
        drop(self._writer_tx);
        self.writer_handle.abort();
        info!("Global writer stopped");
    }
}
