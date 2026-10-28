//! BPM Engine REST server binary. Run: `cargo run -p bpm-engine-server-rest`
//!
//! All application assembly lives in the `bpm_engine_server_rest` library
//! ([`build_app`] / [`serve`]); this binary only loads configuration and
//! initializes tracing.

use bpm_engine_server_rest::{bpm_config::BpmConfig, otel, serve};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Load configuration (file + env + defaults)
    let config = BpmConfig::load();

    // Initialize structured tracing (with optional OTel integration)
    otel::init_tracing(&config.log.level, &config.log.format);

    // Print effective config (secrets masked)
    config.log_effective();

    // Build, serve, and shut down gracefully (cancels the timer scheduler)
    serve(config).await?;
    Ok(())
}
