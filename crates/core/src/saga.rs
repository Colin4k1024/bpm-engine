/// A record of a compensatable activity for saga-style rollback.
///
/// Compensation records are created as service tasks complete successfully.
/// When a failure triggers compensation, records are processed in reverse
/// `order` to undo completed work (newest first).
///
/// # Example
///
/// ```
/// use bpm_engine_core::{CompensationRecord, CompensationStatus};
///
/// let records = vec![
///     CompensationRecord {
///         instance_id: "inst-1".into(),
///         node_id: "task-a".into(),
///         order: 1,
///         status: CompensationStatus::Pending,
///     },
///     CompensationRecord {
///         instance_id: "inst-1".into(),
///         node_id: "task-b".into(),
///         order: 2,
///         status: CompensationStatus::Pending,
///     },
/// ];
///
/// // Sort by order descending — newest first for compensation
/// let mut sorted: Vec<_> = records.iter()
///     .filter(|r| r.status == CompensationStatus::Pending)
///     .collect();
/// sorted.sort_by(|a, b| b.order.cmp(&a.order));
///
/// assert_eq!(sorted[0].node_id, "task-b"); // compensated first
/// assert_eq!(sorted[1].node_id, "task-a"); // compensated second
/// ```
#[derive(Debug, Clone)]
pub struct CompensationRecord {
    /// The process instance this record belongs to.
    pub instance_id: String,
    /// The node that performed the compensatable action.
    pub node_id: String,
    /// Execution order (higher = completed later = compensated first).
    pub order: u32,
    /// Current compensation lifecycle state.
    pub status: CompensationStatus,
}

/// Lifecycle state of a compensation record.
///
/// Compensation records start as `Pending` when the activity completes.
/// During saga rollback, handlers are invoked in reverse order:
/// - `Pending` -> `Completed` on successful compensation
/// - `Pending` -> `Failed` if the compensation handler fails
///
/// # Example
///
/// ```
/// use bpm_engine_core::CompensationStatus;
///
/// let status = CompensationStatus::Pending;
/// assert_eq!(status, CompensationStatus::Pending);
/// assert_ne!(status, CompensationStatus::Completed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationStatus {
    /// Compensation handler has not been invoked yet.
    Pending,
    /// Compensation handler executed successfully.
    Completed,
    /// Compensation handler failed (requires manual intervention).
    Failed,
}
