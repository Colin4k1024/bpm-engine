//! Regenerate `crates/dsl/schema/bpm-dsl-v1.json` from the DSL types.
//!
//! Run from the repository root:
//!
//! ```sh
//! cargo run -p bpm-engine-dsl --example dump_schema
//! ```
//!
//! The `schema_lockstep` test fails if the committed file drifts from
//! `bpm_engine_dsl::schema_json()` output, so any type change must be
//! followed by a run of this example.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/bpm-dsl-v1.json");
    std::fs::write(&path, bpm_engine_dsl::schema_json())?;
    println!("wrote {}", path.display());
    Ok(())
}
