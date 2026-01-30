use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("engine error: {0}")]
    Engine(#[from] anyhow::Error),
}
