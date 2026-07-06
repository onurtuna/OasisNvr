// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Camera stream abstraction using GStreamer.
//!
//! Each camera runs a GStreamer pipeline:
//!   rtspsrc → (depay → parse, chosen at runtime from the negotiated RTP
//!   encoding) → splitmuxsink(mp4mux)
//!
//! The depayloader/parser pair isn't known until `rtspsrc` negotiates the
//! stream's RTP caps with the camera, so it's wired up dynamically from the
//! `pad-added` signal instead of being part of a static pipeline string.
//! Currently supported encodings: H264 (`rtph264depay ! h264parse`) and AV1
//! (`rtpav1depay ! av1parse`).
//!
//! `splitmuxsink` owns segment cutting: it always splits at the next
//! keyframe at/after `max-size-time`, so every resulting fragment file is a
//! clean, self-initializing fMP4 (`ftyp+moov+moof+mdat`) that a player can
//! start decoding from byte 0. We're notified of each completed fragment via
//! the `format-location` signal and forward it to the ingestion worker as a
//! [`SegmentReady`].
//!
//! Exception: the very first fragment of every connection is written by a
//! muxer bin `splitmuxsink` bootstraps synchronously before it has a chance
//! to apply `muxer-properties`, so it comes out as a plain non-fragmented
//! MP4 instead — see `FragmentState::discard_next`, which drops it instead
//! of serving it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gstreamer as gst;
use gstreamer::prelude::*;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::CameraConfig;
use crate::error::{NvrError, Result};

/// A completed, independently-playable fMP4 segment file produced by
/// `splitmuxsink`. The ingestion worker reads its bytes, deletes the temp
/// file, and forwards the data to the global writer.
#[derive(Debug)]
pub struct SegmentReady {
    pub path: PathBuf,
    pub start_ts: DateTime<Utc>,
    pub end_ts: DateTime<Utc>,
}

/// Tracks the fragment currently being written by `splitmuxsink`, shared
/// between the `format-location` signal callback (runs on a GStreamer
/// streaming thread) and `CameraStream::stop()`.
struct FragmentState {
    current_path: Option<PathBuf>,
    current_start: DateTime<Utc>,
    /// The very first fragment of every connection is written by a muxer
    /// bin `splitmuxsink` bootstraps synchronously at startup, which does
    /// *not* pick up `muxer-properties` (confirmed empirically: it comes out
    /// as a classic single-`moov` MP4 instead of fragmented `moof`+`mdat`).
    /// Every later fragment uses a muxer bin pre-rolled via `async-finalize`
    /// while the previous one finalizes, and that one fragments correctly.
    /// A non-fragmented segment can't be appended to a browser's
    /// MediaSource, so we discard this one instead of serving it.
    discard_next: bool,
}

/// Handle to a running GStreamer pipeline for one RTSP camera.
pub struct CameraStream {
    pub config: CameraConfig,
    pipeline: gst::Pipeline,
    rx: mpsc::Receiver<SegmentReady>,
    seg_tx: mpsc::Sender<SegmentReady>,
    state: Arc<Mutex<FragmentState>>,
}

impl CameraStream {
    /// Build and start a GStreamer pipeline for the given camera.
    /// Completed segment files are forwarded through an async channel.
    ///
    /// `tmp_dir` is where `splitmuxsink` writes short-lived per-segment
    /// fragment files before the ingestion worker reads and deletes them.
    pub fn connect(
        config: &CameraConfig,
        segment_duration: Duration,
        tmp_dir: &Path,
    ) -> Result<Self> {
        gst::init().map_err(|e| NvrError::GStreamer(format!("gst::init: {e}")))?;

        std::fs::create_dir_all(tmp_dir)
            .map_err(|e| NvrError::GStreamer(format!("create temp dir {tmp_dir:?}: {e}")))?;

        let (tx, rx) = mpsc::channel::<SegmentReady>(32);

        // rtspsrc and splitmuxsink are declared here, unlinked — the
        // depay/parse chain between them depends on the RTP encoding
        // negotiated at connect time, so it's built dynamically below from
        // rtspsrc's `pad-added` signal instead of being part of this string.
        let max_size_time_ns = segment_duration.as_nanos() as u64;
        let pipeline_str = format!(
            "rtspsrc name=src location={url} latency=200 protocols=tcp \
             splitmuxsink name=splitmux max-size-time={max_size_time_ns} send-keyframe-requests=true",
            url = config.url
        );

        let pipeline = gst::parse::launch(&pipeline_str)
            .map_err(|e| NvrError::GStreamer(format!("parse_launch: {e}")))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| NvrError::GStreamer("Not a pipeline".into()))?;

