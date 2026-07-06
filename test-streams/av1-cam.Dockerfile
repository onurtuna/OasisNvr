# Serves a synthetic AV1-over-RTSP test stream, for exercising OasisNvr's
# AV1 auto-detect path (see src/camera.rs). Acts as an RTSP *server*
# (like a real camera) rather than pushing to MediaMTX, because neither
# ffmpeg nor Debian's gst-plugins-bad can payload/push AV1 over RTSP:
#   - ffmpeg's RTP muxer has no AV1 support at all (checked libavformat in
#     the 6.1 and 7.1 jrottenberg/ffmpeg images: rtpmap table has
#     H264/H265/VP8/VP9 but no AV1).
#   - GStreamer's rtspclientsink (the client-push element) isn't compiled
#     into Debian's gst-plugins-bad package.
# rtpav1pay itself is only shipped as gst-plugins-rs Rust source, so it's
# built from source here — same as rtpav1depay in the project's own
# Dockerfile.
FROM rust:1-slim-bookworm AS rtp-builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    git \
    libssl-dev \
    libgstreamer1.0-dev \
    libgstreamer-plugins-base1.0-dev \
    libgstreamer-plugins-bad1.0-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-c
RUN git clone --depth 1 https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs.git /tmp/gst-plugins-rs \
    && cd /tmp/gst-plugins-rs \
    && cargo cinstall -p gst-plugin-rtp --release --prefix=/usr --libdir=/usr/lib/x86_64-linux-gnu \
    && rm -rf /tmp/gst-plugins-rs

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    python3-gi \
    gir1.2-gst-rtsp-server-1.0 \
    gstreamer1.0-tools \
    gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad \
    && rm -rf /var/lib/apt/lists/*

# Rust-built rtpav1pay (see build stage comment above)
COPY --from=rtp-builder /usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstrsrtp.so /usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstrsrtp.so

COPY test-streams/av1_rtsp_server.py /av1_rtsp_server.py

ENTRYPOINT ["python3", "/av1_rtsp_server.py"]
