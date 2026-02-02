//! Shared app state for REST API.

use crate::replay::ReplaySessions;
use bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine_runtime::BpmEngine;
use std::sync::Arc;

/// Shared app state. Pass Arc<AppState> to router.
pub struct AppState {
    pub engine: BpmEngine,
    pub repo: Arc<MemoryRepo>,
    pub def_store: Arc<ProcessDefStore>,
    /// Ephemeral replay sessions (session_id -> ReplaySession). Not persisted.
    pub replay_sessions: Arc<ReplaySessions>,
}