        let rtspsrc = pipeline
            .by_name("src")
            .ok_or_else(|| NvrError::GStreamer("rtspsrc not found".into()))?;
        let splitmux = pipeline
            .by_name("splitmux")
            .ok_or_else(|| NvrError::GStreamer("splitmuxsink not found".into()))?;

        // A single mp4mux instance reused across fragments (the default,
        // `async-finalize=false` mode) does NOT reliably produce fragmented
        // (`moof`+`mdat`) output across resets in this GStreamer version —
        // confirmed empirically: `fragment-duration` stuck even though set,
        // but every split file still came out as a classic single-`moov`
        // MP4. Using `async-finalize=true` instead gives every fragment a
        // *fresh* `mp4mux` instance (via `muxer-factory`), which does
        // fragment correctly — verified directly against recorded output.
        // Properties for each fresh instance are supplied via
        // `muxer-properties` (only honored when `async-finalize=true`).
        let muxer_props = gst::Structure::builder("properties")
            .field("fragment-duration", segment_duration.as_millis() as u32)
            .field("streamable", true)
            .build();
        splitmux.set_property("async-finalize", true);
        splitmux.set_property("muxer-factory", "mp4mux");
        splitmux.set_property("muxer-properties", &muxer_props);

        // The depay/parse chain depends on the codec the camera actually
        // negotiates over RTSP, which isn't known until `rtspsrc` creates its
        // (sometimes) src pad for the stream. Build and link it dynamically
        // here rather than assuming H264 up front.
        let pipeline_for_pad = pipeline.clone();
        let splitmux_for_pad = splitmux.clone();
        let camera_id_for_pad = config.id.clone();
        rtspsrc.connect_pad_added(move |_src, pad| {
            let Some(caps) = pad.current_caps() else {
                warn!(camera = camera_id_for_pad, "rtspsrc pad has no negotiated caps yet, ignoring");
                return;
            };
            let Some(s) = caps.structure(0) else {
                warn!(camera = camera_id_for_pad, "rtspsrc pad caps have no structure, ignoring");
                return;
            };

            // Only the video media is recorded; silently ignore any other
            // pad (e.g. an audio track) rather than treating it as an error.
            if s.get::<String>("media").ok().as_deref() != Some("video") {
                return;
            }

            let encoding_name = s.get::<String>("encoding-name").ok();
            let (depay_factory, parse_factory) = match encoding_name.as_deref() {
                Some("H264") => ("rtph264depay", "h264parse"),
                Some("AV1") => ("rtpav1depay", "av1parse"),
                other => {
                    error!(
                        camera = camera_id_for_pad,
                        encoding = ?other,
                        "Unsupported video RTP encoding, cannot record this camera"
                    );
                    return;
                }
            };

            let make = |factory: &str| -> Option<gst::Element> {
                match gst::ElementFactory::make(factory).build() {
                    Ok(el) => Some(el),
                    Err(e) => {
                        error!(camera = camera_id_for_pad, factory, error = %e, "Failed to create element");
                        None
                    }
                }
            };

            let (Some(depay), Some(parse)) = (make(depay_factory), make(parse_factory)) else {
                return;
            };
            if parse_factory == "h264parse" {
                parse.set_property("config-interval", -1i32);
            }

            for el in [&depay, &parse] {
                if let Err(e) = pipeline_for_pad.add(el) {
                    error!(camera = camera_id_for_pad, error = %e, "Failed to add element to pipeline");
                    return;
                }
                if let Err(e) = el.sync_state_with_parent() {
                    error!(camera = camera_id_for_pad, error = %e, "Failed to sync element state with pipeline");
                    return;
                }
            }

            if let Err(e) = depay.link(&parse) {
                error!(camera = camera_id_for_pad, error = %e, "Failed to link depayloader to parser");
                return;
            }

            let Some(depay_sink) = depay.static_pad("sink") else {
                error!(camera = camera_id_for_pad, "depayloader has no sink pad");
                return;
            };
            if let Err(e) = pad.link(&depay_sink) {
                error!(camera = camera_id_for_pad, error = ?e, "Failed to link rtspsrc pad to depayloader");
                return;
            }

            let Some(splitmux_sink) = splitmux_for_pad.request_pad_simple("video") else {
                error!(camera = camera_id_for_pad, "splitmuxsink has no video pad available");
                return;
            };
            let Some(parse_src) = parse.static_pad("src") else {
                error!(camera = camera_id_for_pad, "parser has no src pad");
                return;
            };
            if let Err(e) = parse_src.link(&splitmux_sink) {
                error!(camera = camera_id_for_pad, error = ?e, "Failed to link parser to splitmuxsink");
            }
        });

