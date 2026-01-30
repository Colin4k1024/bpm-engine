//! Domain layer: process definition, instance, token (design: overview §8).
//! Re-exports legacy model and adds design types (InstanceState, TokenStatus, Token with id/version).

pub mod instance;
pub mod process;
pub mod token;

pub use instance::{InstanceState, ProcessInstance};
pub use process::{EdgeCondition, Node, NodeType, OutgoingEdge, ProcessDefinition};
pub use token::{ParallelGroupId, Token, TokenId, TokenMode, TokenStatus};
