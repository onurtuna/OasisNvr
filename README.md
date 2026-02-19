# NVR — Network Video Recorder

> **⚠️ License Notice:** This software is provided for **non-commercial use only**. Commercial use is strictly prohibited. If you use, modify, or redistribute this software, you must provide **proper attribution** to the original author. See [License](#license) below.

A high-performance Network Video Recorder written in Rust. Records RTSP streams from multiple IP cameras into pre-allocated binary pool files using a ring buffer strategy. Designed for HDD-based storage with sequential I/O optimization — **no SSD required**.

## Features

- **Multi-camera support** — record from unlimited RTSP cameras simultaneously
- **Global shared writer** — single sequential I/O stream eliminates HDD seek storms
- **Ring buffer storage** — fixed pool files are overwritten cyclically, no manual cleanup needed
- **Persistent index** — segment index rebuilt from pool files on restart, no data loss
- **HTTP API** — status, segment listing, export, and live streaming via REST endpoints
- **LL-HLS live playback** — watch live or recorded video in any HLS-compatible player (VLC, Safari, HLS.js)
- **VOD playback** — export any time range as `.ts` file or stream via HLS
- **Pool read safety** — per-pool atomic read locks prevent data corruption during concurrent read/write
- **GStreamer pipeline** — robust RTSP ingestion with automatic reconnection
- **Async architecture** — built on Tokio for efficient concurrency

## Architecture

```
cam1 ──┐                                                    ┌─ /api/status
cam2 ──┤  mpsc channel  →  GlobalChunkWriter  →  pool_XXX   ├─ /api/list
cam3 ──┤                         │                .bin      ├─ /api/export
cam4 ──┘                         ▼                          ├─ /api/hls/.../live.m3u8
                           SegmentIndex (RAM)               └─ /api/hls/.../segment/N.ts
                                 ▲
                      rebuilt from pool files on startup
```

All cameras share a single write queue. The writer appends records sequentially into pre-allocated pool files — the HDD head only moves forward. The HTTP API reads segments directly from pool files using per-pool read guards.

## Prerequisites

- **Rust** 1.70+
- **GStreamer** with H.264 plugins

```bash
# macOS (My implementation environment)
brew install gstreamer gst-plugins-good gst-plugins-bad gst-plugins-ugly

# Ubuntu / Debian (Not Tested)
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad
```

## Quick Start

```bash
# Build
cargo build --release

# Copy and edit config
cp config.example.toml config.toml
# Edit config.toml with your camera RTSP URLs

# Start recording (HTTP API starts automatically on port 8080)
cargo run --release -- record --config config.toml
```

## HTTP API

While recording, the HTTP API is available at `http://localhost:8080`:

| Endpoint | Description |
|---|---|
| `GET /api/status` | System status — pools, segments, cameras (JSON) |
| `GET /api/list?camera=cam1` | Segment list for a camera (JSON) |
| `GET /api/export?camera=cam1&from=...&to=...` | Download `.ts` file for a time range |
| `GET /api/hls/{camera}/live.m3u8` | LL-HLS live playlist (low-latency) |
| `GET /api/hls/{camera}/vod.m3u8?from=...&to=...` | VOD playlist for a time range |
| `GET /api/hls/{camera}/segment/{id}.ts` | Individual segment data |

### Examples

```bash
# System status
curl http://localhost:8080/api/status | jq

# List segments for a camera
curl "http://localhost:8080/api/list?camera=cam1" | jq

# Export 1 hour of video
curl -o kayit.ts "http://localhost:8080/api/export?camera=cam1&from=2026-02-19T14:00:00&to=2026-02-19T15:00:00"

# Watch live in VLC
vlc http://localhost:8080/api/hls/cam1/live.m3u8

# Watch recorded video in VLC
vlc "http://localhost:8080/api/hls/cam1/vod.m3u8?from=2026-02-19T14:00:00&to=2026-02-19T15:00:00"
```

## CLI Commands

```bash
# Start recording + HTTP API
nvr record --config config.toml

# Offline status (scans pool files)
nvr status --config config.toml

# Offline segment listing
nvr list --config config.toml --camera cam1

# Offline export to file
nvr export --config config.toml --camera cam1 \
    --from "2026-02-19T14:00:00" --to "2026-02-19T15:00:00" -o kayit.ts
```

## Configuration

```toml
[storage]
base_path = "/path/to/storage"    # Where pool files are stored
chunk_size_mb = 512               # Size of each pool file (MB)
max_pools = 20                    # Number of pool files (ring depth)
segment_duration_secs = 60        # Segment duration
writer_queue_size = 256           # Writer channel buffer size

[api]
enabled = true                    # Enable HTTP API (default: true)
port = 8080                       # API port (default: 8080)

[[cameras]]
id = "cam1"
name = "Front Door"
url = "rtsp://user:pass@192.168.1.10:554/stream1"
max_reconnect_attempts = 0        # 0 = unlimited
```

### Storage Calculation

| Cameras | Pool Size | Pools | Total   | Est. Duration (1 Mbps/cam) |
|---------|-----------|-------|---------|----------------------------|
| 4       | 512 MB    | 20    | 10 GB   | ~5.5 hours                 |
| 4       | 1024 MB   | 40    | 40 GB   | ~22 hours                  |
| 16      | 512 MB    | 80    | 40 GB   | ~5.5 hours                 |
| 150     | 1024 MB   | 100   | 100 GB  | ~1.5 hours                 |

## Safety & Persistence

- **Index survives restarts** — pool files are scanned on startup, segment index rebuilt from embedded RecordHeaders
- **No extra disk I/O** — index lives in RAM, no separate index file written during recording
- **Safe concurrent reads** — per-pool atomic counters prevent rotation during active reads (RAII guards)
- **Rotation timeout** — writer waits up to 5s for readers before rotating, ensuring read integrity

## License

This project is licensed under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/).

- ✅ Personal and educational use
- ✅ Modification and redistribution with attribution
- ❌ Commercial use without written permission

© 2026 Onur Tuna. All rights reserved.
