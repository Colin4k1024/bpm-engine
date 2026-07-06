use thiserror::Error;

/// Core error types for the BPM engine.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Requested resource (process, token, instance) was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Token or instance is in an invalid state for the requested operation.
    #[error("invalid state: {0}")]
    InvalidState(String),
}
