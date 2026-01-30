//! Saga (compensation) model and stubs (design: saga.md).
//! CompensationRecord, TokenMode, TokenFailedHandler, SagaCoordinator to be wired when integrating.

/// Token mode: forward or compensation (design: saga.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenMode {
    Forward,
    Compensation,
}

/// Compensation record (design: saga.md §5.1).
#[derive(Debug)]
pub struct CompensationRecord {
    pub instance_id: String,
    pub node_id: String,
    pub order: u32,
    pub status: CompensationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationStatus {
    Pending,
    Completed,
    Failed,
}

/// CompensationRecordRepo trait (design: saga.md §14).
pub trait CompensationRecordRepo {
    fn list_completed(&self, instance_id: &str) -> Vec<CompensationRecord>;
    fn add(&self, record: &CompensationRecord);
}

/// SagaCoordinator: on TokenFailed, start compensation flow (design: saga.md §7, §8).
/// Stub: no-op until TokenFailed event and compensation execution are wired.
pub struct SagaCoordinator;
