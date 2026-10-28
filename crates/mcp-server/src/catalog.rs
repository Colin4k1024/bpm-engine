//! The agent tool catalog (E4-1) as parsed by the MCP server.
//!
//! Single source of truth: the COMMITTED artifact that lives in the server
//! crate (`crates/server/rest/capabilities/agent-tools.json`, served by
//! `GET /capabilities`) is embedded here via [`include_str!`] with a
//! workspace-relative path — this crate holds **no copy** of the catalog, so
//! the two can never drift at the file level. The lockstep test
//! (`tests/catalog_lockstep.rs`) additionally compares the embedded bytes
//! with the server crate's file (and its own embed) byte-for-byte, so a
//! redirected include path fails CI as loudly as an edited artifact.
//!
//! Only the fields the MCP server needs are parsed: the tool name, the REST
//! operation (`"<METHOD> <path>"`), the read-only/mutating classification,
//! the human-facing summary/description/side-effect notes, and the parameter
//! list (path/query/header/body) with their JSON-schema fragments. Response
//! shapes and error examples stay in the catalog/OpenAPI for agents that
//! fetch them via the `get_capabilities` tool.

use serde::Deserialize;
use serde_json::Value;

/// The committed catalog artifact (E4-1), embedded from the server crate's
/// file. Regenerate the artifact (from the normative `docs/openapi.yaml`)
/// with `cargo run -p bpm-engine-server-rest --example dump_capabilities`.
pub const AGENT_TOOLS_JSON: &str = include_str!("../../server/rest/capabilities/agent-tools.json");

/// How a catalog tool interacts with engine state (E4-1 classification).
///
/// `read_only` covers every GET plus the side-effect-free `validate` and
/// `dry-run` POSTs; everything else is `mutating`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// GETs and the side-effect-free validate/dry-run POSTs.
    ReadOnly,
    /// Every other POST/PUT/DELETE — consult the tool's side-effects note.
    Mutating,
}

impl Classification {
    /// Whether the tool never changes engine state.
    pub fn is_read_only(self) -> bool {
        matches!(self, Classification::ReadOnly)
    }
}

/// Where a catalog parameter lives on the REST call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamLocation {
    /// Path template segment, e.g. `{id}` in `/api/v1/tasks/{task_id}`.
    Path,
    /// Query-string parameter.
    Query,
    /// HTTP header (e.g. `X-Tenant-Id`, `Idempotency-Key`).
    Header,
    /// Request body.
    Body,
}

/// One catalog tool parameter (path/query/header/body) with its JSON-schema
/// fragment, description, and (for bodies) content type.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogParam {
    /// Parameter name — the path template name, query key, header name, or
    /// `body` for the request body.
    pub name: String,
    /// Where the parameter is placed on the REST call.
    #[serde(rename = "in")]
    pub location: ParamLocation,
    /// Whether the parameter must be provided.
    #[serde(default)]
    pub required: bool,
    /// JSON-schema fragment of the parameter (type/properties/required/enum).
    #[serde(default)]
    pub schema: Value,
    /// Human-facing parameter documentation.
    #[serde(default)]
    pub description: Option<String>,
    /// Content type of a body parameter (`application/json` or
    /// `application/xml`).
    #[serde(default)]
    pub content_type: Option<String>,
}

impl CatalogParam {
    /// The body parameter's content type, defaulting to JSON.
    pub fn body_content_type(&self) -> String {
        self.content_type
            .clone()
            .unwrap_or_else(|| "application/json".to_string())
    }
}

/// One catalog tool: a named REST operation with parameters and metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolSpec {
    /// Tool name (the MCP tool name, stable).
    pub name: String,
    /// REST operation in `"<METHOD> <path>"` form (e.g.
    /// `"POST /api/v1/process-definitions/validate"`).
    pub operation: String,
    /// One-line summary (the MCP tool title).
    pub summary: String,
    /// Longer description (optional).
    #[serde(default)]
    pub description: Option<String>,
    /// Read-only vs mutating classification.
    pub classification: Classification,
    /// Side-effect note (present on mutating tools).
    #[serde(default)]
    pub side_effects: Option<String>,
    /// Whether the operation sits behind the auth layer (health, readiness,
    /// and capabilities do not).
    #[serde(default)]
    pub auth_required: bool,
    /// Parameters (path/query/header/body).
    #[serde(default)]
    pub parameters: Vec<CatalogParam>,
}

