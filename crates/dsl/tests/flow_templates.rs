//! Flow templates (E4-4, issue #22): every JSON document under
//! `templates/flows/` must compile through the public DSL entry.
//!
//! The templates are the machine-readable examples shipped to users and
//! agents; this test is the drift guard — a template that stops compiling
//! (schema typo, dangling edge, invalid timer) fails CI instead of
//! shipping broken. The documents live at the repo root, reached from this
//! crate's manifest directory (`CARGO_MANIFEST_DIR` =
//! `<repo>/crates/dsl`).

use std::fs;
use std::path::Path;

use bpm_engine_dsl::compile;

/// The committed flow-template directory, relative to this crate.
const TEMPLATES_DIR: &str = "../../templates/flows";

/// Templates that must exist (one entry per shipped file, so a rename or
/// deletion is also caught).
const EXPECTED: &[(&str, &str)] = &[
    ("basic-gateway.json", "basic-gateway"),
    ("ai-approval-flow.json", "ai-approval-flow"),
    ("timer-wait.json", "timer-wait"),
];

#[test]
fn flow_templates_compile() {
    let dir = dir();
    for (file, process_id) in EXPECTED {
        let path = dir.join(file);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing flow template {}: {e}", path.display()));
        match compile(&source) {
            Ok(def) => {
                assert_eq!(
                    def.id, *process_id,
                    "{file}: process.id changed — update the template or EXPECTED"
                );
                assert!(
                    !def.nodes.is_empty(),
                    "{file}: compiled to an empty definition"
                );
            }
            Err(report) => {
                let diagnostics = report
                    .errors
                    .iter()
                    .map(|e| format!("[{}] {}: {}", e.code, e.path, e.message))
                    .collect::<Vec<_>>()
                    .join("\n  ");
                panic!("{file} does not compile:\n  {diagnostics}");
            }
        }
    }
}

#[test]
fn flow_templates_dir_has_no_stray_documents() {
    let dir = dir();
    let mut seen: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()?.to_str()? == "json" {
                Some(path.file_name()?.to_str()?.to_string())
            } else {
                None
            }
        })
        .collect();
    seen.sort();
    let mut expected: Vec<String> = EXPECTED.iter().map(|(f, _)| f.to_string()).collect();
    expected.sort();
    assert_eq!(
        seen, expected,
        "templates/flows changed — add the file to EXPECTED in this test"
    );
}

fn dir() -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(TEMPLATES_DIR);
    assert!(
        dir.is_dir(),
        "flow template directory not found at {} (is the crate checkout complete?)",
        dir.display()
    );
    dir
}
