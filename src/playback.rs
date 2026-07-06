// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Playback / export: retrieve recorded video for a camera in a time range.
//!
//! Reads the in-memory `SegmentIndex` (rebuilt from pool files on startup)
//! to locate matching segments. Each stored segment is an independent,
//! self-initializing fMP4 file (own `ftyp+moov+moof+mdat`), so unlike the
//! old MPEG-TS format they can't be concatenated as raw bytes — exporting a
//! range does a real demux + remux through a short-lived GStreamer pipeline
//! instead.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::info;

use crate::error::{NvrError, Result};
use crate::storage::chunk_pool::ChunkPool;
use crate::storage::index::SegmentIndex;

/// Export recorded video for `camera_id` in the range `[from, to]` to `output_path`.
///
/// The output is one continuous, standalone MP4 playable directly with VLC,
/// ffplay, or any MP4-aware player.
///
/// Returns the number of segments exported.
pub fn export_range(
    pool: &ChunkPool,
    index: &SegmentIndex,
    camera_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    output_path: &Path,
) -> Result<usize> {
    gst::init().map_err(|e| NvrError::GStreamer(format!("gst::init: {e}")))?;

    let segments = index.segments_in_range(camera_id, from, to);

    if segments.is_empty() {
        return Err(NvrError::Storage(format!(
            "No segments found for camera '{}' in range {} — {}",
            camera_id, from, to
        )));
    }

    // Stage each segment's bytes into its own temp file — qtdemux needs a
    // seekable source, and the pool only gives us in-memory byte slices.
    let tmp_dir = std::env::temp_dir().join(format!(
        "nvr_export_{}_{}",
        std::process::id(),
        Utc::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&tmp_dir)?;

    let mut seg_paths = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let data = pool.read_segment_data(&seg.location)?;
        let seg_path = tmp_dir.join(format!("seg_{i:05}.mp4"));
        std::fs::File::create(&seg_path)?.write_all(&data)?;
        seg_paths.push(seg_path);
    }

    let result = remux_segments(&seg_paths, output_path);

    // Best-effort cleanup regardless of outcome.
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result?;

    info!(
        camera = camera_id,
        segments = segments.len(),
        output = ?output_path,
        "Export complete"
    );

    Ok(segments.len())
}

/// Demux and remux a list of standalone fMP4 segment files, in order, into
/// one continuous playable MP4 at `output_path`.
///
/// Uses `concat` to play each segment's demuxed elementary stream out
/// sequentially (not as separate simultaneous tracks) into one fresh
/// `mp4mux`. Built via explicit element construction (not a `parse::launch`
/// string) since segment/camera-derived paths could otherwise need escaping.
fn remux_segments(seg_paths: &[PathBuf], output_path: &Path) -> Result<()> {
    let pipeline = gst::Pipeline::new();

    let make = |factory: &str| -> Result<gst::Element> {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|e| NvrError::GStreamer(format!("create {factory}: {e}")))
    };

    let concat = make("concat")?;
    let h264parse = make("h264parse")?;
    let mp4mux = make("mp4mux")?;
    let filesink = gst::ElementFactory::make("filesink")
        .property("location", output_path.to_string_lossy().as_ref())
        .build()
        .map_err(|e| NvrError::GStreamer(format!("create filesink: {e}")))?;

    for el in [&concat, &h264parse, &mp4mux, &filesink] {
        pipeline
            .add(el)
            .map_err(|e| NvrError::GStreamer(format!("add element: {e}")))?;
    }
    concat
        .link(&h264parse)
        .map_err(|e| NvrError::GStreamer(format!("link concat->h264parse: {e}")))?;
    h264parse
        .link(&mp4mux)
        .map_err(|e| NvrError::GStreamer(format!("link h264parse->mp4mux: {e}")))?;
    mp4mux
        .link(&filesink)
        .map_err(|e| NvrError::GStreamer(format!("link mp4mux->filesink: {e}")))?;

    for seg_path in seg_paths {
        let filesrc = gst::ElementFactory::make("filesrc")
            .property("location", seg_path.to_string_lossy().as_ref())
            .build()
            .map_err(|e| NvrError::GStreamer(format!("create filesrc: {e}")))?;
        let qtdemux = make("qtdemux")?;

        pipeline
            .add(&filesrc)
            .map_err(|e| NvrError::GStreamer(format!("add filesrc: {e}")))?;
        pipeline
            .add(&qtdemux)
            .map_err(|e| NvrError::GStreamer(format!("add qtdemux: {e}")))?;
        filesrc
            .link(&qtdemux)
            .map_err(|e| NvrError::GStreamer(format!("link filesrc->qtdemux: {e}")))?;

        let concat_sink = concat
            .request_pad_simple("sink_%u")
            .ok_or_else(|| NvrError::GStreamer("concat: no sink pad available".into()))?;

        qtdemux.connect_pad_added(move |_demux, src_pad| {
            if src_pad.name().starts_with("video") {
                let _ = src_pad.link(&concat_sink);
            }
        });
    }

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| NvrError::GStreamer(format!("set_state Playing (export remux): {e}")))?;

    let bus = pipeline
        .bus()
        .ok_or_else(|| NvrError::GStreamer("no bus on export remux pipeline".into()))?;

    let result = loop {
        match bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(msg) => match msg.view() {
                gst::MessageView::Eos(_) => break Ok(()),
                gst::MessageView::Error(err) => {
                    break Err(NvrError::GStreamer(format!(
                        "export remux pipeline error: {} ({:?})",
                        err.error(),
                        err.debug()
                    )));
                }
                _ => unreachable!(),
            },
            None => {
                break Err(NvrError::GStreamer("export remux pipeline timed out".into()));
            }
        }
    };

    let _ = pipeline.set_state(gst::State::Null);
    result
}
