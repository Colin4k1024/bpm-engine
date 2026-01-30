//! v3: Cluster coordination — leader election, outbox publisher, timer poller.

pub mod leader;
pub mod outbox_publisher;
pub mod timer_poller;

pub use leader::{LeaderElection, ROLE_OUTBOX_PUBLISHER, ROLE_TIMER_POLLER};
pub use outbox_publisher::run_one_cycle as outbox_publisher_run_one_cycle;
pub use timer_poller::run_one_cycle as timer_poller_run_one_cycle;
