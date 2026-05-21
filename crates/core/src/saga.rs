/// A record of a compensatable activity for saga-style rollback.
///
/// Compensation records are created as service tasks complete successfully.
/// When a failure triggers compensation, records are processed in reverse
/// `order` to undo completed work (newest first).
#[derive(Debug, Clone)]
pub struct CompensationRecord {
    pub instance_id: String,
    pub node_id: String,
    /// Execution order (higher = completed later = compensated first).
    pub order: u32,
    pub status: CompensationStatus,
}

/// Lifecycle state of a compensation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationStatus {
    /// Compensation handler has not been invoked yet.
    Pending,
    /// Compensation handler executed successfully.
    Completed,
    /// Compensation handler failed (requires manual intervention).
    Failed,
}
