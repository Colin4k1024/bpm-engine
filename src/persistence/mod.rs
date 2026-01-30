//! Persistence layer (design: overview §5).
//! Repo traits and SQLite / memory implementation.

pub mod memory;
pub mod memory_repo;
pub mod repo;
pub mod sqlite;

pub use memory::ProcessDefStore;
pub use memory_repo::MemoryRepo;
pub use repo::{
    CompensationRecordRepo, CompensationRecordRow, LeaderLeaseRepo, OutboxEvent, OutboxRepo,
    ParallelJoinRepo, ProcessDefinitionRepo, ProcessInstanceRepo, TimerRecord, TimerRepo, TokenRepo,
    TransactionScope, UserTaskRepo,
};
pub use sqlite::InstanceRepo;