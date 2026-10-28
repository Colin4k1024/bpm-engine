//! Error contract tests: every [`DslErrorCode`] is exercised at least once
//! with an exact JSON-pointer path assertion, plus collect-all behavior.

use bpm_engine_dsl::{compile, DslCompileReport, DslErrorCode as Code};

/// Helper: build a minimal valid document and patch nodes/edges via JSON.
fn doc_with(nodes: &str, edges: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "process": {{ "id": "p" }},
  "nodes": {nodes},
  "edges": {edges}
}}"#
    )
}

fn first_error(report: &DslCompileReport) -> &bpm_engine_dsl::DslError {
    report
        .first()
        .unwrap_or_else(|| panic!("expected at least one error, got none"))
}

fn assert_code_path(json: &str, code: Code, path: &str) -> bpm_engine_dsl::DslError {
    let report = match compile(json) {
        Err(report) => report,
        Ok(def) => panic!("expected compilation to fail, got def {}", def.id),
    };
    let err = first_error(&report);
    assert_eq!(err.code, code, "unexpected code for report: {report}");
    assert_eq!(err.path, path, "unexpected path for report: {report}");
    err.clone()
}

// --------------------------------------------------------------------------
// JsonParse
// --------------------------------------------------------------------------

#[test]
fn error_json_parse_bad_syntax() {
    assert_code_path("{not json", Code::JsonParse, "/");
}

#[test]
fn error_json_parse_truncated() {
    assert_code_path(r#"{"schema_version": 1, "process""#, Code::JsonParse, "/");
}

// --------------------------------------------------------------------------
// SchemaVersionUnsupported
// --------------------------------------------------------------------------

#[test]
fn error_schema_version_unsupported() {
    let json = r#"{
  "schema_version": 2,
  "process": { "id": "p" },
  "nodes": [ { "id": "start", "type": "start" } ],
  "edges": []
}"#;
    let err = assert_code_path(json, Code::SchemaVersionUnsupported, "/schema_version");
    assert!(err.message.contains("2"));
}

// --------------------------------------------------------------------------
// UnknownField (deny_unknown_fields)
// --------------------------------------------------------------------------

#[test]
fn error_unknown_field_on_node() {
    // The issue's canonical example: typo'd field on /nodes/0.
    let json = r#"{
  "schema_version": 1,
  "process": { "id": "p" },
  "nodes": [
    { "id": "s", "type": "start", "taskTypo": "x" }
  ],
  "edges": []
}"#;
    assert_code_path(json, Code::UnknownField, "/nodes/0/taskTypo");
}

#[test]
fn error_unknown_field_nested_in_timer() {
    let json = r#"{
  "schema_version": 1,
  "process": { "id": "p" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "w", "type": "timerIntermediateCatch", "timer": { "duration": "PT1S", "duation": "x" } },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "w" },
    { "source": "w", "target": "end" }
  ]
}"#;
    assert_code_path(json, Code::UnknownField, "/nodes/1/timer/duation");
}

// --------------------------------------------------------------------------
// MissingField
// --------------------------------------------------------------------------

#[test]
fn error_missing_field_on_node() {
    let json = r#"{
  "schema_version": 1,
  "process": { "id": "p" },
  "nodes": [ { "type": "start" } ],
  "edges": []
}"#;
    assert_code_path(json, Code::MissingField, "/nodes/0/id");
}

#[test]
fn error_missing_field_on_document() {
    let json = r#"{
  "schema_version": 1,
  "process": { "id": "p" },
  "edges": []
}"#;
    assert_code_path(json, Code::MissingField, "/nodes");
}

#[test]
fn error_missing_field_required_by_node_type() {
    // serviceTask without task_type: structurally present in the type, but
    // required by the node type -> structural MissingField.
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "t", "type": "serviceTask" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "t" },
    { "source": "t", "target": "end" }
  ]"#,
    );
    assert_code_path(&json, Code::MissingField, "/nodes/1/task_type");
}

#[test]
fn error_missing_field_timer_required_by_node_type() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "w", "type": "timerIntermediateCatch" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "w" },
    { "source": "w", "target": "end" }
  ]"#,
    );
    assert_code_path(&json, Code::MissingField, "/nodes/1/timer");
}

