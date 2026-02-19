use thiserror::Error;

#[derive(Debug, Error)]
pub enum NvrError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("GStreamer error: {0}")]
    GStreamer(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Camera '{id}' connection failed: {reason}")]
    CameraConnection { id: String, reason: String },

    #[error("Chunk storage error: {0}")]
    Storage(String),

    #[error("Index error: {0}")]
    Index(String),

    #[error("Camera '{id}' not found")]
    CameraNotFound { id: String },
}

pub type Result<T> = std::result::Result<T, NvrError>;
