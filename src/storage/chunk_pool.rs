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
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use chrono::{DateTime, Utc};
use tracing::{info, warn};

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
}

impl ChunkPool {
    /// Open (or create + pre-allocate) all pool files.
    pub fn open(base_path: &Path, pool_size_bytes: u64, max_pools: usize) -> Result<Self> {
        std::fs::create_dir_all(base_path)
            .map_err(|e| NvrError::Storage(format!("Cannot create storage dir: {e}")))?;

        let mut slots = Vec::with_capacity(max_pools);
        for i in 0..max_pools {
            let path = base_path.join(format!("pool_{:03}.bin", i));
            if !path.exists() {
                let total = POOL_HEADER_SIZE + pool_size_bytes;
                let f = File::create(&path)?;
                f.set_len(total)
                    .map_err(|e| NvrError::Storage(format!("preallocate {path:?}: {e}")))?;
                info!(pool = i, path = ?path, size_mb = total / 1_048_576, "Pre-allocated pool file");
            }
            slots.push(PoolSlot { path, pool_id: i as u64, bytes_used: 0 });
        }

        let pool = ChunkPool {
            base_path: base_path.to_path_buf(),
            pool_capacity: pool_size_bytes,
            slots,
            write_idx: 0,
        };
        // Write header into the first pool slot.
        pool.write_pool_header(0)?;
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
    fn rotate(&mut self) -> Result<()> {
        self.write_idx = (self.write_idx + 1) % self.slots.len();
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
}
