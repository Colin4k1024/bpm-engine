//! Catalog lockstep (E4-2): the catalog THIS crate embeds must equal the
//! server crate's committed artifact byte-for-byte, and both must parse.
//!
//! The MCP server embeds the server crate's file directly via a
//! workspace-relative `include_str!` (single source — no copy exists in
//! this crate). These tests make that guarantee CHECKED rather than
//! incidental: a redirected include path (someone "fixing" the path to a
//! copied file), a server-side move of the artifact, or a hand-edit that
//! breaks parsing all fail here with instructions.

use bpm_engine_mcp_server::catalog::Catalog;
use bpm_engine_mcp_server::AGENT_TOOLS_JSON;

/// The committed artifact inside the SERVER crate, read from disk.
const SERVER_CATALOG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../server/rest/capabilities/agent-tools.json"
);

#[test]
fn embedded_catalog_equals_the_server_crates_file_on_disk() {
    let server_file = std::fs::read_to_string(SERVER_CATALOG_PATH)
        .expect("the server crate's agent-tools.json must be readable");
    assert_eq!(
        AGENT_TOOLS_JSON, server_file,
        "the MCP server's embedded catalog drifted from \
         {SERVER_CATALOG_PATH}: keep the include_str! path pointed at the \
         server crate's file (never copy it); regenerate the artifact with \
         `cargo run -p bpm-engine-server-rest --example dump_capabilities`"
    );
}

#[test]
fn embedded_catalog_equals_the_server_crates_embed() {
    // The server crate embeds the same file through its own include path;
    // comparing the two EMBEDS catches an include-path drift on either
    // side (a copy of the JSON anywhere would fail one of the two checks).
    let server_embed = bpm_engine_server_rest::capabilities::AGENT_TOOLS_JSON;
    assert_eq!(
        AGENT_TOOLS_JSON, server_embed,
        "the MCP server's embedded catalog differs from the server crate's \
         embedded AGENT_TOOLS_JSON: both must embed \
         crates/server/rest/capabilities/agent-tools.json"
    );
}

#[test]
fn embedded_catalog_parses_and_covers_the_v1_surface() {
    let catalog = Catalog::embedded().expect("embedded catalog parses");
    assert_eq!(catalog.tools.len(), 40);
    let names: Vec<&str> = catalog.tools.iter().map(|t| t.name.as_str()).collect();
    for name in bpm_engine_mcp_server::allowlist::V1_SURFACE {
        assert!(names.contains(name), "v1 surface tool {name} missing");
    }
}

#[test]
fn lockstep_helper_exposes_the_same_bytes_through_the_served_endpoint_shape() {
    // The artifact served by GET /capabilities is this exact document;
    // the end-to-end suite checks that tool call too — this unit-level
    // check pins the parseability of every tool's parameter schemas.
    let catalog = Catalog::embedded().expect("embedded catalog parses");
    for tool in &catalog.tools {
        bpm_engine_mcp_server::input_schema(tool)
            .unwrap_or_else(|e| panic!("tool {} does not flatten: {e}", tool.name));
    }
}