// --------------------------------------------------------------------------
// InvalidType
// --------------------------------------------------------------------------

#[test]
fn error_invalid_type_wrong_field_type() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "t", "type": "serviceTask", "task_type": "work", "retries": "three" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "t" },
    { "source": "t", "target": "end" }
  ]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/nodes/1/retries");
}

#[test]
fn error_invalid_type_unknown_node_type() {
    let json = doc_with(r#"[ { "id": "s", "type": "weirdNode" } ]"#, r#"[]"#);
    assert_code_path(&json, Code::InvalidType, "/nodes/0/type");
}

#[test]
fn error_invalid_type_field_not_valid_for_node_type() {
    // form_key set on a start node.
    let json = doc_with(
        r#"[ { "id": "s", "type": "start", "form_key": "f" } ]"#,
        r#"[]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/nodes/0/form_key");
}

#[test]
fn error_invalid_type_empty_node_id() {
    let json = doc_with(r#"[ { "id": "", "type": "start" } ]"#, r#"[]"#);
    assert_code_path(&json, Code::InvalidType, "/nodes/0/id");
}

#[test]
fn error_invalid_type_condition_and_default_conflict() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "expression", "expression": "amount > 100" }, "default": true },
    { "source": "xor", "target": "end2" }
  ]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/edges/1/condition");
}

#[test]
fn error_invalid_type_variable_eq_unsafe_variable() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "variableEq", "variable": "a b", "value": "x" } },
    { "source": "xor", "target": "end2", "default": true }
  ]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/edges/1/condition/variable");
}

#[test]
fn error_invalid_type_variable_eq_value_with_quotes() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "variableEq", "variable": "ok", "value": "say \"hi\"" } },
    { "source": "xor", "target": "end2", "default": true }
  ]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/edges/1/condition/value");
}

#[test]
fn error_invalid_type_empty_expression() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "expression", "expression": "  " } },
    { "source": "xor", "target": "end2", "default": true }
  ]"#,
    );
    assert_code_path(&json, Code::InvalidType, "/edges/1/condition/expression");
}

// --------------------------------------------------------------------------
// DuplicateNodeId / DuplicateEdgeId
// --------------------------------------------------------------------------

#[test]
fn error_duplicate_node_id() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "a", "type": "end" },
    { "id": "a", "type": "end" }
  ]"#,
        r#"[]"#,
    );
    let err = assert_code_path(&json, Code::DuplicateNodeId, "/nodes/2/id");
    assert!(err.message.contains("first defined at /nodes/1"));
}

#[test]
fn error_duplicate_edge_id() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "id": "f", "source": "start", "target": "end" },
    { "id": "f", "source": "start", "target": "end" }
  ]"#,
    );
    assert_code_path(&json, Code::DuplicateEdgeId, "/edges/1/id");
}

// --------------------------------------------------------------------------
// UnknownNodeRef
// --------------------------------------------------------------------------

#[test]
fn error_unknown_node_ref_source() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "end" },
    { "source": "ghost", "target": "end" }
  ]"#,
    );
    assert_code_path(&json, Code::UnknownNodeRef, "/edges/1/source");
}

#[test]
fn error_unknown_node_ref_target() {
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[ { "source": "start", "target": "ghost" } ]"#,
    );
    assert_code_path(&json, Code::UnknownNodeRef, "/edges/0/target");
}

// --------------------------------------------------------------------------
// EmptyNodeList
// --------------------------------------------------------------------------

#[test]
fn error_empty_node_list() {
    let json = doc_with(r#"[]"#, r#"[]"#);
    assert_code_path(&json, Code::EmptyNodeList, "/nodes");
}

// --------------------------------------------------------------------------
// Compile (pass-through of shared BPMN compiler semantics)
// --------------------------------------------------------------------------

#[test]
fn error_compile_no_end_event_carries_source_code() {
    // start -> orphan task, no end event at all: NoEndEvent (plus orphan and
    // dead-end errors) from the shared compiler, passed through with the
    // upstream code.
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "t", "type": "serviceTask", "task_type": "work" }
  ]"#,
        r#"[
    { "source": "start", "target": "t" }
  ]"#,
    );
    let report = compile(&json).unwrap_err();
    assert!(
        report.len() >= 2,
        "expected multiple collected errors: {report}"
    );
    let no_end = report
        .errors
        .iter()
        .find(|e| e.source_code.as_deref() == Some("NoEndEvent"))
        .expect("expected a NoEndEvent pass-through error");
    assert_eq!(no_end.code, Code::Compile);
    assert_eq!(no_end.path, "/");
    let orphan = report
        .errors
        .iter()
        .find(|e| e.source_code.as_deref() == Some("OrphanNode"))
        .expect("expected an OrphanNode pass-through error");
    assert_eq!(
        orphan.path, "/nodes/1",
        "semantic errors map back to node index"
    );
}