impl ToolSpec {
    /// The HTTP method of the operation (`GET`, `POST`, `PUT`, `DELETE`).
    pub fn rest_method(&self) -> &str {
        self.operation.split(' ').next().unwrap_or_default()
    }

    /// The raw path template of the operation, without the method prefix
    /// (e.g. `/api/v1/external-tasks/{task_id}/complete`).
    pub fn rest_path(&self) -> &str {
        self.operation
            .split_once(' ')
            .map(|(_, path)| path.trim())
            .unwrap_or_default()
    }
}

/// The parsed agent tool catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    /// Catalog format version (bump on breaking catalog shape changes).
    #[serde(default)]
    pub catalog_version: String,
    /// Every catalog tool, in file order.
    pub tools: Vec<ToolSpec>,
}

impl Catalog {
    /// Parse the catalog artifact. Structural invariants are validated
    /// loudly (bad operations, multiple bodies) so catalog drift fails
    /// startup instead of misbehaving at dispatch time.
    pub fn parse(json: &str) -> Result<Catalog, String> {
        let catalog: Catalog = serde_json::from_str(json).map_err(|e| e.to_string())?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Parse the embedded catalog artifact (see [`AGENT_TOOLS_JSON`]).
    pub fn embedded() -> Result<Catalog, String> {
        Catalog::parse(AGENT_TOOLS_JSON)
    }

    /// Find a tool by name.
    pub fn find(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Validate the structural assumptions the MCP dispatch layer makes.
    fn validate(&self) -> Result<(), String> {
        for tool in &self.tools {
            let (method, path) = match tool.operation.split_once(' ') {
                Some((method, path)) => (method, path.trim()),
                None => {
                    return Err(format!(
                        "tool {:?}: operation {:?} is not \"<METHOD> <path>\"",
                        tool.name, tool.operation
                    ))
                }
            };
            if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") {
                return Err(format!(
                    "tool {:?}: unsupported REST method {method:?} in {:?}",
                    tool.name, tool.operation
                ));
            }
            if !path.starts_with('/') {
                return Err(format!(
                    "tool {:?}: REST path {path:?} is not root-relative",
                    tool.name
                ));
            }
            let bodies = tool
                .parameters
                .iter()
                .filter(|p| p.location == ParamLocation::Body)
                .count();
            if bodies > 1 {
                return Err(format!(
                    "tool {:?}: {} body parameters (the dispatch supports one)",
                    tool.name, bodies
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses() {
        let catalog = Catalog::embedded().expect("embedded catalog parses");
        assert_eq!(catalog.catalog_version, "1");
        // The catalog count is pinned so adding an engine operation without
        // regenerating the catalog fails loudly (E4-1 shipped 35; E5-2's
        // approval endpoints took it to 40).
        assert_eq!(catalog.tools.len(), 40);
    }

    #[test]
    fn operation_splitting() {
        let catalog = Catalog::embedded().expect("embedded catalog parses");
        let validate = catalog
            .find("validate_process_definition")
            .expect("present");
        assert_eq!(validate.rest_method(), "POST");
        assert_eq!(validate.rest_path(), "/api/v1/process-definitions/validate");
        let capabilities = catalog.find("get_capabilities").expect("present");
        assert_eq!(capabilities.rest_path(), "/capabilities");
        assert!(!capabilities.auth_required);
    }

    #[test]
    fn validate_rejects_broken_operations() {
        let err = Catalog::parse(r#"{"tools":[{"name":"t","operation":"GET","summary":"s","classification":"read_only","auth_required":true,"parameters":[]}]}"#)
            .expect_err("missing path must fail");
        assert!(err.contains("not \"<METHOD> <path>\""), "{err}");
    }
}
