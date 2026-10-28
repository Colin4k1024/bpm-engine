//! Offline regression-eval harness (E6-3, issue #28) — the RUNNABLE half
//! of the statistical/comparative output.
//!
//! Spawns the engine REST app (memory storage) on an ephemeral loopback
//! port, deploys BOTH definition fixture versions of the five reference
//! flows, runs the FULL fixed case set (13 cases, failure injections
//! included) against v1 and v2 with the deterministic offline mock
//! provider, then:
//!
//! 1. writes the serde-serializable report to `regression-report.json`
//!    (or the first CLI argument's path),
//! 2. prints the markdown summary.
//!
//! Run:
//!
//! ```text
//! cargo run -p bpm-engine-ai-reference --example regression_eval
//! ```
//!
//! **The separation principle (统计指标与确定性内核测试分开):** this
//! harness REPORTS — success-rate deltas, latency distributions, cost
//! totals per version — and asserts NOTHING. Wall-clock latency and the
//! aggregate rates are never gates; the deterministic invariants (exact
//! completion states, attempt counts, costs per the mock price table,
//! recovery verdicts) live as ordinary tests in
//! `crates/ai-reference/tests/regression_eval.rs`. See
//! `docs/regression-eval.md`.
//!
//! Offline always: the mock provider is pure, HTTP goes to `127.0.0.1`
//! only, no API keys (any ledger credential value works — outputs are
//! masked).

use std::sync::Arc;

use bpm_engine_ai_reference::eval::{
    eval_flow_options, fixed_case_set, fixture_version_selector, run_regression_with,
};
use bpm_engine_ai_reference::{MockModelProvider, ModelProvider};
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Optional CLI: the report path (default regression-report.json in the
    // current directory).
    let output_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "regression-report.json".to_string());

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

    // 2. The reference tool credential (any value; outputs are masked).
    std::env::set_var("BPM_REFERENCE_LEDGER_KEY", "sk-eval-demo-not-a-real-secret");

    // 3. Run the FULL fixed case set against both definition versions with
    //    the short eval timings (2 s lease, 200 ms worker poll). Both
    //    versions are deployed by the harness itself (deduplicated).
    let cases = fixed_case_set();
    println!(
        "E6-3 regression eval: {} cases × 2 definition versions (v1 as-is, \
         v2 fixtures) against {} — offline, mock provider.",
        cases.len(),
        base_url
    );
    let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::new());
    let selector = fixture_version_selector();
    let report = run_regression_with(
        &base_url,
        app.state.storage.deployer.as_ref(),
        provider,
        &cases,
        &selector,
        eval_flow_options(),
    )
    .await?;

    // 4. Write the JSON report and print the markdown summary.
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&output_path, json)?;
    println!();
    println!(
        "report written: {}",
        std::fs::canonicalize(&output_path)
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| output_path.clone())
    );
    println!();
    println!("{}", report.markdown_summary());

    // 5. Tear down.
    app.timer_cancel.cancel();
    serve.abort();
    Ok(())
}
