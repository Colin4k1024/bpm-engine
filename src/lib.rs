//! Rust BPM Engine — library for embedding the BPM runtime.
//!
//! Re-exports core workspace crates and provides optional observability support.
//! Enable the `observability` feature for Prometheus metrics.

pub use bpm_engine_adapter_memory;
pub use bpm_engine_bpmn;
pub use bpm_engine_core;
pub use bpm_engine_runtime;
pub use bpm_engine_storage;

#[cfg(feature = "observability")]
pub mod metrics;
