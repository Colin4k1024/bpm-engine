//! Tool surface mechanics: flattening a catalog tool into an MCP
//! `inputSchema`, validating tool arguments, and building the REST request
//! a tool call dispatches to.
//!
//! Flattening rules (pure functions of the catalog entry — no hand-written
//! per-tool tables, so a regenerated catalog needs no code change):
//!
//! - **path/query parameters** become top-level input properties under
//!   their own names (with the catalog descriptions attached);
//! - **body parameters** with an `object` schema are MERGED: each body
//!   property becomes a top-level input property and the body's `required`
//!   list feeds the top-level `required`;
//! - **body parameters** with any other schema (the raw BPMN XML string of
//!   `deploy`) become a single property named `body`;
//! - **header parameters** are deliberately NOT caller-settable, with one
//!   exception: `Idempotency-Key` is exposed as the optional
//!   `idempotency_key` argument (start-instance idempotency, E2-4).
//!   `X-Tenant-Id` and every other header can never be set through this
//!   server — unknown arguments are rejected, so an MCP client cannot
//!   smuggle a tenant (or any other header) past the boundary. Tenancy is
//!   enforced ENGINE-side from the verified JWT claim (E5-1); this layer
//!   forwards credentials only and adds no trust.
//!
//! Argument validation checks the top-level properties only (presence of
//! required keys, unknown-key rejection, primitive type and enum checks).
//! Nested structures are passed through verbatim — the engine's REST layer
//! is the authority for full request validation and answers with its
//! stable error codes.

use crate::catalog::{ParamLocation, ToolSpec};
use serde_json::{Map, Value};

/// The single header argument the MCP surface exposes (start-instance
/// idempotency, E2-4). Every other header — notably `X-Tenant-Id` — is
/// never settable through MCP.
const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";
/// The input-property name [`IDEMPOTENCY_HEADER`] is exposed as.
const IDEMPOTENCY_PROPERTY: &str = "idempotency_key";

/// A flattened tool schema: the MCP input properties, the required
/// argument names, and which properties form the JSON body.
#[derive(Debug, Clone)]
pub struct FlatSchema {
    /// Property name → JSON schema (descriptions merged in).
    pub properties: Map<String, Value>,
    /// Required property names, in flattening order.
    pub required: Vec<String>,
    /// Property names that belong to a JSON-object request body.
    pub body_keys: Vec<String>,
    /// The single raw (non-object) body parameter, when present.
    pub raw_body: Option<RawBodySpec>,
}

/// A non-object body parameter (`deploy_process_definition`'s BPMN XML).
#[derive(Debug, Clone)]
pub struct RawBodySpec {
    /// Content type to send the raw body with.
    pub content_type: String,
}

impl FlatSchema {
    /// Whether `name` is a known input property.
    pub fn has_property(&self, name: &str) -> bool {
        self.properties.contains_key(name)
    }
}

/// Flatten a catalog tool into its MCP input schema shape.
///
/// Errors on name collisions between path/query parameters and body
/// properties (the E4-1 catalog has none; a collision would make the
/// flattening ambiguous and must fail loudly).
pub fn flatten(tool: &ToolSpec) -> Result<FlatSchema, String> {
    let mut flat = FlatSchema {
        properties: Map::new(),
        required: Vec::new(),
        body_keys: Vec::new(),
        raw_body: None,
    };

    let add_property = |flat: &mut FlatSchema,
                        name: &str,
                        schema: &Value,
                        description: Option<&str>,
                        required: bool|
     -> Result<(), String> {
        if flat.has_property(name) {
            return Err(format!(
                "tool {:?}: input property {name:?} collides with a parameter",
                tool.name
            ));
        }
        // A missing/odd schema fragment becomes the empty schema (accept
        // anything) — `null` is not a legal JSON-Schema value.
        let mut prop = if schema.is_object() {
            schema.clone()
        } else {
            Value::Object(Map::new())
        };
        if let Some(desc) = description {
            if !desc.is_empty() {
                prop.as_object_mut()
                    .map(|obj| obj.insert("description".to_string(), Value::String(desc.into())));
            }
        }
        if required && !flat.required.iter().any(|r| r == name) {
            flat.required.push(name.to_string());
        }
        flat.properties.insert(name.to_string(), prop);
        Ok(())
    };

    for param in &tool.parameters {
        match param.location {
            ParamLocation::Path | ParamLocation::Query => {
                add_property(
                    &mut flat,
                    &param.name,
                    &param.schema,
                    param.description.as_deref(),
                    param.required,
                )?;
            }
            ParamLocation::Header => {
                if param.name == IDEMPOTENCY_HEADER {
                    add_property(
                        &mut flat,
                        IDEMPOTENCY_PROPERTY,
                        &param.schema,
                        param.description.as_deref(),
                        param.required,
                    )?;
                }
                // Every other header (X-Tenant-Id, ...) is intentionally
                // dropped: never caller-settable through MCP.
            }
            ParamLocation::Body => {
                let is_object = param.schema.get("type").and_then(Value::as_str) == Some("object");
                if is_object {
                    if let Some(props) = param.schema.get("properties").and_then(Value::as_object) {
                        let required_names: Vec<&str> = param
                            .schema
                            .get("required")
                            .and_then(Value::as_array)
                            .map(|arr| arr.iter().filter_map(Value::as_str).collect::<Vec<&str>>())
                            .unwrap_or_default();
                        for (name, schema) in props {
                            add_property(
                                &mut flat,
                                name,
                                schema,
                                None,
                                required_names.contains(&name.as_str()),
                            )?;
                            flat.body_keys.push(name.clone());
                        }
                    }
                } else {
                    add_property(
                        &mut flat,
                        &param.name,
                        &param.schema,
                        param.description.as_deref(),
                        param.required,
                    )?;
                    flat.raw_body = Some(RawBodySpec {
                        content_type: param.body_content_type(),
                    });
                }
            }
        }
    }
    Ok(flat)
}

