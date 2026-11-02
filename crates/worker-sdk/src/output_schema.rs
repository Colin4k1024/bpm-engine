//! Structured output validation (N3-2): a minimal JSON Schema subset validator
//! and an output contract that validates model output and maps only declared
//! fields to process variables.
//!
//! The validator supports the JSON Schema keywords used in practice for AI
//! output contracts: `type`, `properties`, `required`, `additionalProperties`,
//! and `enum`. It is intentionally not a full [Draft 2020-12] implementation —
//! it covers the subset that gates model output before it reaches process
//! variables and gateways.
//!
//! [Draft 2020-12]: https://json-schema.org/

use serde_json::Value;
use std::collections::HashMap;

/// A validation error with a stable machine-readable code and a human-readable
/// detail. The code prefix maps to `output_validation_failed` (ADR-011 §5:
/// Permanent retry category).
#[derive(Debug, Clone)]
pub struct SchemaValidationError {
    /// Stable code: `type_mismatch`, `missing_required`, `unknown_property`,
    /// `enum_mismatch`.
    pub code: &'static str,
    /// JSON-pointer-style path to the offending element (e.g. `/verdict`).
    pub path: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Validate a JSON value against a minimal JSON Schema subset.
///
/// Returns a list of errors (empty = valid). The schema must be a JSON object;
/// a non-object schema is treated as "accept anything" (no constraints).
pub fn validate_json_schema(value: &Value, schema: &Value) -> Vec<SchemaValidationError> {
    let mut errors = Vec::new();
    if !schema.is_object() {
        return errors; // no constraints
    }
    validate_node(value, schema, "", &mut errors);
    errors
}

fn validate_node(
    value: &Value,
    schema: &Value,
    path: &str,
    errors: &mut Vec<SchemaValidationError>,
) {
    let Some(schema_obj) = schema.as_object() else {
        return;
    };

    // type
    if let Some(type_val) = schema_obj.get("type") {
        if let Some(expected) = type_val.as_str() {
            if !type_matches(value, expected) {
                errors.push(SchemaValidationError {
                    code: "type_mismatch",
                    path: path.to_string(),
                    detail: format!(
                        "expected type '{}', got '{}'",
                        expected,
                        json_type_name(value)
                    ),
                });
                return; // further checks are meaningless on a type mismatch
            }
        }
    }

    // enum
    if let Some(enum_arr) = schema_obj.get("enum").and_then(|v| v.as_array()) {
        if !enum_arr.iter().any(|e| e == value) {
            errors.push(SchemaValidationError {
                code: "enum_mismatch",
                path: path.to_string(),
                detail: format!("value {} is not one of the allowed values", value),
            });
        }
    }

    // properties + required + additionalProperties (only for objects)
    if value.is_object() {
        let value_obj = value.as_object().unwrap();
        let properties = schema_obj.get("properties").and_then(|v| v.as_object());

        // required
        if let Some(required) = schema_obj.get("required").and_then(|v| v.as_array()) {
            for req in required {
                if let Some(key) = req.as_str() {
                    if !value_obj.contains_key(key) {
                        errors.push(SchemaValidationError {
                            code: "missing_required",
                            path: format!("{path}/{key}"),
                            detail: format!("required property '{key}' is missing"),
                        });
                    }
                }
            }
        }

        // additionalProperties: false → reject unknown keys
        let additional_false = schema_obj
            .get("additionalProperties")
            .map(|v| v.as_bool() == Some(false))
            .unwrap_or(false);

        for (key, child) in value_obj {
            let child_path = format!("{path}/{key}");
            if let Some(props) = properties {
                if let Some(child_schema) = props.get(key) {
                    validate_node(child, child_schema, &child_path, errors);
                } else if additional_false {
                    errors.push(SchemaValidationError {
                        code: "unknown_property",
                        path: child_path,
                        detail: format!(
                            "property '{key}' is not declared in the schema (additionalProperties: false)"
                        ),
                    });
                }
            } else if additional_false {
                errors.push(SchemaValidationError {
                    code: "unknown_property",
                    path: child_path,
                    detail: format!(
                        "property '{key}' is not allowed (additionalProperties: false)"
                    ),
                });
            }
        }
    }
}

fn type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" | "integer" => {
            value.is_number()
                && (expected != "integer"
                    || value
                        .as_i64()
                        .map(|i| i as f64 == value.as_f64().unwrap())
                        .unwrap_or(false))
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true, // unknown type → accept
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// An output contract: a JSON Schema for the model output and a field mapping
/// that declares which output fields become process variables (N3-2).
///
/// Only fields listed in `field_mapping` are extracted into variables —
/// undeclared fields are silently dropped (default-deny). The schema validates
/// the model output before any field is mapped.
#[derive(Debug, Clone, Default)]
pub struct OutputContract {
    /// JSON Schema describing the expected model output shape.
    pub schema: Option<Value>,
    /// `(json_field, variable_name)` pairs. Only these fields are mapped to
    /// process variables. An empty list means "no field mapping" (the worker
    /// sets variables manually).
    pub field_mapping: Vec<(String, String)>,
}

/// Outcome of validating and mapping model output.
#[derive(Debug)]
pub enum OutputMapping {
    /// The model output was valid and declared fields were mapped to variables.
    Mapped(HashMap<String, String>),
    /// The model output was valid but no field mapping was configured — the
    /// caller should handle the raw text itself.
    NoMapping { raw_text: String },
    /// The model output failed schema validation.
    Invalid(Vec<SchemaValidationError>),
}

impl OutputContract {
    /// Validate the model response text against the schema and map declared
    /// fields to process variables.
    ///
    /// - If `schema` is `None`, no validation is performed; returns `NoMapping`.
    /// - If the response is not valid JSON, returns `Invalid` with a
    ///   `type_mismatch` error.
    /// - If the response fails schema validation, returns `Invalid` with the
    ///   error list.
    /// - If valid, extracts only the declared fields (as plain strings) into
    ///   a variable map.
    pub fn validate_and_map(&self, response_text: &str) -> OutputMapping {
        let Some(schema) = &self.schema else {
            return OutputMapping::NoMapping {
                raw_text: response_text.to_string(),
            };
        };

        let parsed: Value = match serde_json::from_str(response_text) {
            Ok(v) => v,
            Err(e) => {
                return OutputMapping::Invalid(vec![SchemaValidationError {
                    code: "type_mismatch",
                    path: String::new(),
                    detail: format!("model output is not valid JSON: {e}"),
                }]);
            }
        };

        let errors = validate_json_schema(&parsed, schema);
        if !errors.is_empty() {
            return OutputMapping::Invalid(errors);
        }

        let mut variables = HashMap::new();
        for (field, var_name) in &self.field_mapping {
            if let Some(val) = parsed.get(field) {
                let string_val = match val {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                variables.insert(var_name.clone(), string_val);
            }
        }
        OutputMapping::Mapped(variables)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_output_passes() {
        let schema = json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string", "enum": ["approved", "rejected"] },
                "reason": { "type": "string" }
            },
            "required": ["verdict"],
            "additionalProperties": false
        });
        let value = json!({ "verdict": "approved", "reason": "low risk" });
        let errors = validate_json_schema(&value, &schema);
        assert!(errors.is_empty(), "{:?}", errors);
    }

