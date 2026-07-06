# NVR — Network Video Recorder

> **⚠️ License Notice:** This software is provided for **non-commercial use only**. Commercial use is strictly prohibited. If you use, modify, or redistribute this software, you must provide **proper attribution** to the original author. See [License](#license) below.

A high-performance Network Video Recorder written in Rust. Records RTSP streams from multiple IP cameras into pre-allocated binary pool files using a ring buffer strategy. Designed for HDD-based storage with sequential I/O optimization — **no SSD required**.

Optimized for longer hardware life.

## Features

- **Multi-camera support** — record from unlimited RTSP cameras simultaneously
- **Dynamic camera management** — add or remove cameras at runtime via API, no restart needed
- **Global shared writer** — single sequential I/O stream eliminates HDD seek storms
- **Ring buffer storage** — fixed pool files are overwritten cyclically, no manual cleanup needed
- **Persistent index** — segment index rebuilt from pool files on restart, no data loss
- **HTTP API** — status, segment listing, export, and live streaming via REST endpoints
- **Rich Web Interface** — built-in offline-capable SPA dashboard for live viewing and VOD playback natively accessible at `http://localhost:8080/`
- **CMAF Support** — watch live or recorded video in any player
- **VOD playback** — export any time range as `.mp4` file or stream
- **Pool read safety** — per-pool atomic read locks prevent data corruption during concurrent read/write
- **GStreamer pipeline** — robust RTSP ingestion with automatic reconnection
- **Async architecture** — built on Tokio for efficient concurrency

## Architecture

```
cam1 ──┐                                                    ┌─ /api/status
cam2 ──┤  mpsc channel  →  GlobalChunkWriter  →  pool_XXX   ├─ /api/list
cam3 ──┤                         │                .bin      ├─ /api/export
cam4 ──┘                         ▼                          ├─ /api/hls/.../live.m3u8
  ↕                        SegmentIndex (RAM)               ├─ /api/hls/.../vod.m3u8
POST/DELETE                      ▲                          ├─ /api/dash/.../manifest.mpd
/api/cameras          rebuilt from pool files on startup    ├─ /api/cameras (GET/POST/DELETE)
                                                            └─ /api/login
```

All cameras share a single write queue. The writer appends records sequentially into pre-allocated pool files — the HDD head only moves forward. The HTTP API reads segments directly from pool files using per-pool read guards.

## Comparison with Other NVRs

| Feature / Aspect | Oasis NVR (This Project) | Moonfire NVR | Frigate NVR |
|-----------------|-------------------------|--------------|-------------|
| **Language** | Rust | Rust | TypeScript/Python |
| **Primary Focus** | High-throughput 24/7 continuous recording | Precise time-based indexing | Smart home integration & Object detection |
| **Recording Method** | Ring buffer → Pre-allocated pool files | H.264 stream to HDD, SQLite metadata | 10-second MP4 segments |
| **Database** | Embedded RAM index | SQLite (on SSD) | SQLite / PostgreSQL |
| **SSD Requirement** | Optional (Zero-overhead on cheap HDDs) | Recommended for Metadata | Recommended, not strict |
| **HDD Seek Optimization** | ✅ One-way sequential write | Partial | ❌ None |
| **HDD Friendly?** | ✅ Yes (Zero fragmentation, Sequential I/O) | ⚠️ Moderate (Frequent small writes) | ❌ No (Designed for SSDs) |
| **AI / Object Detection** | ❌ None (Raw streams only) | ❌ None | ✅ Coral, GPU |
| **Live Stream** | ✅ CMAF | Partial | ✅ RTSP/WebRTC |
| **VOD/Export** | ✅ MP4 or CMAF stream | ✅ MP4 | ✅ MP4 |
| **AV1 Camera Support** | ✅ Auto-detected per camera, recorded natively (no re-encode) | ❌ H.264 only (no H.265 either) | ⚠️ Only via optional HW transcode of recordings, not native camera ingest |
| **Runtime Camera Management**| ✅ Add/remove via API without restart | ❌ No | ❌ No |
| **Advantages** | Ultimate performance, 0-config storage cleanup, extremely lightweight. | Mature, precise seeking, frame-level granularity. | Powerful automation, rich smart-alerts, AI integration. |
| **License** | CC BY-NC 4.0 (Non-commercial) | Apache 2.0 | Apache 2.0 |

## Prerequisites

- **Rust** 1.70+
- **GStreamer** with H.264 and AV1 plugins. Codec is auto-detected per camera from
  the RTSP stream, so both can be used at once.

```bash
# macOS (My implementation environment)
brew install gstreamer gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-plugins-rs

# Ubuntu / Debian (Not Tested)
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad
```

> AV1 needs `rtpav1depay`, which isn't packaged by Debian/Ubuntu at all — it only
> exists as [gst-plugins-rs](https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs)
> Rust source. On Homebrew it's covered by the `gst-plugins-rs` formula above; on
> Debian/Ubuntu (and in the project's own `Dockerfile`) it's built from source via
> `cargo-c` — see the `Dockerfile` for the exact build/install commands.

> **On Windows?** Instead of natively installing GStreamer (MSVC) and building with Rust, use the [Windows Setup (Docker)](#windows-setup-docker--recommended) section below. The only requirement is Docker Desktop.

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

## Windows Setup (Docker — Recommended)

Natively building the `gstreamer-rs` crate on Windows (MSVC toolchain + GStreamer development kit + `pkg-config`/environment variable setup) is quite tedious. Instead, the project already ships a `Dockerfile` and `docker-compose.yml` — GStreamer and Rust come ready-made inside the container, so **you don't need to install anything on Windows** except Docker Desktop.

### 1. Install and start Docker Desktop

1. Download and install Docker Desktop from [docker.com/products/docker-desktop](https://www.docker.com/products/docker-desktop/) (leave the **WSL 2 backend** option checked during setup — it's the default).
2. Restart Windows after installation if prompted.
3. Open the Docker Desktop app and wait until you see **"Engine running"** in the bottom left. On first launch, Docker automatically sets up its own WSL2 subsystems (`docker-desktop`, `docker-desktop-data`) — you don't need to install an extra Linux distro.
4. To verify, in a terminal:
   ```powershell
   docker version
   docker compose version
   ```
   Both should return version info (not an error).

### 2. Prepare the project

Open a PowerShell in the project folder (this repo):

```powershell
# Copy the example config
copy config.example.toml config.toml

# Edit it with your camera RTSP URLs
notepad config.toml
```

In `config.toml`, update at least the following:

- Replace the `url = "rtsp://..."` addresses in the `[[cameras]]` blocks with your own cameras (format `rtsp://user:pass@ip:port/path` if there's a username/password).
- `storage.base_path` — inside the container this can always stay `./recordings`; which Windows disk it actually writes to is set via `volumes` in `docker-compose.yml` (see below).

To decide which Windows disk/folder recordings are written to, open `docker-compose.yml` and change the `./recordings` path if needed (e.g. to use a separate HDD):

```yaml
volumes:
  - D:/nvr-recordings:/app/recordings   # e.g. a separate HDD/folder
  - ./config.toml:/app/config.toml:ro
```

### 3. Build & run

```powershell
# Build the image and start in the background on first setup
docker compose up --build -d

# Watch the logs
docker compose logs -f

# Stop
docker compose down

# Rebuild and restart after changing config or code
docker compose up --build -d
```

### 4. Use it

Go to `http://localhost:8080/` in your browser — you should see the dashboard. Windows Firewall may ask for permission on the first connection — click "Allow".

### If you want to develop natively (Rust + GStreamer)

If you want to run directly on Windows with `cargo run`/`cargo build` (for debugging), you'll additionally need the following — this path is much more complex than Docker:

1. [Install Rust](https://rustup.rs/) (MSVC toolchain, `rustup-init.exe`'s default choice).
2. [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) — the "Desktop development with C++" workload (required for the MSVC linker).
3. [GStreamer MSVC development installer](https://gstreamer.freedesktop.org/download/#windows) — install both the **runtime** and **development** MSVC 64-bit packages.
4. After installation, set the environment variables (set them via System Environment Variables to make them permanent):
   ```powershell
   $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\gstreamer\1.0\msvc_x86_64\"
   $env:PATH += ";$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\bin"
   $env:PKG_CONFIG_PATH = "$env:GSTREAMER_1_0_ROOT_MSVC_X86_64\lib\pkgconfig"
   ```
5. Open a new terminal and verify: `gst-launch-1.0.exe --version`
6. Then you can follow the usual `cargo build --release` / `cargo run --release -- record --config config.toml` steps.

This path is untested (the Docker method is recommended); if you run into issues, most `pkg-config` errors are caused by the environment variables above being missing or incorrect.

## Built-in Web Interface

The NVR comes with a built-in rich web dashboard accessible directly from your browser:
* **Dashboard URL**: `http://localhost:8080/`
* Features: System metrics, multi-camera live view, and historical segment VOD playback.
* **Offline Ready**: The web interface does not rely on external CDNs or internet access once downloaded.

While recording, the HTTP API is also available at `http://localhost:8080`:

| Endpoint | Description |
|---|---|
| `GET /api/status` | System status — pools, segments, cameras (JSON) |
| `GET /api/list?camera=cam1` | Segment list for a camera (JSON) |
| `GET /api/export?camera=cam1&from=...&to=...` | Download `.mp4` file for a time range |
| `GET /api/hls/{camera}/live.m3u8` | HLS live playlist (LL-HLS, supports `?_HLS_msn=N` blocking reload) |
| `GET /api/hls/{camera}/vod.m3u8?from=...&to=...` | HLS VOD playlist for a time range |
| `GET /api/hls/{camera}/segment/mp4/{id}` | Individual segment data (fMP4) |
| `GET /api/hls/{camera}/player` | 🖥 Live video player (browser) |
| `GET /api/hls/{camera}/vod/player?from=...&to=...` | 🖥 VOD video player (browser) |
| `GET /api/dash/{camera}/manifest.mpd` | DASH live manifest |
| `GET /api/dash/{camera}/manifest.mpd?from=...&to=...` | DASH VOD manifest for a time range |
| `GET /api/cameras` | List active and historical cameras |
| `POST /api/cameras` | Add a camera at runtime (JSON body) |
| `DELETE /api/cameras/{id}` | Remove a camera at runtime |
| `POST /api/login` | Web UI login (`{"username": "...", "password": "..."}`) |

### Examples

```bash
# ── Watch live in any browser ─────────────────────────────────────
open http://localhost:8080/api/hls/cam1/player

# ── Watch recorded video in any browser (1 minute) ────────────────
open "http://localhost:8080/api/hls/cam1/vod/player?from=2026-02-19T23:00:00&to=2026-02-19T23:01:00"

# ── VLC playback ──────────────────────────────────────────────────
vlc http://localhost:8080/api/hls/cam1/live.m3u8
vlc "http://localhost:8080/api/hls/cam1/vod.m3u8?from=2026-02-19T23:00:00&to=2026-02-19T23:01:00"

# ── DASH playback (live and VOD manifest) ─────────────────────────
vlc http://localhost:8080/api/dash/cam1/manifest.mpd
vlc "http://localhost:8080/api/dash/cam1/manifest.mpd?from=2026-02-19T23:00:00&to=2026-02-19T23:01:00"

# ── System status ─────────────────────────────────────────────────
curl http://localhost:8080/api/status | jq

# ── List segments ─────────────────────────────────────────────────
curl "http://localhost:8080/api/list?camera=cam1" | jq

# ── Export 1 hour to file ─────────────────────────────────────────
curl -o kayit.ts "http://localhost:8080/api/export?camera=cam1&from=2026-02-19T14:00:00&to=2026-02-19T15:00:00"

# ── Camera management (hot add/remove) ───────────────────────────
curl http://localhost:8080/api/cameras | jq

curl -X POST http://localhost:8080/api/cameras \
  -H "Content-Type: application/json" \
  -d '{"id":"cam5","name":"Garden","url":"rtsp://user:pass@192.168.1.15:554/stream1","max_reconnect_attempts":0}'

curl -X DELETE http://localhost:8080/api/cameras/cam5
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
