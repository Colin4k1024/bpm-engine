//! Load DSL from JSON/YAML and register to ProcessDefStore (plan v2.0 A.4).

use super::{to_process_definition, DslProcessDefinition, ServiceTaskRegistry, ServiceTaskRegistryError};
use crate::persistence::ProcessDefStore;
use std::path::Path;

/// Load process definition from JSON string and register it into the store using the given registry.
pub fn load_and_register_json(
    json: &str,
    registry: &ServiceTaskRegistry,
    store: &ProcessDefStore,
) -> Result<(), LoadError> {
    let dsl: DslProcessDefinition = serde_json::from_str(json).map_err(LoadError::Json)?;
    let def = to_process_definition(&dsl, registry).map_err(LoadError::Registry)?;
    store.register(def);
    Ok(())
}

/// Load process definition from a JSON file path and register it.
pub fn load_and_register_json_file(
    path: impl AsRef<Path>,
    registry: &ServiceTaskRegistry,
    store: &ProcessDefStore,
) -> Result<(), LoadError> {
    let json = std::fs::read_to_string(path).map_err(LoadError::Io)?;
    load_and_register_json(&json, registry, store)
}

/// Parse JSON string into DSL only (no registry or store).
pub fn load_from_json(json: &str) -> Result<DslProcessDefinition, LoadError> {
    serde_json::from_str(json).map_err(LoadError::Json)
}

#[derive(Debug)]
pub enum LoadError {
    Json(serde_json::Error),
    Io(std::io::Error),
    Registry(ServiceTaskRegistryError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "JSON: {}", e),
            LoadError::Io(e) => write!(f, "IO: {}", e),
            LoadError::Registry(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Json(e) => Some(e),
            LoadError::Io(e) => Some(e),
            LoadError::Registry(e) => Some(e),
        }
    }
}