    #[test]
    fn missing_required_fails() {
        let schema = json!({
            "type": "object",
            "required": ["verdict"],
            "properties": { "verdict": { "type": "string" } }
        });
        let value = json!({ "reason": "no verdict" });
        let errors = validate_json_schema(&value, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "missing_required");
        assert!(errors[0].path.contains("verdict"));
    }

    #[test]
    fn unknown_property_rejected() {
        let schema = json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" } },
            "additionalProperties": false
        });
        let value = json!({ "verdict": "approved", "extra": "sneaky" });
        let errors = validate_json_schema(&value, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "unknown_property");
        assert!(errors[0].path.contains("extra"));
    }

    #[test]
    fn enum_mismatch_fails() {
        let schema = json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string", "enum": ["approved", "rejected"] }
            },
            "required": ["verdict"]
        });
        let value = json!({ "verdict": "maybe" });
        let errors = validate_json_schema(&value, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "enum_mismatch");
    }

    #[test]
    fn type_mismatch_fails() {
        let schema = json!({ "type": "string" });
        let value = json!(42);
        let errors = validate_json_schema(&value, &schema);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "type_mismatch");
    }

    #[test]
    fn output_contract_maps_declared_fields_only() {
        let contract = OutputContract {
            schema: Some(json!({
                "type": "object",
                "properties": {
                    "verdict": { "type": "string", "enum": ["approved", "rejected"] },
                    "score": { "type": "number" },
                    "internal": { "type": "string" }
                },
                "required": ["verdict"],
                "additionalProperties": false
            })),
            field_mapping: vec![
                ("verdict".to_string(), "decision".to_string()),
                ("score".to_string(), "risk_score".to_string()),
            ],
        };
        let response = r#"{"verdict":"approved","score":85,"internal":"hidden"}"#;
        match contract.validate_and_map(response) {
            OutputMapping::Mapped(vars) => {
                assert_eq!(vars.get("decision"), Some(&"approved".to_string()));
                assert_eq!(vars.get("risk_score"), Some(&"85".to_string()));
                // "internal" is NOT mapped — default deny.
                assert!(!vars.contains_key("internal"));
                assert!(!vars.contains_key("verdict"));
            }
            other => panic!("expected Mapped, got {:?}", other),
        }
    }

    #[test]
    fn output_contract_rejects_invalid_json() {
        let contract = OutputContract {
            schema: Some(json!({ "type": "object" })),
            field_mapping: vec![],
        };
        match contract.validate_and_map("not json at all") {
            OutputMapping::Invalid(errors) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, "type_mismatch");
            }
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn output_contract_no_schema_returns_no_mapping() {
        let contract = OutputContract::default();
        match contract.validate_and_map("anything") {
            OutputMapping::NoMapping { raw_text } => {
                assert_eq!(raw_text, "anything");
            }
            other => panic!("expected NoMapping, got {:?}", other),
        }
    }
}
