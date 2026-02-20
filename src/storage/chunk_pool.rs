// This software is provided for non-commercial use only.
// Commercial use is strictly prohibited.
// If you use, modify, or redistribute this software, you must provide proper attribution to the original author.
// (c) 2026 Onur Tuna. All rights reserved.

//! Global chunk pool — shared storage for ALL cameras.
//!
//! Pre-allocates `max_chunks` fixed-size binary files under `base_path/`:
//!   pool_000.bin, pool_001.bin, …, pool_N.bin
//!
//! All cameras write into the SAME sequential stream → zero seek overhead.
//!
//! ## File Layout
//!
//! ```text
//! [PoolHeader  : 64 bytes]
//!   magic      : [u8;8]  = b"NVRPOOL0"
//!   pool_id    : u64     (LE) — monotonic ID, incremented on each rotation
//!   created_at : i64     (unix seconds, LE)
//!   reserved   : [u8;40]
//!
//! [RecordHeader: 32 bytes per record]
//!   magic      : [u8;4]  = b"NREC"
//!   camera_id  : [u8;16] (UTF-8, zero-padded)
//!   start_ts   : i64     (unix seconds, LE)
//!   end_ts     : i64     (unix seconds, LE) — filled in by writer
//!   data_len   : u32     (LE)
//!
//! [raw data    : data_len bytes]
//! ```

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use tracing::{debug, info, warn};

use crate::error::{NvrError, Result};

// ─────────────────────────────── constants ───────────────────────────────────

pub const POOL_MAGIC: &[u8; 8] = b"NVRPOOL0";
pub const RECORD_MAGIC: &[u8; 4] = b"NREC";
pub const POOL_HEADER_SIZE: u64 = 64;
pub const RECORD_HEADER_SIZE: u64 = 4 + 16 + 8 + 8 + 4; // 40 bytes

// ─────────────────────────────── types ───────────────────────────────────────

/// Identifies the physical location of a segment in the pool.
#[derive(Debug, Clone)]
pub struct SegmentLocation {
    /// Index of the pool file (0-based).
    pub pool_idx: usize,
    /// Monotonic ID written in the pool header at rotation time.
    pub pool_id: u64,
    /// Byte offset of the `NREC` magic within the pool file.
    pub record_offset: u64,
    /// Total byte length of the record (header + data).
    pub record_size: u64,
}

/// A record recovered from scanning a pool file on startup.
#[derive(Debug, Clone)]
pub struct ScannedRecord {
    pub camera_id: String,
    pub start_ts: DateTime<Utc>,
    pub end_ts: DateTime<Utc>,
    pub pool_idx: usize,
    pub pool_id: u64,
    pub record_offset: u64,
    pub record_size: u64,
}

// ─────────────────────────────── ChunkPool ───────────────────────────────────

struct PoolSlot {
    path: PathBuf,
    pool_id: u64,
    /// Bytes used after POOL_HEADER_SIZE.
    bytes_used: u64,
}

/// Manages `max_pools` pre-allocated binary pool files under `base_path/`.
/// **Not** thread-safe on its own; callers must hold a lock or use
/// `GlobalChunkWriter` which is the single writer.
pub struct ChunkPool {
    #[allow(dead_code)]
    base_path: PathBuf,
    pool_capacity: u64, // bytes per pool excluding header
    slots: Vec<PoolSlot>,
    /// Index of the pool currently being written.
    pub write_idx: usize,
    /// Shared per-pool reader counters.
    pub read_counters: Arc<PoolReadCounters>,
}

// ────────────── read safety ───────────────────────────────────────

/// Per-pool atomic reader counters. Shared between the writer and all API
/// handlers via `Arc`. When a reader is active on a pool, the writer will
/// wait before rotating into that pool.
pub struct PoolReadCounters {
    counters: Vec<AtomicUsize>,
}

impl PoolReadCounters {
    /// Create counters for `n` pools.
    pub fn new(n: usize) -> Self {
        let mut counters = Vec::with_capacity(n);
        for _ in 0..n {
            counters.push(AtomicUsize::new(0));
        }
        Self { counters }
    }

