//! Rust BPM Engine — library for embedding the BPM runtime.
//!
//! Re-exports core workspace crates and provides optional observability support.
//! Enable the `observability` feature for Prometheus metrics.
//!
//! Three entry points share one compiler and one set of semantics (ADR-006):
//! the typed Rust builder ([`bpm_engine_bpmn::builder`]), the JSON DSL
//! ([`bpm_engine_dsl`]) and BPMN 2.0 XML ([`bpm_engine_bpmn`]). The public
//! surface of this facade is pinned by `tests/api_surface.rs` — update that
//! test whenever the facade changes (semver tripwire).
//!
//! The worker SDK is re-exported behind the default-on `worker-sdk` feature;
//! embedders who do not run workers can build with
//! `default-features = false` to drop its HTTP stack.

pub use bpm_engine_adapter_memory;
pub use bpm_engine_bpmn;
pub use bpm_engine_core;
pub use bpm_engine_dsl;
pub use bpm_engine_runtime;
pub use bpm_engine_storage;

#[cfg(feature = "worker-sdk")]
pub use bpm_engine_worker_sdk;

#[cfg(feature = "observability")]
pub mod metrics;