        let state = Arc::new(Mutex::new(FragmentState {
            current_path: None,
            current_start: Utc::now(),
            discard_next: true,
        }));

        let state_clone = state.clone();
        let tx_clone = tx.clone();
        let tmp_dir_owned = tmp_dir.to_path_buf();
        let camera_id = config.id.clone();
        splitmux.connect("format-location", false, move |values| {
            let fragment_id: u32 = values[1].get().unwrap_or(0);
            let now = Utc::now();

            let mut st = state_clone.lock().unwrap();
            if let Some(prev_path) = st.current_path.take() {
                if st.discard_next {
                    warn!(
                        camera = camera_id,
                        path = ?prev_path,
                        "Discarding non-fragmented bootstrap segment (unplayable via MSE)"
                    );
                    let _ = std::fs::remove_file(&prev_path);
                    st.discard_next = false;
                } else {
                    let seg = SegmentReady {
                        path: prev_path,
                        start_ts: st.current_start,
                        end_ts: now,
                    };
                    if tx_clone.blocking_send(seg).is_err() {
                        warn!(camera = camera_id, "Segment channel closed, dropping completed segment");
                    }
                }
            }

            let new_path = tmp_dir_owned.join(format!("{camera_id}_{fragment_id:010}.mp4"));
            st.current_path = Some(new_path.clone());
            st.current_start = now;
            drop(st);

            Some(new_path.to_string_lossy().into_owned().to_value())
        });

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| NvrError::GStreamer(format!("set_state Playing: {e}")))?;

        // Force the unreliable bootstrap fragment (see `FragmentState::discard_next`)
        // to close quickly at the next keyframe instead of running for a full
        // `segment_duration`, so we lose a few seconds of footage per (re)connect
        // instead of up to a whole segment.
        let splitmux_clone = splitmux.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            splitmux_clone.emit_by_name::<()>("split-now", &[]);
        });

        info!(camera = config.id, url = config.url, "GStreamer pipeline started");

        Ok(CameraStream {
            config: config.clone(),
            pipeline,
            rx,
            seg_tx: tx,
            state,
        })
    }

    /// Receive the next completed [`SegmentReady`] produced by the pipeline.
    /// Returns `None` when the channel is closed (EOS or error).
    pub async fn read_segment(&mut self) -> Option<SegmentReady> {
        self.rx.recv().await
    }

    /// Stop the pipeline cleanly, making sure the in-flight fragment is
    /// properly finalized (and forwarded) before tearing down.
    pub fn stop(&self) {
        // Ask splitmuxsink to finish the current fragment properly instead
        // of just killing the pipeline mid-write.
        let _ = self.pipeline.send_event(gst::event::Eos::new());
        if let Some(bus) = self.pipeline.bus() {
            let _ = bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
        }
        let _ = self.pipeline.set_state(gst::State::Null);

        // No further `format-location` call will happen for this stream, so
        // manually flush whatever fragment was still open when it stopped.
        let last = {
            let mut st = self.state.lock().unwrap();
            let discard = st.discard_next;
            st.current_path.take().map(|path| (path, st.current_start, discard))
        };
        if let Some((path, start_ts, discard)) = last {
            if discard {
                let _ = std::fs::remove_file(&path);
            } else {
                let seg = SegmentReady {
                    path,
                    start_ts,
                    end_ts: Utc::now(),
                };
                let _ = self.seg_tx.try_send(seg);
            }
        }

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
/// Returns a ready-to-use `CameraStream`. When a stream errors or closes,
/// call this again to automatically reconnect with exponential backoff.
pub async fn supervised_connect(
    config: &CameraConfig,
    segment_duration: Duration,
    tmp_dir: &Path,
) -> Option<CameraStream> {
    let max_attempts = if config.max_reconnect_attempts == 0 {
        u32::MAX
    } else {
        config.max_reconnect_attempts
    };

    let mut attempt = 0u32;
    loop {
        if attempt >= max_attempts {
            error!(camera = config.id, "Max reconnect attempts reached, giving up");
            return None;
        }

        match CameraStream::connect(config, segment_duration, tmp_dir) {
            Ok(stream) => {
                return Some(stream);
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
