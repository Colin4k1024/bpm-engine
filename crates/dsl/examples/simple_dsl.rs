//! Compile an embedded DSL document and print the compiled node list.
//!
//! Run:
//!
//! ```sh
//! cargo run -p bpm-engine-dsl --example simple_dsl
//! ```

const ORDER_FLOW: &str = r#"{
  "schema_version": 1,
  "process": { "id": "order-flow", "name": "Order Flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    {
      "id": "charge",
      "name": "Charge Payment",
      "type": "serviceTask",
      "task_type": "payment",
      "retries": 5,
      "timeout_secs": 120
    },
    { "id": "decide", "type": "exclusiveGateway" },
    { "id": "end-approved", "type": "end" },
    { "id": "end-review", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "charge" },
    { "source": "charge", "target": "decide" },
    {
      "source": "decide",
      "target": "end-approved",
      "condition": { "kind": "variableEq", "variable": "approved", "value": "true" }
    },
    { "source": "decide", "target": "end-review", "default": true }
  ]
}"#;

fn main() {
    match bpm_engine_dsl::compile(ORDER_FLOW) {
        Ok(def) => {
            println!("compiled process: {} (start: {})", def.id, def.start);
            let mut ids: Vec<&str> = def.nodes.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let node = &def.nodes[id];
                println!("  {:<14} {:?} -> {:?}", id, node.node_type, {
                    node.outgoing_edges
                        .iter()
                        .map(|e| (e.target, &e.condition))
                        .collect::<Vec<_>>()
                });
            }
        }
        Err(report) => {
            eprintln!("failed to compile the DSL document:");
            eprintln!("{}", report);
            std::process::exit(1);
        }
    }
}
