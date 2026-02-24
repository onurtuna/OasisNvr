# Stage 1: Build
FROM rust:1-slim-bookworm AS builder

# Install build dependencies required by gstreamer
RUN apt-get update && apt-get install -y \
    pkg-config \
    libgstreamer1.0-dev \
    libgstreamer-plugins-base1.0-dev \
    && rm -rf /var/lib/apt/lists/*

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

# Copy frontend directory
COPY frontend /app/frontend

# Provide a volume for recordings
VOLUME ["/app/recordings"]

# Expose HTTP API port
EXPOSE 8080

# Specify entrypoint
ENTRYPOINT ["/usr/local/bin/oasis_nvr", "record", "--config", "/app/config.toml"]
