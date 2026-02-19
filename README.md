# NVR — Network Video Recorder

> **⚠️ License Notice:** This software is provided for **non-commercial use only**. Commercial use is strictly prohibited. If you use, modify, or redistribute this software, you must provide **proper attribution** to the original author. See [License](#license) below.

A high-performance Network Video Recorder written in Rust. Records RTSP streams from multiple IP cameras into pre-allocated binary pool files using a ring buffer strategy. Designed for HDD-based storage with sequential I/O optimization.

## Features

- **Multi-camera support** — record from unlimited RTSP cameras simultaneously
- **Global shared writer** — single sequential I/O stream eliminates HDD seek storms
- **Ring buffer storage** — fixed pool files are overwritten cyclically, no manual cleanup needed
- **GStreamer pipeline** — robust RTSP ingestion with automatic reconnection
- **Async architecture** — built on Tokio for efficient concurrency
- **Zero-copy rotation** — oldest pool is overwritten in place, files are never deleted

## Architecture

```
cam1 ──┐
cam2 ──┤  mpsc channel  →  GlobalChunkWriter  →  pool_000.bin
cam3 ──┤                                      →  pool_001.bin
cam4 ──┘                                      →  ...
                                               ↓
                                         SegmentIndex
```

All cameras share a single write queue. The writer appends records sequentially into pre-allocated pool files — the HDD head only moves forward.

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

# Start recording
cargo run --release -- record --config config.toml

# Check status
cargo run --release -- status --config config.toml
```

## Configuration

```toml
[storage]
base_path = "/path/to/storage"    # Where pool files are stored
chunk_size_mb = 512               # Size of each pool file (MB)
max_pools = 20                    # Number of pool files (ring depth)
segment_duration_secs = 60        # Segment duration
writer_queue_size = 256           # Writer channel buffer size

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

## License

This project is licensed under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/).

- ✅ Personal and educational use
- ✅ Modification and redistribution with attribution
- ❌ Commercial use without written permission

© 2026 Onur Tuna. All rights reserved.
