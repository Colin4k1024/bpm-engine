//! JSON Schema generation for the BPM DSL v1 types.
//!
//! The schema is derived from the same types the compiler deserializes
//! (`schemars`), so it stays in lockstep with validation. A committed copy
//! lives at `crates/dsl/schema/bpm-dsl-v1.json`; a unit test asserts the
//! committed file matches [`schema_json`] output byte for byte, and
//! `examples/dump_schema.rs` regenerates it.

use schemars::schema_for;
use serde_json::Value;

use crate::types::DslDocument;

/// Generate the JSON Schema (draft 2020-12) for `BPM DSL v1` documents.
///
/// The output is deterministic (object keys are emitted in sorted order) and:
///
/// - carries the title `BPM DSL v1` and a description of the restricted node
///   set (from the [`DslDocument`] doc comment);
/// - requires `schema_version` and constrains it to the constant `1`
///   (matching the runtime check in [`crate::compile`]);
/// - sets `additionalProperties: false` on every object type (mirroring the
///   `deny_unknown_fields` serde attribute).
pub fn schema_json() -> String {
    let schema = schema_for!(DslDocument);
    let mut value = serde_json::to_value(&schema).expect("schema must serialize");

    // Pin schema_version to the constant 1 in addition to the runtime check.
    if let Some(properties) = value.get_mut("properties").and_then(Value::as_object_mut) {
        if let Some(schema_version) = properties
            .get_mut("schema_version")
            .and_then(Value::as_object_mut)
        {
            schema_version.insert("const".to_string(), serde_json::json!(1));
        }
    }

    serde_json::to_string_pretty(&value).expect("schema must serialize")
}