    /// Acquire a read lock on `pool_idx`. Returns a guard that auto-releases.
    pub fn acquire(&self, pool_idx: usize) -> PoolReadGuard {
        self.counters[pool_idx].fetch_add(1, Ordering::SeqCst);
        PoolReadGuard {
            counters: self as *const PoolReadCounters,
            pool_idx,
        }
    }

    /// Check if any reader holds `pool_idx`.
    pub fn has_readers(&self, pool_idx: usize) -> bool {
        self.counters[pool_idx].load(Ordering::SeqCst) > 0
    }

    fn release(&self, pool_idx: usize) {
        self.counters[pool_idx].fetch_sub(1, Ordering::SeqCst);
    }
}

/// RAII guard that decrements the per-pool reader count on drop.
/// This ensures readers are always released even on error/panic.
pub struct PoolReadGuard {
    counters: *const PoolReadCounters,
    pool_idx: usize,
}

// Safety: PoolReadCounters uses AtomicUsize which is Send+Sync.
unsafe impl Send for PoolReadGuard {}
unsafe impl Sync for PoolReadGuard {}

impl Drop for PoolReadGuard {
    fn drop(&mut self) {
        // Safety: the Arc<PoolReadCounters> outlives all guards because
        // both the writer and API share the same Arc.
        unsafe { &*self.counters }.release(self.pool_idx);
    }
}

impl ChunkPool {
    /// Open (or create + pre-allocate) all pool files.
    /// If pool files already exist, scans their headers to determine
    /// which pool was last written to and resumes from there.
    pub fn open(base_path: &Path, pool_size_bytes: u64, max_pools: usize) -> Result<Self> {
        std::fs::create_dir_all(base_path)
            .map_err(|e| NvrError::Storage(format!("Cannot create storage dir: {e}")))?;

        let mut slots = Vec::with_capacity(max_pools);
        let mut best_idx: usize = 0;
        let mut best_pool_id: u64 = 0;
        let mut any_existing = false;

        for i in 0..max_pools {
            let path = base_path.join(format!("pool_{:03}.bin", i));
            if !path.exists() {
                let total = POOL_HEADER_SIZE + pool_size_bytes;
                let f = File::create(&path)?;
                f.set_len(total)
                    .map_err(|e| NvrError::Storage(format!("preallocate {path:?}: {e}")))?;
                info!(pool = i, path = ?path, size_mb = total / 1_048_576, "Pre-allocated pool file");
                slots.push(PoolSlot { path, pool_id: i as u64, bytes_used: 0 });
            } else {
                any_existing = true;
                // Read pool header to recover pool_id and detect latest.
                let (pid, _created) = Self::read_pool_header(&path)?;
                // Scan records to find bytes_used.
                let records = Self::scan_records(&path, i, pid, pool_size_bytes)?;
                let bytes_used: u64 = records.iter().map(|r| r.record_size).sum();
                if pid >= best_pool_id {
                    best_pool_id = pid;
                    best_idx = i;
                }
                info!(pool = i, pool_id = pid, records = records.len(), bytes_used, "Recovered pool file");
                slots.push(PoolSlot { path, pool_id: pid, bytes_used });
            }
        }

        let write_idx = if any_existing { best_idx } else { 0 };

        let read_counters = Arc::new(PoolReadCounters::new(max_pools));

        let pool = ChunkPool {
            base_path: base_path.to_path_buf(),
            pool_capacity: pool_size_bytes,
            slots,
            write_idx,
            read_counters,
        };

        if !any_existing {
            pool.write_pool_header(0)?;
        }

        info!(write_idx, "ChunkPool opened");
        Ok(pool)
    }

