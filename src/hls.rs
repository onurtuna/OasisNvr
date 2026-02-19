//! HLS playlist generation — live (LL-HLS) and VOD.
//!
//! Endpoints served via the HTTP API:
//!   GET /api/hls/{camera_id}/live.m3u8           → live sliding-window playlist (LL-HLS)
//!   GET /api/hls/{camera_id}/live.m3u8?_HLS_msn=N  → blocking reload until segment N
//!   GET /api/hls/{camera_id}/vod.m3u8?from=...&to=...  → VOD playlist for time range
//!   GET /api/hls/{camera_id}/segment/{segment_id}.ts  → raw MPEG-TS segment data

use std::fmt::Write as FmtWrite;

use chrono::DateTime;
use chrono::Utc;

use crate::storage::index::{SegmentIndex, SegmentMeta};

/// Number of segments to include in the live sliding-window playlist.
const LIVE_WINDOW_SEGMENTS: usize = 10;

/// Generate a live LL-HLS playlist for a camera.
///
/// If `block_msn` is specified and is greater than the current max segment_id,
/// this function returns `None` — the caller should wait and retry.
pub fn generate_live_playlist(
    index: &SegmentIndex,
    camera_id: &str,
    segment_duration_secs: u64,
    block_msn: Option<u64>,
) -> Option<String> {
    let all_segments = index.segments_for_camera(camera_id);

    if all_segments.is_empty() {
        return Some(empty_live_playlist(segment_duration_secs));
    }

    // If blocking, check if the requested MSN exists yet.
    if let Some(msn) = block_msn {
        let max_id = all_segments.iter().map(|s| s.segment_id).max().unwrap_or(0);
        if msn > max_id {
            return None; // Caller should wait/poll.
        }
    }

    // Take the last N segments for the sliding window.
    let window_start = all_segments.len().saturating_sub(LIVE_WINDOW_SEGMENTS);
    let window = &all_segments[window_start..];

    let first_seq = window.first().map(|s| s.segment_id).unwrap_or(0);

    let mut m3u8 = String::with_capacity(2048);
    writeln!(m3u8, "#EXTM3U").unwrap();
    writeln!(m3u8, "#EXT-X-VERSION:6").unwrap();
    writeln!(m3u8, "#EXT-X-TARGETDURATION:{}", segment_duration_secs).unwrap();
    writeln!(m3u8, "#EXT-X-MEDIA-SEQUENCE:{}", first_seq).unwrap();

    // LL-HLS server control.
    writeln!(
        m3u8,
        "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,HOLD-BACK={}.0,PART-HOLD-BACK={:.1}",
        segment_duration_secs * 3,
        segment_duration_secs as f64 * 0.5,
    )
    .unwrap();

    for seg in window {
        let duration = segment_actual_duration(seg, segment_duration_secs);
        writeln!(m3u8, "#EXTINF:{:.3},", duration).unwrap();
        writeln!(
            m3u8,
            "/api/hls/{}/segment/{}.ts",
            camera_id, seg.segment_id
        )
        .unwrap();
    }

    // Preload hint for the next segment (LL-HLS).
    if let Some(last) = window.last() {
        let next_id = last.segment_id + 1;
        writeln!(
            m3u8,
            "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"/api/hls/{}/segment/{}.ts\"",
            camera_id, next_id
        )
        .unwrap();
    }

    Some(m3u8)
}

/// Generate a VOD playlist for a camera in a time range.
pub fn generate_vod_playlist(
    index: &SegmentIndex,
    camera_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    segment_duration_secs: u64,
) -> Option<String> {
    let segments = index.segments_in_range(camera_id, from, to);

    if segments.is_empty() {
        return None;
    }

    let first_seq = segments.first().map(|s| s.segment_id).unwrap_or(0);

    let mut m3u8 = String::with_capacity(2048);
    writeln!(m3u8, "#EXTM3U").unwrap();
    writeln!(m3u8, "#EXT-X-VERSION:6").unwrap();
    writeln!(m3u8, "#EXT-X-TARGETDURATION:{}", segment_duration_secs).unwrap();
    writeln!(m3u8, "#EXT-X-MEDIA-SEQUENCE:{}", first_seq).unwrap();
    writeln!(m3u8, "#EXT-X-PLAYLIST-TYPE:VOD").unwrap();

    for seg in &segments {
        let duration = segment_actual_duration(seg, segment_duration_secs);
        writeln!(m3u8, "#EXTINF:{:.3},", duration).unwrap();
        writeln!(
            m3u8,
            "/api/hls/{}/segment/{}.ts",
            camera_id, seg.segment_id
        )
        .unwrap();
    }

    writeln!(m3u8, "#EXT-X-ENDLIST").unwrap();
    Some(m3u8)
}

/// Compute the actual duration of a segment from its timestamps.
fn segment_actual_duration(seg: &SegmentMeta, fallback_secs: u64) -> f64 {
    let d = (seg.end_ts - seg.start_ts).num_milliseconds() as f64 / 1000.0;
    if d > 0.0 { d } else { fallback_secs as f64 }
}

/// Return an empty live playlist (no segments yet).
fn empty_live_playlist(segment_duration_secs: u64) -> String {
    let mut m3u8 = String::with_capacity(256);
    writeln!(m3u8, "#EXTM3U").unwrap();
    writeln!(m3u8, "#EXT-X-VERSION:6").unwrap();
    writeln!(m3u8, "#EXT-X-TARGETDURATION:{}", segment_duration_secs).unwrap();
    writeln!(m3u8, "#EXT-X-MEDIA-SEQUENCE:0").unwrap();
    writeln!(
        m3u8,
        "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,HOLD-BACK={}.0",
        segment_duration_secs * 3,
    )
    .unwrap();
    m3u8
}
