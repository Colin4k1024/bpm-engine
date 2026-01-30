use std::collections::HashMap;

use super::token::Token;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum InstanceState {
    Running,
    Completed,
    Terminated,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessInstance {
    pub id: String,
    pub process_def_id: String,
    pub tenant_id: Option<String>,
    pub tokens: Vec<Token>,
    pub variables: HashMap<String, String>,
    pub state: InstanceState,
    pub version: u32,
}

impl ProcessInstance {
    pub fn completed(&self) -> bool {
        self.state == InstanceState::Completed
    }
}