    /// Append one segment record.  Returns the [`SegmentLocation`] written.
    pub fn append(
        &mut self,
        camera_id: &str,
        start_ts: DateTime<Utc>,
        end_ts: DateTime<Utc>,
        data: &[u8],
    ) -> Result<SegmentLocation> {
        let record_size = RECORD_HEADER_SIZE + data.len() as u64;

        if record_size > self.pool_capacity {
            return Err(NvrError::Storage(format!(
                "Segment ({record_size} bytes) > pool capacity ({} bytes)",
                self.pool_capacity
            )));
        }

        // Rotate to next pool if current one is full.
        if self.slots[self.write_idx].bytes_used + record_size > self.pool_capacity {
            self.rotate()?;
        }

        let slot = &mut self.slots[self.write_idx];
        let record_offset = POOL_HEADER_SIZE + slot.bytes_used;

        let mut file = BufWriter::new(
            OpenOptions::new()
                .write(true)
                .open(&slot.path)
                .map_err(|e| NvrError::Storage(format!("open pool {:?}: {e}", slot.path)))?,
        );
        file.seek(SeekFrom::Start(record_offset))?;

        // Write RecordHeader.
        file.write_all(RECORD_MAGIC)?;

        // camera_id: 16 bytes, zero-padded.
        let mut cam_bytes = [0u8; 16];
        let src = camera_id.as_bytes();
        cam_bytes[..src.len().min(16)].copy_from_slice(&src[..src.len().min(16)]);
        file.write_all(&cam_bytes)?;

        file.write_i64::<LittleEndian>(start_ts.timestamp())?;
        file.write_i64::<LittleEndian>(end_ts.timestamp())?;
        file.write_u32::<LittleEndian>(data.len() as u32)?;
        file.write_all(data)?;
        file.flush()?;

        let loc = SegmentLocation {
            pool_idx: self.write_idx,
            pool_id: slot.pool_id,
            record_offset,
            record_size,
        };
        slot.bytes_used += record_size;
        Ok(loc)
    }

    /// Rotate to the next pool file (ring wrap-around).
    /// If readers are active on the target pool, spins briefly (up to 5s)
    /// before proceeding to avoid data corruption during reads.
    fn rotate(&mut self) -> Result<()> {
        self.write_idx = (self.write_idx + 1) % self.slots.len();

        // Wait for any readers on the target pool to finish.
        let mut waited = 0u32;
        while self.read_counters.has_readers(self.write_idx) && waited < 50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            waited += 1;
        }
        if self.read_counters.has_readers(self.write_idx) {
            warn!(
                pool_idx = self.write_idx,
                "Rotating despite active readers (timeout after 5s)"
            );
        }

