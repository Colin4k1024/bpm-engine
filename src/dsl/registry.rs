//! ServiceTask handler registry (plan v2.0 A.2).
//! Maps handler name (e.g. "validate") to fn(&mut ProcessInstance) for DSL → ProcessDefinition.

use crate::model::ProcessInstance;
use std::collections::HashMap;

/// Registry of named ServiceTask handlers. Used when converting DSL to ProcessDefinition.
#[derive(Default)]
pub struct ServiceTaskRegistry {
    handlers: HashMap<String, fn(&mut ProcessInstance)>,
}

impl ServiceTaskRegistry {
    pub fn new() -> Self {
        ServiceTaskRegistry {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler under the given name.
    pub fn register(&mut self, name: impl Into<String>, handler: fn(&mut ProcessInstance)) {
        self.handlers.insert(name.into(), handler);
    }

    /// Look up a handler by name. Returns None if not registered.
    pub fn get(&self, name: &str) -> Option<fn(&mut ProcessInstance)> {
        self.handlers.get(name).copied()
    }

    /// Resolve handler by name; returns error if not found (for conversion).
    pub fn resolve(
        &self,
        name: &str,
    ) -> Result<fn(&mut ProcessInstance), ServiceTaskRegistryError> {
        self.get(name).ok_or_else(|| ServiceTaskRegistryError {
            handler_ref: name.to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ServiceTaskRegistryError {
    pub handler_ref: String,
}

impl std::fmt::Display for ServiceTaskRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServiceTask handler not registered: {}", self.handler_ref)
    }
}

impl std::error::Error for ServiceTaskRegistryError {}
