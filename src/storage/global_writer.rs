//! Global chunk writer — single async task that serialises all camera segment
//! writes into one sequential I/O stream.
//!
//! ```text
//! cam1_worker ─┐
//! cam2_worker ─┤       mpsc
//! cam3_worker ─┼────→ channel ────→  GlobalChunkWriter task
//! ...          ─┘                         │
//!                                         ▼
//!                              pool_000.bin, pool_001.bin …
//!                                         │
//!                                         ▼
//!                                   SegmentIndex (in-memory)
//! ```
//!
//! Each camera worker sends a [`WriteRequest`] through a bounded `mpsc`
//! channel. The writer drains the channel in order and appends records to the
//! current pool file, rotating when full.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::storage::chunk_pool::ChunkPool;
use crate::storage::index::SegmentIndex;

/// Payload sent by camera workers to the global writer.
#[derive(Debug)]
pub struct WriteRequest {
    pub camera_id: String,
    pub start_ts: DateTime<Utc>,
    pub end_ts: DateTime<Utc>,
    pub data: Vec<u8>,
}

/// Shared handle through which workers and the CLI can query the index.
pub type SharedIndex = Arc<RwLock<SegmentIndex>>;

/// Create the writer channel and spawn the writer task.
///
/// On startup the pool files are scanned sequentially to rebuild the
/// in-memory segment index from existing RecordHeaders. This makes
/// the index persistent across restarts — no separate index file needed.
///
/// Returns:
///   - `mpsc::Sender<WriteRequest>` — hand out clones to each camera worker.
///   - `SharedIndex` — read-only handle for status / listing.
///   - `JoinHandle` for the writer task.
pub fn spawn_writer(
    pool: ChunkPool,
    channel_bound: usize,
) -> (
    mpsc::Sender<WriteRequest>,
    SharedIndex,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::channel::<WriteRequest>(channel_bound);
    let index = Arc::new(RwLock::new(SegmentIndex::new()));
    let idx_clone = index.clone();

    let handle = tokio::spawn(async move {
        writer_loop(pool, rx, idx_clone).await;
    });

    (tx, index, handle)
}

async fn writer_loop(
    mut pool: ChunkPool,
    mut rx: mpsc::Receiver<WriteRequest>,
    index: SharedIndex,
) {
    // Rebuild index from existing pool data (sequential scan, one-time).
    match pool.scan_all_pools() {
        Ok(records) => {
            let count = records.len();
            index.write().rebuild_from_scanned(records);
            if count > 0 {
                info!(recovered = count, "Index rebuilt from pool files");
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to scan pool files, starting with empty index");
        }
    }

    info!("GlobalChunkWriter started");

    while let Some(req) = rx.recv().await {
        let camera_id = req.camera_id.clone();
        let data_len = req.data.len();

        // Check if rotation will happen and evict first.
        let (cur_idx, used, cap) = pool.status();
        let record_size = crate::storage::chunk_pool::RECORD_HEADER_SIZE + data_len as u64;
        if used + record_size > cap {
            // Next pool slot will be overwritten.
            let next_idx = (cur_idx + 1) % pool.pool_count();
            index.write().evict_pool(next_idx);
        }

        match pool.append(&camera_id, req.start_ts, req.end_ts, &req.data) {
            Ok(loc) => {
                let seg_id = index.write().insert(
                    &camera_id,
                    req.start_ts,
                    req.end_ts,
                    loc.clone(),
                );
                debug!(
                    camera = camera_id,
                    segment_id = seg_id,
                    pool_idx = loc.pool_idx,
                    offset = loc.record_offset,
                    bytes = data_len,
                    "Segment written"
                );
            }
            Err(e) => {
                error!(camera = camera_id, error = %e, "Failed to write segment to pool");
            }
        }
    }

    info!("GlobalChunkWriter shutting down (channel closed)");
}
