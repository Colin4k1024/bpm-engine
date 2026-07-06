use thiserror::Error;

/// Runtime error types for the BPM engine.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// An engine-level error (storage, I/O, or logic failure).
    #[error("engine error: {0}")]
    Engine(#[from] anyhow::Error),
}
