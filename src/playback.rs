// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Playback / export: retrieve recorded video for a camera in a time range.
//!
//! Reads the in-memory `SegmentIndex` (rebuilt from pool files on startup)
//! to locate matching segments, then reads the raw MPEG-TS payloads from
//! the pool files and writes them to an output `.ts` file.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use tracing::info;

use crate::error::{NvrError, Result};
use crate::storage::chunk_pool::ChunkPool;
use crate::storage::index::SegmentIndex;

/// Export recorded video for `camera_id` in the range `[from, to]` to `output_path`.
///
/// The output is a concatenation of MPEG-TS segment payloads. It can be played
/// directly with VLC, ffplay, or any MPEG-TS-aware player.
///
/// Returns the number of segments written.
pub fn export_range(
    pool: &ChunkPool,
    index: &SegmentIndex,
    camera_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    output_path: &Path,
) -> Result<usize> {
    let segments = index.segments_in_range(camera_id, from, to);

    if segments.is_empty() {
        return Err(NvrError::Storage(format!(
            "No segments found for camera '{}' in range {} — {}",
            camera_id, from, to
        )));
    }

    let mut out = std::fs::File::create(output_path)
        .map_err(|e| NvrError::Storage(format!("create output {output_path:?}: {e}")))?;

    let mut total_bytes: u64 = 0;
    for seg in &segments {
        let data = pool.read_segment_data(&seg.location)?;
        out.write_all(&data)?;
        total_bytes += data.len() as u64;
        info!(
            camera = camera_id,
            segment_id = seg.segment_id,
            start = %seg.start_ts,
            end = %seg.end_ts,
            bytes = data.len(),
            "Segment exported"
        );
    }

    out.flush()?;
    info!(
        camera = camera_id,
        segments = segments.len(),
        total_mb = total_bytes / 1_048_576,
        output = ?output_path,
        "Export complete"
    );

    Ok(segments.len())
}
