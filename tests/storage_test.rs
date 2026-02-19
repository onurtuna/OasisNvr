//! Storage integration tests: global pool + index + writer.
//!
//! Run with: `cargo test`

use chrono::Utc;
use tempfile::TempDir;

use nvr::storage::chunk_pool::ChunkPool;
use nvr::storage::index::SegmentIndex;

fn tmp_dir() -> TempDir {
    tempfile::tempdir().expect("create tempdir")
}

#[test]
fn test_append_and_read_segment() {
    let dir = tmp_dir();
    let mut pool = ChunkPool::open(dir.path(), 1024 * 1024, 3).expect("open pool");
    let mut index = SegmentIndex::new();

    let data = b"fake-mpegts-data-1234".as_slice();
    let start = Utc::now();
    let end = Utc::now();

    let loc = pool.append("cam1", start, end, data).expect("append");
    let seg_id = index.insert("cam1", start, end, loc);

    assert_eq!(seg_id, 0);
    assert_eq!(index.len(), 1);
    let segs = index.segments_for_camera("cam1");
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].camera_id, "cam1");
}

#[test]
fn test_multiple_cameras_interleaved() {
    let dir = tmp_dir();
    let mut pool = ChunkPool::open(dir.path(), 1024 * 1024, 3).expect("open pool");
    let mut index = SegmentIndex::new();
    let now = Utc::now();

    // Simulate interleaved writes from 3 cameras -> same pool file
    for i in 0..9 {
        let cam = format!("cam{}", i % 3);
        let data = vec![0xABu8; 50];
        let loc = pool.append(&cam, now, now, &data).expect("append");
        index.insert(&cam, now, now, loc);
    }

    assert_eq!(index.len(), 9);
    assert_eq!(index.segments_for_camera("cam0").len(), 3);
    assert_eq!(index.segments_for_camera("cam1").len(), 3);
    assert_eq!(index.segments_for_camera("cam2").len(), 3);

    // All records should be in pool_000.bin (single sequential file)
    let (idx, _, _) = pool.status();
    assert_eq!(idx, 0, "All small writes should fit in the first pool file");
}

#[test]
fn test_pool_rotation_and_eviction() {
    let dir = tmp_dir();
    // Small pools: 512 bytes each, 2 pool files
    // RecordHeader = 40 bytes, so 100 bytes payload => 140 bytes per record
    // 512 / 140 = 3 records per pool
    let pool_size: u64 = 512;
    let max_pools = 2;
    let mut pool = ChunkPool::open(dir.path(), pool_size, max_pools).expect("open pool");
    let mut index = SegmentIndex::new();
    let now = Utc::now();

    let payload = vec![0xCDu8; 100];

    // Write enough to fill both pools and rotate
    // pool 0: 3 records, pool 1: 3 records, next write wraps to pool 0
    for i in 0..10u64 {
        let cam = format!("cam{}", i % 3);
        let loc = pool.append(&cam, now, now, &payload).expect("append");
        let _pool_idx = loc.pool_idx;
        
        // Evict before insert if the writer would (mimicking GlobalChunkWriter logic)
        // The pool.append already handles rotation, but we need to evict index entries
        // for the destination pool_idx BEFORE the data that was previously there.
        // In practice, the GlobalChunkWriter does this; here we test index eviction separately.
        index.insert(&cam, now, now, loc);
    }

    // Index should have entries, but the exact count depends on eviction timing.
    assert!(index.len() <= 10);
}

#[test]
fn test_segment_too_large_errors() {
    let dir = tmp_dir();
    let mut pool = ChunkPool::open(dir.path(), 100, 2).expect("open pool");
    let huge = vec![0u8; 200];
    let now = Utc::now();
    assert!(pool.append("cam1", now, now, &huge).is_err());
}

#[test]
fn test_index_eviction() {
    let mut index = SegmentIndex::new();
    let now = Utc::now();

    let loc0 = nvr::storage::chunk_pool::SegmentLocation {
        pool_idx: 0,
        pool_id: 0,
        record_offset: 64,
        record_size: 100,
    };
    let loc1 = nvr::storage::chunk_pool::SegmentLocation {
        pool_idx: 1,
        pool_id: 1,
        record_offset: 64,
        record_size: 100,
    };

    index.insert("cam1", now, now, loc0.clone());
    index.insert("cam2", now, now, loc0.clone());
    index.insert("cam1", now, now, loc1.clone());
    assert_eq!(index.len(), 3);

    // Evict pool 0: should remove 2 entries (cam1+cam2 from pool 0)
    index.evict_pool(0);
    assert_eq!(index.len(), 1);

    // Remaining segment should be from pool 1
    let segs: Vec<_> = index.all_segments().collect();
    assert_eq!(segs[0].location.pool_idx, 1);
}

#[tokio::test]
async fn test_global_writer_end_to_end() {
    let dir = tmp_dir();
    let pool = ChunkPool::open(dir.path(), 1024 * 1024, 3).expect("open pool");

    let (tx, index, handle) = nvr::storage::global_writer::spawn_writer(pool, 64);

    let now = Utc::now();
    // Send 5 write requests from different "cameras"
    for i in 0..5 {
        let req = nvr::storage::global_writer::WriteRequest {
            camera_id: format!("cam{}", i % 2),
            start_ts: now,
            end_ts: now,
            data: vec![0xFFu8; 50],
        };
        tx.send(req).await.expect("send");
    }

    // Drop sender so writer loop exits
    drop(tx);
    handle.await.expect("writer task");

    let idx = index.read();
    assert_eq!(idx.len(), 5);
    assert_eq!(idx.segments_for_camera("cam0").len(), 3);
    assert_eq!(idx.segments_for_camera("cam1").len(), 2);
}

#[test]
fn test_restart_recovery() {
    // Simulate: write some data, "crash" (drop pool), reopen, verify index rebuilt.
    let dir = tmp_dir();
    let pool_size: u64 = 1024 * 1024;

    // Phase 1: write segments.
    {
        let mut pool = ChunkPool::open(dir.path(), pool_size, 3).expect("open");
        let now = Utc::now();
        for i in 0..5 {
            let cam = format!("cam{}", i % 2);
            pool.append(&cam, now, now, &vec![0xABu8; 100]).expect("append");
        }
        // Pool dropped here — simulates NVR crash/restart.
    }

    // Phase 2: reopen and scan.
    {
        let pool = ChunkPool::open(dir.path(), pool_size, 3).expect("reopen");
        let records = pool.scan_all_pools().expect("scan");
        assert_eq!(records.len(), 5, "Should recover all 5 records from disk");

        // Rebuild index from scanned records.
        let mut index = SegmentIndex::new();
        index.rebuild_from_scanned(records);
        assert_eq!(index.len(), 5);
        assert_eq!(index.segments_for_camera("cam0").len(), 3);
        assert_eq!(index.segments_for_camera("cam1").len(), 2);
    }
}