/// The MCP `inputSchema` of a catalog tool (a JSON-Schema object with
/// `additionalProperties: false` so clients reject smuggled keys).
pub fn input_schema(tool: &ToolSpec) -> Result<Value, String> {
    let flat = flatten(tool)?;
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".into()));
    schema.insert("properties".to_string(), Value::Object(flat.properties));
    if !flat.required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(
                flat.required
                    .iter()
                    .map(|r| Value::String(r.clone()))
                    .collect(),
            ),
        );
    }
    schema.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(Value::Object(schema))
}

/// Validate tool `arguments` against the flattened schema.
///
/// Checks (top level only): unknown keys are rejected (this is the tenant /
/// header smuggling guard), required keys must be present, and primitive
/// types / enums must match. Nested values are passed through for the
/// engine to validate.
pub fn validate_arguments(tool: &ToolSpec, arguments: &Value) -> Result<(), String> {
    let Some(args) = arguments.as_object() else {
        return Err("arguments must be a JSON object".to_string());
    };
    let flat = flatten(tool)?;
    for key in args.keys() {
        if !flat.has_property(key) {
            return Err(format!(
                "unknown parameter {key:?}: not part of this tool's input schema \
                 (headers — including X-Tenant-Id — are never settable through this server)"
            ));
        }
    }
    for required in &flat.required {
        if !args.contains_key(required) {
            return Err(format!("missing required parameter {required:?}"));
        }
    }
    for (key, value) in args {
        if let Some(schema) = flat.properties.get(key) {
            check_type(tool, key, value, schema)?;
        }
    }
    Ok(())
}

/// Check one argument against its schema's `type` and `enum` keywords.
fn check_type(tool: &ToolSpec, key: &str, value: &Value, schema: &Value) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let ok = match expected {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            "null" => value.is_null(),
            _ => true, // Unknown type keyword: pass through, engine validates.
        };
        if !ok {
            return Err(format!(
                "tool {:?}: parameter {key:?} must be {expected}, got {}",
                tool.name,
                type_name(value)
            ));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(format!(
                "tool {:?}: parameter {key:?} must be one of {allowed:?}, got {value}",
                tool.name
            ));
        }
    }
    Ok(())
}

/// The JSON type name of a value (for error messages).
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// One dispatchable REST request, built from a tool + validated arguments.
#[derive(Debug, Clone)]
pub struct RestRequest {
    /// HTTP method (`GET`, `POST`, `PUT`, `DELETE`).
    pub method: String,
    /// Substituted path (root-relative, no query string).
    pub path: String,
    /// Query parameters (order preserved; encoded by the HTTP client).
    pub query: Vec<(String, String)>,
    /// Extra headers the MCP surface exposes (only `Idempotency-Key`).
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: Option<RestBody>,
}

/// The body of a REST request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestBody {
    /// A JSON document (the common case).
    Json(Value),
    /// A raw payload with an explicit content type (deploy's BPMN XML).
    Raw {
        /// Content type of the raw payload.
        content_type: String,
        /// The raw payload bytes (UTF-8).
        data: String,
    },
}

