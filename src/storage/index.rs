//! Segment index — maps (camera_id, time_range) → SegmentLocation.
//!
//! Kept entirely in memory during a recording session.
//! When a pool slot is rotated (overwritten) its index entries are evicted.
//!
//! For persistence across restarts the index can later be serialised to
//! `index.bin`; the initial version keeps it in-memory only.

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
}
