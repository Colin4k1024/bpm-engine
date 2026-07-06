//! Async persistence traits for the BPM engine.
//!
//! This crate defines the storage interface (trait objects) that the runtime depends on.
//! Concrete implementations live in adapter crates (`bpm-engine-adapter-postgres`,
//! `bpm-engine-adapter-memory`).

#![warn(missing_docs)]

pub mod compensation;
pub mod dead_letter_store;
pub mod event_store;
pub mod external_task_store;
pub mod history;
pub mod invariant;
pub mod invariant_checker;
pub mod parallel_join;
pub mod process_store;
pub mod timer_store;
pub mod token_store;

pub use compensation::*;
pub use dead_letter_store::*;
pub use event_store::*;
pub use external_task_store::*;
pub use history::*;
pub use invariant::*;
pub use invariant_checker::*;
pub use parallel_join::*;
pub use process_store::*;
pub use timer_store::*;
pub use token_store::*;
