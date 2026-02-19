use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::error::{NvrError, Result};

/// Top-level configuration loaded from a TOML file.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Storage configuration.
    pub storage: StorageConfig,
    /// List of cameras to record.
    pub cameras: Vec<CameraConfig>,
    /// HTTP API configuration (optional).
    #[serde(default)]
    pub api: ApiConfig,
}

/// HTTP API configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct ApiConfig {
    /// Whether to enable the HTTP API.
    #[serde(default = "default_api_enabled")]
    pub enabled: bool,
    /// Port to listen on.
    #[serde(default = "default_api_port")]
    pub port: u16,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self { enabled: default_api_enabled(), port: default_api_port() }
    }
}

fn default_api_enabled() -> bool { true }
fn default_api_port() -> u16 { 8080 }

/// Storage parameters for the global shared pool.
#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    /// Base directory where pool files are stored.
    pub base_path: PathBuf,
    /// Size of each pre-allocated pool file in megabytes.
    /// All cameras share the same pool files (sequential I/O, HDD friendly).
    #[serde(default = "default_chunk_size_mb")]
    pub chunk_size_mb: u64,
    /// Total number of pool files in the ring buffer.
    /// When all pools are full the oldest is overwritten.
    #[serde(default = "default_max_chunks")]
    pub max_pools: usize,
    /// Duration of a single video segment in seconds.
    #[serde(default = "default_segment_duration")]
    pub segment_duration_secs: u64,
    /// Bounded channel capacity for the global writer queue.
    #[serde(default = "default_writer_queue")]
    pub writer_queue_size: usize,
}

/// Per-camera configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CameraConfig {
    /// Unique identifier used for directory/file naming.
    pub id: String,
    /// Human-readable label shown in status output.
    pub name: String,
    /// RTSP (or HTTP) URL of the camera stream.
    pub url: String,
    /// Optional reconnection attempt limit (0 = unlimited).
    #[serde(default)]
    pub max_reconnect_attempts: u32,
}

fn default_chunk_size_mb() -> u64 { 512 }
fn default_max_chunks() -> usize { 20 }
fn default_segment_duration() -> u64 { 60 }
fn default_writer_queue() -> usize { 256 }

impl Config {
    /// Load configuration from a TOML file at `path`.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| NvrError::Config(format!("Cannot read config file: {e}")))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| NvrError::Config(format!("Invalid TOML: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.cameras.is_empty() {
            return Err(NvrError::Config("No cameras defined".into()));
        }
        if self.storage.chunk_size_mb == 0 {
            return Err(NvrError::Config("chunk_size_mb must be > 0".into()));
        }
        if self.storage.max_pools == 0 {
            return Err(NvrError::Config("max_pools must be > 0".into()));
        }
        if self.storage.segment_duration_secs == 0 {
            return Err(NvrError::Config("segment_duration_secs must be > 0".into()));
        }
        Ok(())
    }
}
