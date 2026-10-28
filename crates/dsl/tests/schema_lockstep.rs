//! Lockstep test: the committed schema file must equal fresh
//! `schema_json()` output, so the types and the published schema can never
//! drift. Regenerate with:
//!
//! ```sh
//! cargo run -p bpm-engine-dsl --example dump_schema
//! ```

use std::path::PathBuf;

const TITLE: &str = "BPM DSL v1";

#[test]
fn committed_schema_file_matches_fresh_output() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/bpm-dsl-v1.json");
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
    let fresh = bpm_engine_dsl::schema_json();
    assert!(
        committed == fresh,
        "committed schema drifted from the DSL types; regenerate with \
         `cargo run -p bpm-engine-dsl --example dump_schema`"
    );
}

#[test]
fn schema_has_expected_shape() {
    let schema: serde_json::Value = serde_json::from_str(&bpm_engine_dsl::schema_json()).unwrap();
    assert_eq!(schema["title"], TITLE);
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);

    // All root fields required.
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for field in ["schema_version", "process", "nodes", "edges"] {
        assert!(
            required.contains(&field),
            "root field `{}` must be required",
            field
        );
    }

    // schema_version pinned to the constant 1.
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);

    // Restricted node-type set (schemars renders unit enums as oneOf consts).
    let node_type_variants: Vec<&str> = schema["$defs"]["DslNodeType"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["const"].as_str().unwrap())
        .collect();
    assert_eq!(
        node_type_variants,
        vec![
            "start",
            "end",
            "serviceTask",
            "userTask",
            "exclusiveGateway",
            "parallelGateway",
            "timerIntermediateCatch",
        ]
    );

    // Object types deny unknown fields, mirroring deny_unknown_fields.
    for def in [
        "DslNode",
        "DslEdge",
        "DslProcess",
        "DslTimer",
        "DslFormField",
    ] {
        assert_eq!(
            schema["$defs"][def]["additionalProperties"], false,
            "{} must deny additional properties",
            def
        );
    }
}

#[test]
fn schema_generation_is_deterministic() {
    assert_eq!(bpm_engine_dsl::schema_json(), bpm_engine_dsl::schema_json());
}
