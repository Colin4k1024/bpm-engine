//! Minimal runnable example: start a process instance and poll until completed.
//!
//! Prerequisite: start the engine first:
//!   cargo run -p bpm-server-rest
//!
//! Then run:
//!   cargo run --example simple_process
//!
//! This uses the built-in "minimal" process (Start -> End) so no worker is needed.

use std::time::Duration;

const BASE: &str = "http://127.0.0.1:3000/api/v1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();

    // Start a process instance (minimal: Start -> End)
    let start_resp = client
        .post(format!("{}/process-instances", BASE))
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await?;

    if !start_resp.status().is_success() {
        let status = start_resp.status();
        let body = start_resp.text().await?;
        eprintln!("Engine returned {}: {}", status, body);
        eprintln!("Make sure the engine is running: cargo run -p bpm-server-rest");
        std::process::exit(1);
    }

    let start_body: serde_json::Value = start_resp.json().await?;
    let instance_id = start_body["instance_id"]
        .as_str()
        .ok_or("missing instance_id")?;

    println!("Process started: instance_id = {}", instance_id);

    // Poll until COMPLETED (or timeout)
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;

        let get_resp = client
            .get(format!("{}/process-instances/{}", BASE, instance_id))
            .send()
            .await?;

        if !get_resp.status().is_success() {
            continue;
        }

        let inst: serde_json::Value = get_resp.json().await?;
        let status = inst["status"].as_str().unwrap_or("");

        if status == "COMPLETED" {
            println!("Process completed.");
            return Ok(());
        }
    }

    eprintln!("Timeout waiting for process to complete.");
    std::process::exit(1);
}
