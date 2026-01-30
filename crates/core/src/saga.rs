#[derive(Debug, Clone)]
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
