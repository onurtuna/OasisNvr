# Stage 1: Build
FROM rust:1-slim-bookworm AS builder

# Install build dependencies required by gstreamer
RUN apt-get update && apt-get install -y \
    pkg-config \
    git \
    libssl-dev \
    libgstreamer1.0-dev \
    libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev \
    && rm -rf /var/lib/apt/lists/*

# rtpav1depay/rtpav1pay (needed for AV1-over-RTSP cameras) aren't packaged as a
# compiled plugin anywhere in Debian — they only exist as gst-plugins-rs Rust
# source — so build that one plugin from source here and ship the resulting
# .so in the runtime image below. Kept above `COPY . .` so it's cached
# independently of application source changes.
RUN cargo install cargo-c
RUN git clone --depth 1 https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs.git /tmp/gst-plugins-rs \
    && cd /tmp/gst-plugins-rs \
    && cargo cinstall -p gst-plugin-rtp --release --prefix=/usr --libdir=/usr/lib/x86_64-linux-gnu \
    && rm -rf /tmp/gst-plugins-rs

# Create a new empty shell project
WORKDIR /usr/src/oasis_nvr
COPY . .

# Build for release
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies for GStreamer
RUN apt-get update && apt-get install -y \
    gstreamer1.0-tools \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly \
    gstreamer1.0-libav \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary from the builder environment
COPY --from=builder /usr/src/oasis_nvr/target/release/oasis_nvr /usr/local/bin/oasis_nvr

# Copy the Rust-built RTP plugin (rtpav1depay/rtpav1pay) — see build stage comment above
COPY --from=builder /usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstrsrtp.so /usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstrsrtp.so

# Copy frontend directory
COPY frontend /app/frontend

# Provide a volume for recordings
VOLUME ["/app/recordings"]

# Expose HTTP API port
EXPOSE 8080

# Specify entrypoint
ENTRYPOINT ["/usr/local/bin/oasis_nvr", "record", "--config", "/app/config.toml"]