/// Build the REST request for `tool` with already-validated `arguments`.
///
/// Pure: no I/O, fully deterministic. Errors signal internal invariant
/// breakage (unsubstituted path templates) that earlier validation should
/// have made impossible.
pub fn build_request(tool: &ToolSpec, arguments: &Value) -> Result<RestRequest, String> {
    let flat = flatten(tool)?;
    let args = arguments
        .as_object()
        .ok_or_else(|| "arguments must be a JSON object".to_string())?;

    let mut path = tool.rest_path().to_string();
    for param in &tool.parameters {
        if param.location != ParamLocation::Path {
            continue;
        }
        let value = args
            .get(&param.name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("path parameter {:?} missing or not a string", param.name))?;
        path = path.replace(&format!("{{{}}}", param.name), &encode_path_segment(value));
    }
    if path.contains('{') || path.contains('}') {
        return Err(format!("tool {:?}: unsubstituted path {path:?}", tool.name));
    }

    let mut query = Vec::new();
    for param in &tool.parameters {
        if param.location != ParamLocation::Query {
            continue;
        }
        if let Some(value) = args.get(&param.name) {
            let rendered = query_value(value)
                .ok_or_else(|| format!("query parameter {:?} must be a scalar", param.name))?;
            query.push((param.name.clone(), rendered));
        }
    }

    let mut headers = Vec::new();
    if let Some(value) = args.get(IDEMPOTENCY_PROPERTY) {
        let key = value
            .as_str()
            .ok_or_else(|| format!("{IDEMPOTENCY_PROPERTY} must be a string"))?;
        headers.push((IDEMPOTENCY_HEADER.to_string(), key.to_string()));
    }

    let body = if let Some(raw) = &flat.raw_body {
        let data = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| "raw body parameter \"body\" missing or not a string".to_string())?;
        Some(RestBody::Raw {
            content_type: raw.content_type.clone(),
            data: data.to_string(),
        })
    } else if !flat.body_keys.is_empty() {
        let mut body_obj = Map::new();
        for key in &flat.body_keys {
            if let Some(value) = args.get(key) {
                body_obj.insert(key.clone(), value.clone());
            }
        }
        Some(RestBody::Json(Value::Object(body_obj)))
    } else {
        None
    };

    Ok(RestRequest {
        method: tool.rest_method().to_string(),
        path,
        query,
        headers,
        body,
    })
}

