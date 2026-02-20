// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Segment index — maps (camera_id, time_range) → SegmentLocation.
//!
//! The index lives in memory during a recording session but is **persistent**:
//! on startup, pool files are scanned sequentially and the index is rebuilt
//! from the RecordHeaders already embedded in the data stream. No separate
//! index file is written, so recording I/O remains purely sequential.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::storage::chunk_pool::SegmentLocation;

/// Metadata about a single recorded segment, stored in the index.
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub segment_id: u64,
    pub camera_id: String,
    pub start_ts: DateTime<Utc>,
    pub end_ts: DateTime<Utc>,
    pub location: SegmentLocation,
}

/// Key for the ordered index: (camera_id, start_ts).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IndexKey {
    camera_id: String,
    start_ts: DateTime<Utc>,
    /// tiebreak on segment_id
    segment_id: u64,
}

/// In-memory index of all live segments across the global pool.
#[derive(Default)]
pub struct SegmentIndex {
    entries: BTreeMap<IndexKey, SegmentMeta>,
    segment_counter: u64,
}

impl SegmentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new segment into the index.
    pub fn insert(
        &mut self,
        camera_id: &str,
        start_ts: DateTime<Utc>,
        end_ts: DateTime<Utc>,
        location: SegmentLocation,
    ) -> u64 {
        let id = self.segment_counter;
        self.segment_counter += 1;
        let key = IndexKey {
            camera_id: camera_id.to_string(),
            start_ts,
            segment_id: id,
        };
        self.entries.insert(
            key,
            SegmentMeta {
                segment_id: id,
                camera_id: camera_id.to_string(),
                start_ts,
                end_ts,
                location,
            },
        );
        id
    }

    /// Evict all segments whose data lives in `pool_idx`.
    /// Called when the pool at that index is about to be overwritten.
    pub fn evict_pool(&mut self, pool_idx: usize) {
        self.entries.retain(|_, v| v.location.pool_idx != pool_idx);
    }

    /// Return all segments for a given camera in chronological order.
    pub fn segments_for_camera(
        &self,
        camera_id: &str,
    ) -> Vec<&SegmentMeta> {
        self.entries.values().filter(move |m| m.camera_id == camera_id).collect()
    }

    /// Return segments for `camera_id` that overlap the time range `[from, to]`.
    /// A segment overlaps if `segment.start_ts < to && segment.end_ts > from`.
    pub fn segments_in_range(
        &self,
        camera_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<&SegmentMeta> {
        self.entries
            .values()
            .filter(|m| m.camera_id == camera_id && m.start_ts < to && m.end_ts > from)
            .collect()
    }

    /// Return all segments across all cameras in insertion order.
    pub fn all_segments(&self) -> impl Iterator<Item = &SegmentMeta> {
        self.entries.values()
    }

    /// Total number of indexed segments.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rebuild the index from records recovered by scanning pool files.
    /// Called once on startup; zero disk I/O of its own.
    pub fn rebuild_from_scanned(&mut self, records: Vec<crate::storage::chunk_pool::ScannedRecord>) {
        self.entries.clear();
        self.segment_counter = 0;
        for r in records {
            let loc = crate::storage::chunk_pool::SegmentLocation {
                pool_idx: r.pool_idx,
                pool_id: r.pool_id,
                record_offset: r.record_offset,
                record_size: r.record_size,
            };
            self.insert(&r.camera_id, r.start_ts, r.end_ts, loc);
        }
    }
}