#[test]
fn error_compile_multiple_start_events() {
    let json = doc_with(
        r#"[
    { "id": "s1", "type": "start" },
    { "id": "s2", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "s1", "target": "end" }
  ]"#,
    );
    let report = compile(&json).unwrap_err();
    assert!(report.errors.iter().any(
        |e| e.code == Code::Compile && e.source_code.as_deref() == Some("MultipleStartEvents")
    ));
}

// --------------------------------------------------------------------------
// Collect-all behavior
// --------------------------------------------------------------------------

#[test]
fn structural_errors_are_all_collected() {
    // One document with a duplicate node id, a dangling edge source, and a
    // field invalid for its node type: three errors, one report.
    let json = doc_with(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "a", "type": "end" },
    { "id": "a", "type": "end", "form_key": "x" }
  ]"#,
        r#"[
    { "source": "start", "target": "a" },
    { "source": "ghost", "target": "a" }
  ]"#,
    );
    let report = compile(&json).unwrap_err();
    assert_eq!(report.len(), 3, "report: {report}");
    let codes: Vec<_> = report.errors.iter().map(|e| e.code).collect();
    assert!(codes.contains(&Code::DuplicateNodeId));
    assert!(codes.contains(&Code::UnknownNodeRef));
    assert!(codes.contains(&Code::InvalidType));
    // Document order.
    assert_eq!(report.errors[0].code, Code::DuplicateNodeId);
    assert_eq!(report.errors[1].code, Code::InvalidType);
    assert_eq!(report.errors[2].code, Code::UnknownNodeRef);
}

#[test]
fn report_display_and_is_empty() {
    let empty = DslCompileReport::default();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);

    let report = compile("{bad").err().unwrap();
    assert!(!report.is_empty());
    let text = report.to_string();
    assert!(text.contains("json_parse"));
}

// --------------------------------------------------------------------------
// Serialized error shape (public JSON contract)
// --------------------------------------------------------------------------

#[test]
fn serialized_report_shape_is_stable() {
    // An unknown-field error; assert the serialized JSON keys and the
    // snake_case code string stay stable.
    let json = doc_with(
        r#"[{ "id": "a", "type": "start" }, { "id": "b", "type": "end", "taskTypo": 1 }]"#,
        r#"[{ "source": "a", "target": "b" }]"#,
    );
    let report = compile(&json).expect_err("must fail");
    let value = serde_json::to_value(&report).expect("serialize report");
    let arr = value["errors"]
        .as_array()
        .expect("report serializes as {errors: [...]}");
    assert!(!arr.is_empty());
    let first = &arr[0];
    for key in ["code", "path", "message"] {
        assert!(
            first.get(key).is_some(),
            "serialized DslError must carry `{key}`"
        );
    }
    assert_eq!(first["code"].as_str().unwrap(), "unknown_field");

    // A single error serialized as an object has the same key set.
    let one = report.first().unwrap();
    let v = serde_json::to_value(one).expect("serialize error");
    assert!(v.get("code").is_some());
    assert!(v.get("path").is_some());
    assert!(v.get("message").is_some());
}

#[test]
fn serialized_compile_error_carries_source_code() {
    // No edges from start -> shared compiler error; source_code must serialize.
    let json = doc_with(
        r#"[{ "id": "a", "type": "start" }, { "id": "b", "type": "end" }]"#,
        r#"[]"#,
    );
    let report = compile(&json).expect_err("must fail");
    let v = serde_json::to_value(report.first().unwrap()).unwrap();
    assert_eq!(v["code"].as_str().unwrap(), "compile");
    assert!(
        v.get("sourceCode").is_some(),
        "compile errors must carry the compiler's source code"
    );
}
