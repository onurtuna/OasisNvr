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

use crate::camera::supervised_connect;
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

        // Once the nominal segment duration elapses, keep recording until the
        // next keyframe so every cut segment starts on a sync point (an
        // MPEG-TS file that starts mid-GOP cannot be decoded from its first
        // frame). This caps how long we're willing to wait for that keyframe
        // before cutting anyway, bounding worst-case segment size.
        let max_keyframe_wait = segment_duration;

        loop {
            // Wait for a connected stream.
            let Some(mut stream) = supervised_connect(&config).await else {
                info!(camera = self.camera_id, "Stream supervisor shut down, exiting");
                break;
            };
            info!(camera = self.camera_id, "Stream connected, recording");

            let mut segment_buf: Vec<u8> = Vec::new();
            let mut seg_start = Utc::now();
            let mut deadline = Instant::now() + segment_duration;
            let mut awaiting_keyframe = false;

            loop {
                // Wait for the next buffer OR the current deadline.
                let vbuf = tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => None,
                    buf = stream.read_buffer() => buf,
                };

                // If the nominal segment duration just elapsed, don't cut
                // immediately — hold off until a keyframe arrives.
                if !awaiting_keyframe && Instant::now() >= deadline {
                    awaiting_keyframe = true;
                    deadline = Instant::now() + max_keyframe_wait;
                }

                match vbuf {
                    Some(vb) => {
                        if awaiting_keyframe && vb.is_keyframe {
                            // Cut right before this keyframe so the new
                            // segment starts on a sync point.
                            self.flush_segment(
                                &mut segment_buf,
                                seg_start,
                                &mut seg_start,
                                &mut deadline,
                                segment_duration,
                            ).await;
                            awaiting_keyframe = false;
                            deadline = Instant::now() + segment_duration;
                        }
                        segment_buf.extend_from_slice(&vb.data);
                    }
                    None => {
                        if !awaiting_keyframe {
                            // Deadline hadn't elapsed, so this can only mean
                            // the stream/channel closed.
                            if !segment_buf.is_empty() {
                                self.flush_segment(
                                    &mut segment_buf,
                                    seg_start,
                                    &mut seg_start,
                                    &mut deadline,
                                    segment_duration,
                                ).await;
                            }
                            warn!(camera = self.camera_id, "Stream closed, waiting for reconnect");
                            break;
                        }

                        // Hard deadline: no keyframe arrived in time, cut anyway.
                        if !segment_buf.is_empty() {
                            self.flush_segment(
                                &mut segment_buf,
                                seg_start,
                                &mut seg_start,
                                &mut deadline,
                                segment_duration,
                            ).await;
                        }
                        awaiting_keyframe = false;
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
