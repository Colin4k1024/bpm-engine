// Placeholder; content migrated in step 2.
pub type TokenId = String;
pub type ParallelGroupId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TokenStatus {
    Created,
    Ready,
    Executing,
    Waiting,
    Suspended,
    Completed,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TokenMode {
    Forward,
    Compensation,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Token {
    pub id: TokenId,
    pub node_id: String,
    pub status: TokenStatus,
    pub mode: TokenMode,
    pub version: u32,
    pub attempt: u32,
    pub parallel_group_id: Option<ParallelGroupId>,
    pub updated_at: Option<String>,
}

impl Token {
    pub fn waiting(&self) -> bool {
        self.status == TokenStatus::Waiting
    }
}
