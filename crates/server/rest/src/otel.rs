//! Tracing initialization with optional OpenTelemetry integration.
//!
//! When the `otel` feature is enabled:
//! - Uses OTLP exporter if `OTEL_EXPORTER_OTLP_ENDPOINT` is set
//! - Falls back to stdout exporter for local development
//! - Service name: `bpm-engine`
//!
//! When the `otel` feature is disabled:
//! - Uses plain tracing-subscriber

/// Global tracer provider for graceful shutdown (OTel feature only).
#[cfg(feature = "otel")]
static TRACER_PROVIDER: std::sync::OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> =
    std::sync::OnceLock::new();

/// Initialize the tracing subscriber.
///
/// - Format: controlled by `log_format` (default `json`, set `pretty` for human-readable)
/// - Level: controlled by `log_level` (default `info`)
pub fn init_tracing(log_level: &str, log_format: &str) {
    #[cfg(feature = "otel")]
    {
        init_tracing_otel(log_level, log_format);
    }
    #[cfg(not(feature = "otel"))]
    {
        init_tracing_plain(log_level, log_format);
    }
}

/// Initialize plain tracing (no OTel).
fn init_tracing_plain(log_level: &str, log_format: &str) {
    use tracing_subscriber::{fmt, EnvFilter};

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    match log_format {
        "pretty" => {
            fmt()
                .with_env_filter(env_filter)
                .with_target(true)
                .with_ansi(true)
                .init();
        }
        _ => {
            fmt()
                .json()
                .with_env_filter(env_filter)
                .with_target(true)
                .with_current_span(true)
                .init();
        }
    }
}

/// Initialize tracing with OTel integration.
#[cfg(feature = "otel")]
fn init_tracing_otel(log_level: &str, log_format: &str) {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};

    // Build OTel exporter - requires OTEL_EXPORTER_OTLP_ENDPOINT
    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err() {
        tracing::warn!("OTEL_EXPORTER_OTLP_ENDPOINT not set, falling back to plain tracing");
        init_tracing_plain(log_level, log_format);
        return;
    }
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .expect("failed to build OTLP exporter");

    let provider = SdkTracerProvider::builder()
        .with_resource(otel_resource())
        .with_simple_exporter(exporter)
        .build();

    let tracer = provider.tracer("bpm-engine");
    let _ = TRACER_PROVIDER.set(provider);

    let level_filter = match log_level {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "warn" => LevelFilter::WARN,
        "error" => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    };

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Use plain fmt layer (json formatting handled by subscriber filter)
    tracing_subscriber::registry()
        .with(level_filter)
        .with(fmt::layer())
        .with(otel_layer)
        .init();
}

/// Build OTel resource with service name.
#[cfg(feature = "otel")]
fn otel_resource() -> opentelemetry_sdk::Resource {
    use opentelemetry::KeyValue;
    opentelemetry_sdk::Resource::builder()
        .with_attribute(KeyValue::new("service.name", "bpm-engine"))
        .build()
}

/// Create an HTTP trace layer for axum (OTel feature only).
#[cfg(feature = "otel")]
pub fn http_trace_layer() -> tower_http::trace::TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
> {
    tower_http::trace::TraceLayer::new_for_http()
}

/// Flush OTel traces and shut down the provider.
pub fn shutdown_tracing() {
    #[cfg(feature = "otel")]
    {
        if let Some(provider) = TRACER_PROVIDER.get() {
            if let Err(e) = provider.shutdown() {
                tracing::error!(error = %e, "failed to flush OTel traces");
            }
        }
    }
}
