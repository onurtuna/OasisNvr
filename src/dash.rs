// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! DASH manifest (.mpd) generation — live and VOD.
//!
//! Serves the exact same self-initializing fMP4 segments as `hls.rs`
//! (`/api/hls/{camera_id}/segment/mp4/{segment_id}`) — CMAF means one set of
//! segments backs both HLS and DASH, just wrapped in a different manifest.
//! Since each segment carries its own `ftyp+moov`, it is referenced as both
//! the `initialization` and `media` template for its own `SegmentTimeline`
//! entry — there is no shared, separate init segment.
//!
//! Endpoints served via the HTTP API:
//!   GET /api/dash/{camera_id}/manifest.mpd             → live manifest
//!   GET /api/dash/{camera_id}/manifest.mpd?from=...&to=...  → VOD manifest for time range

use std::fmt::Write as FmtWrite;

use chrono::{DateTime, Utc};

use crate::storage::index::{SegmentIndex, SegmentMeta};

/// Number of segments to include in the live sliding-window manifest.
const LIVE_WINDOW_SEGMENTS: usize = 10;

/// Generate a live DASH manifest for a camera using an explicit
/// `SegmentTimeline` (segment durations vary since cuts are keyframe-aligned,
/// not fixed-duration).
pub fn generate_live_mpd(
    index: &SegmentIndex,
    camera_id: &str,
    segment_duration_secs: u64,
) -> Option<String> {
    let all_segments = index.segments_for_camera(camera_id);

    if all_segments.is_empty() {
        return Some(empty_mpd(segment_duration_secs, true));
    }

    let window_start = all_segments.len().saturating_sub(LIVE_WINDOW_SEGMENTS);
    let window = &all_segments[window_start..];

    Some(render_mpd(window, segment_duration_secs, true, camera_id))
}

/// Generate a VOD DASH manifest for a camera in a time range.
pub fn generate_vod_mpd(
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

    Some(render_mpd(&segments, segment_duration_secs, false, camera_id))
}

fn render_mpd(
    segments: &[&SegmentMeta],
    segment_duration_secs: u64,
    is_live: bool,
    camera_id: &str,
) -> String {
    let media_present_time = segments
        .first()
        .map(|s| s.start_ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();

    let total_duration: f64 = segments
        .iter()
        .map(|s| segment_actual_duration(s, segment_duration_secs))
        .sum();

    let mut mpd = String::with_capacity(2048);
    writeln!(mpd, r#"<?xml version="1.0" encoding="UTF-8"?>"#).unwrap();
    writeln!(
        mpd,
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" type="{}" availabilityStartTime="{}" minBufferTime="PT{:.1}S"{}>"#,
        if is_live { "dynamic" } else { "static" },
        media_present_time,
        segment_duration_secs as f64,
        if is_live {
            String::new()
        } else {
            format!(r#" mediaPresentationDuration="PT{:.3}S""#, total_duration)
        },
    )
    .unwrap();

    writeln!(mpd, r#"  <Period id="0" start="PT0S">"#).unwrap();
    writeln!(
        mpd,
        r#"    <AdaptationSet id="0" mimeType="video/mp4" segmentAlignment="true" startWithSAP="1">"#
    )
    .unwrap();
    writeln!(
        mpd,
        r#"      <Representation id="{}" bandwidth="2000000">"#,
        camera_id
    )
    .unwrap();
    writeln!(mpd, r#"        <SegmentTemplate media="segment/mp4/$Number$" timescale="1000" startNumber="{}">"#,
        segments.first().map(|s| s.segment_id).unwrap_or(0)
    ).unwrap();
    writeln!(mpd, r#"          <SegmentTimeline>"#).unwrap();
    for seg in segments {
        let duration_ms = (segment_actual_duration(seg, segment_duration_secs) * 1000.0) as u64;
        writeln!(
            mpd,
            r#"            <S t="{}" d="{}" />"#,
            seg.start_ts.timestamp_millis(),
            duration_ms
        )
        .unwrap();
    }
    writeln!(mpd, r#"          </SegmentTimeline>"#).unwrap();
    writeln!(mpd, r#"        </SegmentTemplate>"#).unwrap();
    writeln!(mpd, r#"      </Representation>"#).unwrap();
    writeln!(mpd, r#"    </AdaptationSet>"#).unwrap();
    writeln!(mpd, r#"  </Period>"#).unwrap();
    writeln!(mpd, r#"</MPD>"#).unwrap();

    mpd
}

fn segment_actual_duration(seg: &SegmentMeta, fallback_secs: u64) -> f64 {
    let d = (seg.end_ts - seg.start_ts).num_milliseconds() as f64 / 1000.0;
    if d > 0.0 { d } else { fallback_secs as f64 }
}

/// Return an empty manifest (no segments yet).
fn empty_mpd(segment_duration_secs: u64, is_live: bool) -> String {
    let mut mpd = String::with_capacity(512);
    writeln!(mpd, r#"<?xml version="1.0" encoding="UTF-8"?>"#).unwrap();
    writeln!(
        mpd,
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" type="{}" minBufferTime="PT{:.1}S">"#,
        if is_live { "dynamic" } else { "static" },
        segment_duration_secs as f64,
    )
    .unwrap();
    writeln!(mpd, r#"  <Period id="0" start="PT0S" />"#).unwrap();
    writeln!(mpd, r#"</MPD>"#).unwrap();
    mpd
}
