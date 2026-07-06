// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Camera ingestion worker.
//!
//! Each `CameraWorker` task pulls completed [`SegmentReady`] fragment files
//! from the `CameraStream` (segment cutting itself is done by
//! `splitmuxsink` in the GStreamer pipeline — see `camera.rs`), reads their
//! bytes, deletes the temp file, and forwards them as a [`WriteRequest`] to
//! the global chunk writer through an `mpsc` channel. NO direct disk writes
//! to the pool from here.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::camera::{supervised_connect, SegmentReady};
use crate::config::CameraConfig;
use crate::storage::global_writer::WriteRequest;

/// Per-camera ingestion task handle.
pub struct CameraWorker {
    pub camera_id: String,
    pub writer_tx: mpsc::Sender<WriteRequest>,
}

impl CameraWorker {
    pub fn new(camera_id: String, writer_tx: mpsc::Sender<WriteRequest>) -> Self {
        Self { camera_id, writer_tx }
    }

    /// Spawn the ingestion loop as an async task.
    pub fn spawn(
        self,
        config: CameraConfig,
        segment_duration: Duration,
        tmp_dir: PathBuf,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(config, segment_duration, tmp_dir).await
        })
    }

    async fn run(self, config: CameraConfig, segment_duration: Duration, tmp_dir: PathBuf) {
        info!(camera = self.camera_id, "Ingestion worker started");

        loop {
            let Some(mut stream) = supervised_connect(&config, segment_duration, &tmp_dir).await else {
                info!(camera = self.camera_id, "Stream supervisor shut down, exiting");
                break;
            };
            info!(camera = self.camera_id, "Stream connected, recording");

            loop {
                match stream.read_segment().await {
                    Some(seg) => self.forward_segment(seg).await,
                    None => {
                        warn!(camera = self.camera_id, "Stream closed, waiting for reconnect");
                        break;
                    }
                }
            }
        }

        error!(camera = self.camera_id, "Ingestion worker exited");
    }

    /// Read a completed fragment file's bytes, delete it, and hand it off
    /// to the global writer as a [`WriteRequest`].
    async fn forward_segment(&self, seg: SegmentReady) {
        // `splitmuxsink` runs with `async-finalize=true` (see camera.rs) so
        // the *previous* fragment's file may still be getting its trailing
        // moov/mfra flushed in the background when we're notified about it.
        // Wait for its size to stop changing before reading — the residual
        // write is tiny (a few KB of trailer) so this settles almost
        // immediately in practice.
        self.wait_for_file_stable(&seg.path).await;

        let data = match tokio::fs::read(&seg.path).await {
            Ok(d) => d,
            Err(e) => {
                error!(
                    camera = self.camera_id,
                    path = ?seg.path,
                    error = %e,
                    "Failed to read completed segment file, dropping"
                );
                return;
            }
        };

        if let Err(e) = tokio::fs::remove_file(&seg.path).await {
            warn!(camera = self.camera_id, path = ?seg.path, error = %e, "Failed to remove temp segment file");
        }

        let bytes = data.len();
        let req = WriteRequest {
            camera_id: self.camera_id.clone(),
            start_ts: seg.start_ts,
            end_ts: seg.end_ts,
            data,
        };

        match self.writer_tx.send(req).await {
            Ok(()) => {
                info!(
                    camera = self.camera_id,
                    bytes,
                    start = %seg.start_ts,
                    end = %seg.end_ts,
                    "Segment queued for global writer"
                );
            }
            Err(_) => {
                error!(camera = self.camera_id, "Global writer channel closed, segment dropped");
            }
        }
    }

    /// Poll a file's size until it stops changing (bounded), so we don't
    /// read it while `splitmuxsink`'s async finalization is still writing
    /// its trailing boxes.
    async fn wait_for_file_stable(&self, path: &std::path::Path) {
        const POLL_INTERVAL: Duration = Duration::from_millis(100);
        const MAX_ATTEMPTS: u32 = 30; // up to ~3s

        let mut last_size = match tokio::fs::metadata(path).await {
            Ok(m) => m.len(),
            Err(_) => return, // Let the subsequent read report the error.
        };

        for _ in 0..MAX_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let size = match tokio::fs::metadata(path).await {
                Ok(m) => m.len(),
                Err(_) => return,
            };
            if size == last_size {
                return;
            }
            last_size = size;
        }
        warn!(camera = self.camera_id, ?path, "Segment file size still changing after max wait, reading anyway");
    }
}
