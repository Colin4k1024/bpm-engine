//! Self-contained AI reference flow demo (E3-4, issue #18).
//!
//! Spawns the engine REST app (memory storage) on an ephemeral port,
//! deploys the APPROVAL (审批) reference flow, runs it end to end with the
//! offline mock model provider, and prints the outcome: final state,
//! variables, and the step timeline with each step's AI envelope summary
//! (model + cost / tools).
//!
//! Run with:
//!
//! ```text
//! cargo run --example ai_flow
//! ```
//!
//! No external server, no network beyond `127.0.0.1`, no API keys — the
//! mock provider is deterministic. Swap in a real provider by enabling the
//! `http` feature and passing an `OpenAiCompatProvider` to `run_flow`
//! instead (see `docs/ai-reference-flows.md`).

use std::sync::Arc;

use bpm_engine_ai_reference::{
    approval_flow, deploy_flow, run_flow, MockModelProvider, ModelProvider,
};
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Build and serve the engine REST app (memory storage) locally.
    let config = BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    };
    let app = build_app(&config).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app.router).await;
    });
    let base_url = format!("http://{addr}");

    // 2. Deploy the approval flow: its DSL JSON compiles to a definition
    //    and is registered with the engine's definition store.
    //    (A ledger credential from the environment exercises the E3-3
    //    credential path; any value works — outputs are masked.)
    std::env::set_var("BPM_REFERENCE_LEDGER_KEY", "sk-demo-not-a-real-secret");
    let flow = approval_flow(true);
    deploy_flow(app.state.storage.deployer.as_ref(), &flow).await?;
    println!("Deployed flow: {} ({})", flow.name, flow.process_def_id);

    // 3. Run it end to end with the offline mock provider.
    let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::new());
    let outcome = run_flow(&base_url, flow, provider).await?;

    // 4. Print the outcome.
    println!();
    println!("instance : {}", outcome.instance_id);
    println!("state    : {}", outcome.state);
    println!("variables:");
    for (name, value) in &outcome.variables {
        let shown = if name == "__task_envelope" {
            "(E3-1 AI task envelope — see below)"
        } else {
            value.as_str()
        };
        println!("  {name} = {shown}");
    }
    println!("steps:");
    for step in &outcome.steps {
        let envelope = match &step.envelope {
            Some(summary) => {
                let model = summary
                    .get("model")
                    .map(|m| format!("{}/{}", m["provider"], m["name"]))
                    .unwrap_or_default();
                let cost = summary
                    .get("cost")
                    .map(|c| {
                        format!(
                            "{} {} ({}->{} tokens)",
                            c["amount_micros"], c["currency"], c["tokens_in"], c["tokens_out"]
                        )
                    })
                    .unwrap_or_default();
                let tools = summary
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .map(|tools| {
                        tools
                            .iter()
                            .map(|t| t["name"].as_str().unwrap_or("?").to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                format!(
                    "  envelope: {model} {cost}{}",
                    if tools.is_empty() {
                        String::new()
                    } else {
                        format!(" tools=[{tools}]")
                    }
                )
            }
            None => String::new(),
        };
        let errors = if step.attempt_errors.is_empty() {
            String::new()
        } else {
            format!(" errors={:?}", step.attempt_errors)
        };
        println!(
            "  #{:<2} {:<10} {:<14} {:?}{}{}",
            step.step_no, step.node_id, step.kind, step.event_types, envelope, errors
        );
    }

    // 5. Tear down.
    app.timer_cancel.cancel();
    serve.abort();
    Ok(())
}
