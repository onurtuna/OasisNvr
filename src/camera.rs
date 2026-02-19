//! Camera stream abstraction using GStreamer.
//!
//! Each camera runs a GStreamer pipeline:
//!   rtspsrc → rtph264depay → h264parse → mpegtsmux → appsink
//!
//! The `appsink` emits MPEG-TS buffers that the ingestion worker accumulates
//! into segments and then flushes to the ring buffer storage.

use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::CameraConfig;
use crate::error::{NvrError, Result};

/// A raw MPEG-TS chunk produced by the GStreamer pipeline.
#[derive(Debug)]
pub struct VideoBuffer {
    pub data: Vec<u8>,
    pub pts_us: Option<i64>, // presentation timestamp in microseconds
}

/// Handle to a running GStreamer pipeline for one RTSP camera.
pub struct CameraStream {
    pub config: CameraConfig,
    pipeline: gst::Pipeline,
    rx: mpsc::Receiver<VideoBuffer>,
}

impl CameraStream {
    /// Build and start a GStreamer pipeline for the given camera.
    /// Buffers are forwarded through an async channel.
    pub fn connect(config: &CameraConfig) -> Result<Self> {
        gst::init().map_err(|e| NvrError::GStreamer(format!("gst::init: {e}")))?;

        let (tx, rx) = mpsc::channel::<VideoBuffer>(128);

        let pipeline_str = format!(
            "rtspsrc location={url} latency=200 protocols=tcp ! \
             rtph264depay ! h264parse ! mpegtsmux ! \
             appsink name=sink emit-signals=true max-buffers=32 drop=true sync=false",
            url = config.url
        );

        let pipeline = gst::parse::launch(&pipeline_str)
            .map_err(|e| NvrError::GStreamer(format!("parse_launch: {e}")))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| NvrError::GStreamer("Not a pipeline".into()))?;

        // Connect the appsink callback.
        let appsink: gst_app::AppSink = pipeline
            .by_name("sink")
            .ok_or_else(|| NvrError::GStreamer("appsink not found".into()))?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| NvrError::GStreamer("Cast to AppSink failed".into()))?;

        let tx_clone = tx.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                    let buf = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buf.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let pts_us = buf.pts().map(|t| t.useconds() as i64);
                    let vbuf = VideoBuffer {
                        data: map.as_slice().to_vec(),
                        pts_us,
                    };
                    // Non-blocking send; drop if channel is full.
                    let _ = tx_clone.try_send(vbuf);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| NvrError::GStreamer(format!("set_state Playing: {e}")))?;

        info!(camera = config.id, url = config.url, "GStreamer pipeline started");

        Ok(CameraStream {
            config: config.clone(),
            pipeline,
            rx,
        })
    }

    /// Receive the next [`VideoBuffer`] produced by the pipeline.
    /// Returns `None` when the channel is closed (EOS or error).
    pub async fn read_buffer(&mut self) -> Option<VideoBuffer> {
        self.rx.recv().await
    }

    /// Stop the pipeline cleanly.
    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        info!(camera = self.config.id, "GStreamer pipeline stopped");
    }
}

impl Drop for CameraStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a task that keeps a camera connected, reconnecting on failure.
///
/// Returns a `Receiver` of ready-to-use `CameraStream`s. The caller consumes
/// streams one at a time; when a stream errors or closes the supervisor
/// automatically reconnects and sends a fresh stream.
pub async fn supervised_connect(
    config: CameraConfig,
    stream_tx: mpsc::Sender<CameraStream>,
) {
    let max_attempts = if config.max_reconnect_attempts == 0 {
        u32::MAX
    } else {
        config.max_reconnect_attempts
    };

    let mut attempt = 0u32;
    loop {
        if attempt >= max_attempts {
            error!(camera = config.id, "Max reconnect attempts reached, giving up");
            break;
        }

        match CameraStream::connect(&config) {
            Ok(stream) => {
                attempt = 0; // Reset backoff on success.
                if stream_tx.send(stream).await.is_err() {
                    // Receiver dropped; manager is shutting down.
                    break;
                }
                // The receiver signals readiness for a new stream by dropping
                // the previous one. We wait briefly then loop.
                sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                attempt += 1;
                let backoff = Duration::from_secs((2u64.pow(attempt.min(6))).min(60));
                warn!(
                    camera = config.id,
                    attempt,
                    ?backoff,
                    error = %e,
                    "Connection failed, will retry"
                );
                sleep(backoff).await;
            }
        }
    }
}
