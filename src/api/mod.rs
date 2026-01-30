//! Application layer (design: overview §6).
//! ProcessService / TaskService; optional REST API when feature "api" is enabled.

pub mod service;

#[cfg(feature = "api")]
pub mod http;
