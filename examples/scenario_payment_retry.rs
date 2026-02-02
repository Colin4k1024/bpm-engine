//! Accident-level scenario: payment + retry + completion, then verify with Trace API.
//!
//! Run the engine first: `cargo run -p bpm-engine-server-rest`
//! Then: `cargo run --example scenario_payment_retry`
//!
//! This example: starts a payment-flow instance, fetches and locks the payment task,
//! fails twice (retry), then fetches again and completes. Then it fetches the trace
//! and prints external_task_history to show "what happened?".

const BASE: &str = "http://127.0.0.1:3000/api/v1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();

    // 1. Start a process instance (payment-flow)
    let start = client
        .post(format!("{}/process-instances", BASE))
        .json(&serde_json::json!({
            "process_def_id": "payment-flow",
            "variables": { "amount": "100" }
        }))
        .send()
        .await?;
    if !start.status().is_success() {
        let body = start.text().await?;
        eprintln!("start failed: {}", body);
        std::process::exit(1);
    }
    let start_body: serde_json::Value = start.json().await?;
    let instance_id = start_body["instance_id"].as_str().unwrap();
    println!("Started instance: {}", instance_id);

    // 2. Fetch and lock (worker-1)
    let fetch = client
        .post(format!("{}/external-tasks/fetch-and-lock", BASE))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "task_types": ["payment"],
            "max_tasks": 5,
            "lock_duration_ms": 30_000
        }))
        .send()
        .await?;
    if !fetch.status().is_success() {
        eprintln!("fetch_and_lock failed: {}", fetch.text().await?);
        std::process::exit(1);
    }
    let tasks: Vec<serde_json::Value> = fetch.json().await?;
    let task_id = tasks.first().and_then(|t| t["task_id"].as_str()).unwrap();
    println!("Locked task: {}", task_id);

    // 3. Fail twice (retry); after each fail task goes back to READY, so we fetch again
    for i in 1..=2 {
        let fail = client
            .post(format!("{}/external-tasks/{}/fail", BASE, task_id))
            .json(&serde_json::json!({
                "worker_id": "worker-1",
                "error": format!("retry {}", i)
            }))
            .send()
            .await?;
        if !fail.status().is_success() {
            eprintln!("fail {} failed: {}", i, fail.text().await?);
            std::process::exit(1);
        }
        println!("Failed (retry {}); task goes back to READY", i);
        let fetch2 = client
            .post(format!("{}/external-tasks/fetch-and-lock", BASE))
            .json(&serde_json::json!({
                "worker_id": "worker-1",
                "task_types": ["payment"],
                "max_tasks": 5,
                "lock_duration_ms": 30_000
            }))
            .send()
            .await?;
        if !fetch2.status().is_success()
            || fetch2.json::<Vec<serde_json::Value>>().await?.is_empty()
        {
            eprintln!("fetch after fail {} failed or empty", i);
            std::process::exit(1);
        }
        println!("Locked again after retry {}", i);
    }

    // 4. Complete (task still locked from last fetch)
    let complete = client
        .post(format!("{}/external-tasks/{}/complete", BASE, task_id))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "variables": { "status": "PAID", "amount": "100" }
        }))
        .send()
        .await?;
    if !complete.status().is_success() {
        eprintln!("complete failed: {}", complete.text().await?);
        std::process::exit(1);
    }
    println!("Completed task.");

    // 5. Verify with Trace API
    let trace = client
        .get(format!("{}/process-instances/{}/trace", BASE, instance_id))
        .send()
        .await?;
    if !trace.status().is_success() {
        eprintln!("trace failed: {}", trace.text().await?);
        std::process::exit(1);
    }
    let trace_body: serde_json::Value = trace.json().await?;
    println!("\n--- Trace (external_task_history) ---");
    let ext = trace_body["external_task_history"].as_array().unwrap();
    for entry in ext {
        let task_id = entry["task_id"].as_str().unwrap_or("");
        let events = entry["events"].as_array().unwrap();
        println!("Task {}: {} events", task_id, events.len());
        for ev in events {
            println!("  - {} at {}", ev["event_type"], ev["occurred_at"]);
        }
    }
    println!("\nInstance status: {}", trace_body["instance"]["status"]);
    Ok(())
}
