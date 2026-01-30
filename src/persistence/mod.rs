//! Persistence layer (design: overview §5).
//! Repo traits and SQLite / memory implementation.

pub mod memory;
pub mod repo;
pub mod sqlite;

pub use memory::ProcessDefStore;
pub use repo::{OutboxEvent, OutboxRepo, ParallelJoinRepo, ProcessDefinitionRepo, ProcessInstanceRepo, TokenRepo, TransactionScope, UserTaskRepo};
pub use sqlite::InstanceRepo;