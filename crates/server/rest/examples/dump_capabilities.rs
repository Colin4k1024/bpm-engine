//! Regenerate `crates/server/rest/capabilities/agent-tools.json` from
//! `docs/openapi.yaml` (E4-1) — the DSL `dump_schema` lockstep pattern.
//!
//! Run from the repository root:
//!
//! ```sh
//! cargo run -p bpm-engine-server-rest --example dump_capabilities
//! ```
//!
//! The lockstep test of the consistency suite
//! (`crates/server/rest/tests/openapi_contract.rs`) fails when the
//! committed file drifts from what the spec generates, so any spec
//! change must be followed by a run of this example.
//!
//! `serde_yaml` is a dev-dependency of this crate (parsing happens here
//! and in the test only — the `/capabilities` endpoint serves the
//! committed artifact verbatim, with no runtime YAML parsing).

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    // <repo>/crates/server/rest → the repository root is three levels up
    // (built via ancestors, not ".." components, so the path stays real).
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest
        .ancestors()
        .nth(3)
        .expect("the manifest dir lives at <repo>/crates/server/rest");
    let spec_path = repo_root.join("docs/openapi.yaml");
    let catalog_path = manifest.join("capabilities/agent-tools.json");

    let yaml = std::fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", spec_path.display(), e));
    // YAML → JSON once, exactly like the consistency suite's `Spec::parse`:
    // the generator then works on plain JSON pointer navigation.
    let spec: serde_json::Value = {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&yaml).expect("docs/openapi.yaml must be valid YAML");
        serde_json::to_value(yaml).expect("yaml must convert to json")
    };

    let catalog = bpm_engine_server_rest::capabilities::generate_catalog(&spec);
    std::fs::write(&catalog_path, catalog)?;
    println!("wrote {}", catalog_path.display());
    Ok(())
}
