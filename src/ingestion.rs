// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Camera ingestion worker.
//!
//! Each `CameraWorker` task:
//!  1. Pulls raw MPEG-TS buffers from the `CameraStream`.
//!  2. Accumulates them until `segment_duration_secs` elapses.
//!  3. Sends the accumulated bytes as a [`WriteRequest`] to the global
//!     chunk writer through an `mpsc` channel.  NO direct disk writes.

use std::time::Duration;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::camera::{CameraStream, supervised_connect};
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
    pub fn spawn(self, config: CameraConfig, segment_duration: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(config, segment_duration).await
        })
    }

    async fn run(self, config: CameraConfig, segment_duration: Duration) {
        info!(camera = self.camera_id, "Ingestion worker started");

        // Channel through which the supervised connector delivers fresh streams.
        let (stream_tx, mut stream_rx) = mpsc::channel::<CameraStream>(1);
        let cam_cfg = config.clone();
        tokio::spawn(async move {
            supervised_connect(cam_cfg, stream_tx).await;
        });

        loop {
            // Wait for a connected stream.
            let Some(mut stream) = stream_rx.recv().await else {
                info!(camera = self.camera_id, "Stream supervisor shut down, exiting");
                break;
            };
            info!(camera = self.camera_id, "Stream connected, recording");

            let mut segment_buf: Vec<u8> = Vec::new();
            let mut seg_start = Utc::now();
            let mut deadline = Instant::now() + segment_duration;

            loop {
                // Wait for the next buffer OR segment deadline.
                let vbuf = tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => {
                        // Flush current segment even if no new buffer arrived.
                        None
                    }
                    buf = stream.read_buffer() => buf,
                };

                match vbuf {
                    Some(vb) => {
                        segment_buf.extend_from_slice(&vb.data);

                        // Check if the segment duration has elapsed.
                        if Instant::now() >= deadline {
                            self.flush_segment(
                                &mut segment_buf,
                                seg_start,
                                &mut seg_start,
                                &mut deadline,
                                segment_duration,
                            ).await;
                        }
                    }
                    None => {
                        // Deadline triggered or stream ended.
                        if !segment_buf.is_empty() {
                            self.flush_segment(
                                &mut segment_buf,
                                seg_start,
                                &mut seg_start,
                                &mut deadline,
                                segment_duration,
                            ).await;
                        } else {
                            // Stream closed without data — reconnect.
                            warn!(camera = self.camera_id, "Stream closed, waiting for reconnect");
                            break;
                        }
                        // Reset deadline after flush.
                        deadline = Instant::now() + segment_duration;
                    }
                }
            }
        }

        error!(camera = self.camera_id, "Ingestion worker exited");
    }

    /// Send accumulated buffer as a [`WriteRequest`] to the global writer.
    async fn flush_segment(
        &self,
        buf: &mut Vec<u8>,
        seg_start: chrono::DateTime<Utc>,
        next_start: &mut chrono::DateTime<Utc>,
        deadline: &mut Instant,
        segment_duration: Duration,
    ) {
        if buf.is_empty() {
            return;
        }
        let seg_end = Utc::now();
        let data = std::mem::take(buf);
        let bytes = data.len();

        let req = WriteRequest {
            camera_id: self.camera_id.clone(),
            start_ts: seg_start,
            end_ts: seg_end,
            data,
        };

        match self.writer_tx.send(req).await {
            Ok(()) => {
                info!(
                    camera = self.camera_id,
                    bytes,
                    start = %seg_start,
                    end = %seg_end,
                    "Segment queued for global writer"
                );
            }
            Err(_) => {
                error!(camera = self.camera_id, "Global writer channel closed, segment dropped");
            }
        }

        *next_start = Utc::now();
        *deadline = Instant::now() + segment_duration;
    }
}