        let num_slots = self.slots.len() as u64;
        let slot = &mut self.slots[self.write_idx];
        slot.pool_id += num_slots;
        slot.bytes_used = 0;
        warn!(
            pool_idx = self.write_idx,
            pool_id = slot.pool_id,
            path = ?slot.path,
            "Pool rotated — oldest data will be overwritten"
        );
        self.write_pool_header(self.write_idx)
    }

    fn write_pool_header(&self, idx: usize) -> Result<()> {
        let slot = &self.slots[idx];
        let mut f = OpenOptions::new().write(true).open(&slot.path)
            .map_err(|e| NvrError::Storage(format!("header open {:?}: {e}", slot.path)))?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(POOL_MAGIC)?;
        f.write_u64::<LittleEndian>(slot.pool_id)?;
        f.write_i64::<LittleEndian>(Utc::now().timestamp())?;
        f.write_all(&[0u8; 40])?; // reserved
        f.flush()?;
        Ok(())
    }

    /// Return the current write pool index and approximate fill percentage.
    pub fn status(&self) -> (usize, u64, u64) {
        let slot = &self.slots[self.write_idx];
        (self.write_idx, slot.bytes_used, self.pool_capacity)
    }

    pub fn pool_count(&self) -> usize { self.slots.len() }
    pub fn pool_path(&self, idx: usize) -> &Path { &self.slots[idx].path }

    /// Read the raw MPEG-TS payload of a segment at the given location.
    /// Returns only the data bytes (skips the 40-byte RecordHeader).
    pub fn read_segment_data(&self, loc: &SegmentLocation) -> Result<Vec<u8>> {
        let slot = &self.slots[loc.pool_idx];
        let data_offset = loc.record_offset + RECORD_HEADER_SIZE;
        let data_len = (loc.record_size - RECORD_HEADER_SIZE) as usize;

        let mut f = BufReader::new(
            File::open(&slot.path)
                .map_err(|e| NvrError::Storage(format!("open pool {:?}: {e}", slot.path)))?,
        );
        f.seek(SeekFrom::Start(data_offset))?;
        let mut buf = vec![0u8; data_len];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    // ───────────────────── pool file scanning ─────────────────────────────

    /// Read the 64-byte PoolHeader from a file. Returns `(pool_id, created_at)`.
    fn read_pool_header(path: &Path) -> Result<(u64, i64)> {
        let mut f = BufReader::new(
            File::open(path)
                .map_err(|e| NvrError::Storage(format!("open {path:?}: {e}")))?,
        );
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != POOL_MAGIC {
            // Fresh or corrupt — treat as empty.
            return Ok((0, 0));
        }
        let pool_id = f.read_u64::<LittleEndian>()?;
        let created_at = f.read_i64::<LittleEndian>()?;
        Ok((pool_id, created_at))
    }

    /// Sequentially scan all RecordHeaders in a pool file.
    /// Returns a Vec of recovered records (metadata only, data is skipped).
    pub fn scan_records(
        path: &Path,
        pool_idx: usize,
        pool_id: u64,
        pool_capacity: u64,
    ) -> Result<Vec<ScannedRecord>> {
        let mut f = BufReader::new(
            File::open(path)
                .map_err(|e| NvrError::Storage(format!("scan open {path:?}: {e}")))?,
        );
        f.seek(SeekFrom::Start(POOL_HEADER_SIZE))?;

        let mut records = Vec::new();
        let mut offset = POOL_HEADER_SIZE;
        let limit = POOL_HEADER_SIZE + pool_capacity;

        while offset + RECORD_HEADER_SIZE <= limit {
            // Try to read record magic.
            let mut magic = [0u8; 4];
            if f.read_exact(&mut magic).is_err() {
                break;
            }
            if &magic != RECORD_MAGIC {
                // No more valid records (hit zero-fill or garbage).
                break;
            }

            let mut cam_bytes = [0u8; 16];
            f.read_exact(&mut cam_bytes)?;
            let camera_id = std::str::from_utf8(&cam_bytes)
                .unwrap_or("")
                .trim_end_matches('\0')
                .to_string();

            let start_ts_unix = f.read_i64::<LittleEndian>()?;
            let end_ts_unix = f.read_i64::<LittleEndian>()?;
            let data_len = f.read_u32::<LittleEndian>()? as u64;

            let record_size = RECORD_HEADER_SIZE + data_len;
            if offset + record_size > limit {
                break; // Partial record — don't trust.
            }

            let start_ts = Utc.timestamp_opt(start_ts_unix, 0)
                .single()
                .unwrap_or_else(Utc::now);
            let end_ts = Utc.timestamp_opt(end_ts_unix, 0)
                .single()
                .unwrap_or_else(Utc::now);

            records.push(ScannedRecord {
                camera_id,
                start_ts,
                end_ts,
                pool_idx,
                pool_id,
                record_offset: offset,
                record_size,
            });

            // Skip over the data payload.
            f.seek(SeekFrom::Current(data_len as i64))?;
            offset += record_size;
        }

        debug!(path = ?path, records = records.len(), "Pool scan complete");
        Ok(records)
    }

    /// Scan all pool files and return every recovered record, sorted by pool_id.
    pub fn scan_all_pools(&self) -> Result<Vec<ScannedRecord>> {
        let mut all = Vec::new();
        for (i, slot) in self.slots.iter().enumerate() {
            let recs = Self::scan_records(&slot.path, i, slot.pool_id, self.pool_capacity)?;
            all.extend(recs);
        }
        // Sort by pool_id (chronological order across rotations).
        all.sort_by_key(|r| (r.pool_id, r.record_offset));
        Ok(all)
    }
}
