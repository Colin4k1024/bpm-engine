//! Rust BPM Engine — library for embedding the BPM runtime.
//!
//! Use this crate to run workflows from your binary or examples:
//! - Define a [ProcessDefinition](model::ProcessDefinition) (nodes, edges, start).
//! - Create an [EngineContext](engine::EngineContext) with repos (e.g. [InstanceRepo](persistence::sqlite::InstanceRepo)).
//! - Run [BpmEngine::run](engine::BpmEngine::run) with [EngineEvent](engine::EngineEvent) (e.g. ProcessStarted, UserTaskCompleted).

pub mod api;
pub mod bpmn;
pub mod cluster;
pub mod dsl;
pub mod domain;
pub mod engine;
pub mod events;
pub mod legacy_engine;
pub mod model;
pub mod persistence;
pub mod recovery;
pub mod service;

/// Legacy db API: re-export from persistence so existing callers still work.
pub mod db {
    pub use crate::persistence::sqlite::InstanceRepo;
}