/// Render a scalar argument as a query-string value.
fn query_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Percent-encode one path segment.
///
/// Unreserved characters (`A-Z a-z 0-9 - _ . ~`) and `:` (user-task ids are
/// `<instance_id>:<node_id>`) pass through; everything else — including
/// `/` (path traversal) — is percent-encoded so an argument can never
/// escape its segment or inject extra segments.
fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use serde_json::json;

    fn tool(name: &str) -> ToolSpec {
        Catalog::embedded()
            .expect("embedded catalog")
            .find(name)
            .unwrap_or_else(|| panic!("tool {name} in catalog"))
            .clone()
    }

    #[test]
    fn input_schema_merges_body_and_drops_headers() {
        let schema = input_schema(&tool("list_process_instances")).expect("flattens");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("limit"));
        assert!(props.contains_key("offset"));
        // The deprecated tenant header must NOT be caller-settable.
        assert!(!props.contains_key("X-Tenant-Id"));
        assert!(!props.contains_key("x-tenant-id"));
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("required").is_none()); // both optional
    }

    #[test]
    fn input_schema_flattens_body_properties() {
        let schema = input_schema(&tool("validate_process_definition")).expect("flattens");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("format"));
        assert!(props.contains_key("source"));
        assert!(props.contains_key("variables"));
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("format")));
        assert!(required.contains(&json!("source")));
        // Enum survives the flattening.
        assert_eq!(props["format"]["enum"], json!(["bpmn", "dsl"]));
    }

    #[test]
    fn deploy_uses_a_raw_body_property() {
        let schema = input_schema(&tool("deploy_process_definition")).expect("flattens");
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert_eq!(props["body"]["type"], "string");
        assert_eq!(schema["required"], json!(["body"]));
    }

    #[test]
    fn idempotency_key_is_exposed_as_a_property() {
        let schema = input_schema(&tool("start_process_instance")).expect("flattens");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("idempotency_key"));
        assert!(props.contains_key("process_def_id"));
        assert!(props.contains_key("variables"));
        // Required: only process_def_id (body) — idempotency and tenant
        // headers are optional.
        assert_eq!(schema["required"], json!(["process_def_id"]));
        // The tenant header is not exposed.
        assert!(!props.contains_key("X-Tenant-Id"));
    }

    #[test]
    fn validate_arguments_rejects_tenant_header_smuggling() {
        let tool = tool("list_process_instances");
        let err = validate_arguments(&tool, &json!({"X-Tenant-Id": "other-tenant"}))
            .expect_err("smuggled header must be rejected");
        assert!(err.contains("unknown parameter"), "{err}");
    }

    #[test]
    fn validate_arguments_checks_required_types_and_enums() {
        let tool = tool("validate_process_definition");
        assert!(validate_arguments(&tool, &json!({})).is_err());
        assert!(validate_arguments(&tool, &json!({"format": "bpmn"})).is_err());
        let err = validate_arguments(&tool, &json!({"format": "yaml", "source": "<xml/>"}))
            .expect_err("bad enum rejected");
        assert!(err.contains("yaml"), "{err}");
        let err = validate_arguments(&tool, &json!({"format": 5, "source": "<xml/>"}))
            .expect_err("bad type rejected");
        assert!(err.contains("string"), "{err}");
        assert!(validate_arguments(&tool, &json!({"format": "dsl", "source": "{}"})).is_ok());
        // Nested structures pass through untouched.
        assert!(validate_arguments(
            &tool,
            &json!({"format": "dsl", "source": "{}", "variables": {"amount": "5"}})
        )
        .is_ok());
    }

    #[test]
    fn validate_arguments_accepts_integral_numbers_only() {
        let tool = tool("get_instance_history");
        assert!(validate_arguments(&tool, &json!({"id": "i", "limit": 10})).is_ok());
        let err = validate_arguments(&tool, &json!({"id": "i", "limit": 10.5}))
            .expect_err("fractional limit rejected");
        assert!(err.contains("integer"), "{err}");
        let err = validate_arguments(&tool, &json!({"id": "i", "limit": "10"}))
            .expect_err("string limit rejected");
        assert!(err.contains("integer"), "{err}");
    }

    #[test]
    fn build_request_substitutes_path_and_keeps_colons() {
        let tool = tool("complete_user_task");
        let request = build_request(
            &tool,
            &json!({"task_id": "inst-1:review", "variables": {"ok": "yes"}}),
        )
        .expect("builds");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/tasks/inst-1:review/complete");
        match request.body {
            Some(RestBody::Json(v)) => assert_eq!(v, json!({"variables": {"ok": "yes"}})),
            other => panic!("expected JSON body, got {other:?}"),
        }
    }

    #[test]
    fn build_request_encodes_path_traversal() {
        let tool = tool("get_process_instance");
        let request = build_request(&tool, &json!({"id": "../api/v1/admin"})).expect("builds");
        assert_eq!(
            request.path,
            "/api/v1/process-instances/..%2Fapi%2Fv1%2Fadmin"
        );
    }

    #[test]
    fn build_request_collects_query_and_idempotency_header() {
        let tool = tool("start_process_instance");
        let request = build_request(
            &tool,
            &json!({"process_def_id": "minimal", "idempotency_key": "abc-123"}),
        )
        .expect("builds");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/process-instances");
        assert_eq!(
            request.headers,
            vec![("Idempotency-Key".to_string(), "abc-123".to_string())]
        );
        assert_eq!(
            request.body,
            Some(RestBody::Json(json!({"process_def_id": "minimal"})))
        );
    }

    #[test]
    fn build_request_deploy_is_raw_xml() {
        let tool = tool("deploy_process_definition");
        let request = build_request(&tool, &json!({"body": "<definitions/>"})).expect("builds");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/process-definitions/deploy");
        assert_eq!(
            request.body,
            Some(RestBody::Raw {
                content_type: "application/xml".to_string(),
                data: "<definitions/>".to_string()
            })
        );
    }

    #[test]
    fn build_request_fetch_and_lock_body() {
        let tool = tool("fetch_and_lock_external_tasks");
        let request = build_request(
            &tool,
            &json!({"worker_id": "w-1", "task_types": ["payment"], "lock_duration_ms": 60000}),
        )
        .expect("builds");
        assert_eq!(request.path, "/api/v1/external-tasks/fetch-and-lock");
        assert_eq!(
            request.body,
            Some(RestBody::Json(
                json!({"worker_id": "w-1", "task_types": ["payment"], "lock_duration_ms": 60000})
            ))
        );
        assert!(request.headers.is_empty());
    }

    #[test]
    fn build_request_query_params_render_scalars() {
        let tool = tool("get_instance_history");
        let request = build_request(
            &tool,
            &json!({"id": "inst-9", "limit": 25, "event_type": "TokenArrived"}),
        )
        .expect("builds");
        // Catalog parameter order (event_type before limit) is preserved.
        assert_eq!(
            request.query,
            vec![
                ("event_type".to_string(), "TokenArrived".to_string()),
                ("limit".to_string(), "25".to_string()),
            ]
        );
    }
}
