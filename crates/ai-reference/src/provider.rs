//! Swappable model-provider adapter layer (E3-4, issue #18).
//!
//! [`ModelProvider`] is the whole contract: one `generate` call from a typed
//! request to a typed response. The reference suite always runs against
//! [`MockModelProvider`] — deterministic, offline, no clock, no randomness —
//! so CI needs no keys and produces byte-stable envelopes. Applications swap
//! the provider (that is the point of the layer):
//!
//! - [`UnconfiguredProvider`] — the explicit "no provider configured" stub
//!   (always compiled in; returns a clear error naming your options).
//! - `OpenAiCompatProvider` (crate feature `http`, **off by default**) — an
//!   OpenAI-compatible chat-completions adapter configured from the
//!   environment (`OPENAI_BASE_URL` / `OPENAI_API_KEY` / `OPENAI_MODEL`,
//!   read only at construction, never during `generate`).
//!
//! Token accounting and cost: every [`ModelResponse`] carries token counts,
//! and [`ProviderConfig`] holds the per-model price table used by
//! [`ProviderConfig::cost_for`] to map a response onto the E3-1
//! [`TaskCost`] of the task envelope (micro-unit amounts, integer math).

use std::collections::HashMap;

use anyhow::anyhow;
use async_trait::async_trait;
use bpm_engine_core::{ModelRef, TaskCost};

/// Canned summary returned by the mock provider's `summar*` route.
pub const MOCK_SUMMARY_TEXT: &str =
    "Summary: 2 documents reviewed; key figures extracted; no anomalies found.";

/// Verdict returned by the mock provider's `classify` route when the deciding
/// variable does not indicate high risk.
pub const MOCK_APPROVED: &str = "approved";

/// Verdict returned by the mock provider's `classify` route when the deciding
/// variable (`risk`) is exactly `"high"`.
pub const MOCK_REJECTED: &str = "rejected";

/// Token counts of the mock provider's routes — fixed constants so the cost
/// of every reference-model task is byte-stable (assertable in CI).
mod mock_tokens {
    /// `summar*` route: input tokens.
    pub const SUMMARY_IN: u64 = 48;
    /// `summar*` route: output tokens.
    pub const SUMMARY_OUT: u64 = 12;
    /// `classify` route: input tokens.
    pub const CLASSIFY_IN: u64 = 24;
    /// `classify` route: output tokens.
    pub const CLASSIFY_OUT: u64 = 2;
    /// Echo route: input tokens.
    pub const ECHO_IN: u64 = 16;
    /// Echo route: output tokens.
    pub const ECHO_OUT: u64 = 8;
}

/// One generation request to a model provider.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// The prompt for this generation.
    pub prompt: String,
    /// Plain (legacy string) task/instance variables the provider may route
    /// on — e.g. the mock's classify route reads `risk`.
    pub variables: HashMap<String, String>,
    /// Optional cap on output tokens; providers that honor it clamp the
    /// reported output-token count.
    pub max_tokens: Option<u32>,
}

impl ModelRequest {
    /// A request with just a prompt (no variables, no cap).
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            variables: HashMap::new(),
            max_tokens: None,
        }
    }

    /// Builder: attach one plain variable.
    pub fn variable(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.variables.insert(name.into(), value.into());
        self
    }

    /// Builder: cap the output tokens.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// A plain variable's value (exact match, case-sensitive).
    pub fn variable_value(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(String::as_str)
    }
}

/// One generation response from a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponse {
    /// The generated text.
    pub text: String,
    /// Input tokens consumed (provider-reported).
    pub tokens_in: u64,
    /// Output tokens produced (provider-reported).
    pub tokens_out: u64,
}

/// The model-provider contract (E3-4): applications implement or configure
/// one provider and hand it to [`run_flow`](crate::run_flow).
///
/// Implementations must be deterministic-free of hidden global state:
/// the reference suite relies on the mock provider being a pure function of
/// the request.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Generate one completion for `request`.
    async fn generate(&self, request: ModelRequest) -> anyhow::Result<ModelResponse>;
}

/// Provider identity plus the per-model price table (E3-4 cost helper).
///
/// [`cost_for`](Self::cost_for) maps a [`ModelResponse`] onto the E3-1
/// [`TaskCost`]: `amount_micros = (tokens_in * input_price +
/// tokens_out * output_price) / 1000`, all integer math, in
/// [`currency`](Self::currency) micro units. The default is the mock
/// provider's pricing (250 / 500 micro-USD per 1k input / output tokens) so
/// reference envelopes carry stable, assertable costs:
///
/// ```text
/// summarize: (48 * 250 + 12 * 500) / 1000 = 18 micro-USD
/// classify:  (24 * 250 +  2 * 500) / 1000 =  7 micro-USD
/// echo:      (16 * 250 +  8 * 500) / 1000 =  8 micro-USD
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// The model identity recorded in every task envelope.
    pub model: ModelRef,
    /// Currency of `amount_micros` (ISO 4217 code, at most 8 bytes).
    pub currency: String,
    /// Price of 1000 input tokens in micro units of `currency`.
    pub input_price_micros_per_1k: u64,
    /// Price of 1000 output tokens in micro units of `currency`.
    pub output_price_micros_per_1k: u64,
}

impl ProviderConfig {
    /// The mock provider's default identity and pricing.
    pub fn mock() -> Self {
        Self {
            model: ModelRef {
                provider: "mock".to_string(),
                name: MOCK_MODEL_NAME.to_string(),
                version: Some(MOCK_MODEL_VERSION.to_string()),
            },
            currency: "USD".to_string(),
            input_price_micros_per_1k: 250,
            output_price_micros_per_1k: 500,
        }
    }

    /// Map one response's token counts onto the E3-1 [`TaskCost`] using this
    /// price table (integer math; see the type docs for the worked examples).
    ///
    /// `duration_ms` is left `None`: the reference suite uses no clock.
    /// `tokens_in` / `tokens_out` ride along verbatim.
    pub fn cost_for(&self, response: &ModelResponse) -> TaskCost {
        let amount_micros = (response.tokens_in * self.input_price_micros_per_1k
            + response.tokens_out * self.output_price_micros_per_1k)
            / 1_000;
        TaskCost {
            currency: Some(self.currency.clone()),
            amount_micros: Some(amount_micros),
            tokens_in: Some(response.tokens_in),
            tokens_out: Some(response.tokens_out),
            duration_ms: None,
        }
    }

    /// Builder: replace the model identity.
    pub fn with_model(mut self, model: ModelRef) -> Self {
        self.model = model;
        self
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self::mock()
    }
}

/// The mock provider's model name.
pub const MOCK_MODEL_NAME: &str = "mock-model-v1";

/// The mock provider's model version.
pub const MOCK_MODEL_VERSION: &str = "1";

/// Deterministic, offline model provider: the reference suite's default.
///
/// A pure function of the request — no randomness, no clock, no I/O. Routing
/// by prompt pattern:
///
/// | prompt contains | response text | tokens (in, out) |
/// |---|---|---|
/// | `summar` | [`MOCK_SUMMARY_TEXT`] | (48, 12) |
/// | `classify` | [`MOCK_APPROVED`] / [`MOCK_REJECTED`] — rejected iff the `risk` variable is exactly `"high"` | (24, 2) |
/// | anything else | `echo: {prompt}` | (16, 8) |
///
/// `max_tokens` clamps the reported output-token count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockModelProvider {
    config: ProviderConfig,
}

impl MockModelProvider {
    /// The mock provider with the default (mock) identity and pricing.
    pub fn new() -> Self {
        Self {
            config: ProviderConfig::mock(),
        }
    }

    /// The mock provider reporting a custom identity/pricing in envelopes.
    pub fn with_config(config: ProviderConfig) -> Self {
        Self { config }
    }

    /// The identity and price table recorded in task envelopes.
    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

impl Default for MockModelProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    async fn generate(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let prompt = request.prompt.as_str();
        let (text, tokens_in, tokens_out) = if prompt.contains("summar") {
            (
                MOCK_SUMMARY_TEXT.to_string(),
                mock_tokens::SUMMARY_IN,
                mock_tokens::SUMMARY_OUT,
            )
        } else if prompt.contains("classify") {
            let verdict = if request.variable_value("risk") == Some("high") {
                MOCK_REJECTED
            } else {
                MOCK_APPROVED
            };
            (
                verdict.to_string(),
                mock_tokens::CLASSIFY_IN,
                mock_tokens::CLASSIFY_OUT,
            )
        } else {
            (
                format!("echo: {prompt}"),
                mock_tokens::ECHO_IN,
                mock_tokens::ECHO_OUT,
            )
        };
        // Honor an output cap deterministically (never above the route's
        // fixed count; the text is not truncated — the count reports what a
        // capped real provider would have billed).
        let tokens_out = match request.max_tokens {
            Some(cap) => tokens_out.min(u64::from(cap)),
            None => tokens_out,
        };
        Ok(ModelResponse {
            text,
            tokens_in,
            tokens_out,
        })
    }
}

/// Error code prefix of [`UnconfiguredProvider`] failures.
pub const UNCONFIGURED_PROVIDER_CODE: &str = "provider_unconfigured";

/// The explicit "no provider configured" stub: every [`generate`] fails with
/// a clear error naming the options.
///
/// Always compiled in (no `http` feature needed) so an application that
/// forgets to wire a provider fails loudly at the first model task instead
/// of silently doing nothing.
///
/// [`generate`]: ModelProvider::generate
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnconfiguredProvider;

impl Default for UnconfiguredProvider {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ModelProvider for UnconfiguredProvider {
    async fn generate(&self, _request: ModelRequest) -> anyhow::Result<ModelResponse> {
        Err(anyhow!(
            "{UNCONFIGURED_PROVIDER_CODE}: no model provider is configured — pass one to \
             run_flow (MockModelProvider for offline/reference runs, or enable this crate's \
             `http` feature and construct OpenAiCompatProvider::from_env)"
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn generate(provider: &dyn ModelProvider, request: ModelRequest) -> ModelResponse {
        provider
            .generate(request)
            .await
            .expect("mock generate never fails")
    }

    #[tokio::test]
    async fn mock_summary_route_is_canned_and_fixed() {
        let provider = MockModelProvider::new();
        let response = generate(
            &provider,
            ModelRequest::new("summarize: invoice-2026-Q3.pdf;receipt-2026-Q3.pdf"),
        )
        .await;
        assert_eq!(response.text, MOCK_SUMMARY_TEXT);
        assert_eq!(response.tokens_in, 48);
        assert_eq!(response.tokens_out, 12);
    }

    #[tokio::test]
    async fn mock_classify_route_decides_by_risk_variable() {
        let provider = MockModelProvider::new();
        let low = generate(
            &provider,
            ModelRequest::new("classify: expense refund request").variable("risk", "low"),
        )
        .await;
        assert_eq!(low.text, MOCK_APPROVED);
        let high = generate(
            &provider,
            ModelRequest::new("classify: expense refund request").variable("risk", "high"),
        )
        .await;
        assert_eq!(high.text, MOCK_REJECTED);
        // Absent risk defaults to approved (fail-open only in the mock's
        // routing; the engine's tool boundary is what fails closed).
        let absent = generate(&provider, ModelRequest::new("classify: request")).await;
        assert_eq!(absent.text, MOCK_APPROVED);
        // Not exactly "high" -> approved.
        let caps = generate(
            &provider,
            ModelRequest::new("classify: request").variable("risk", "HIGH"),
        )
        .await;
        assert_eq!(caps.text, MOCK_APPROVED);
    }

    #[tokio::test]
    async fn mock_default_route_echoes_with_fixed_tokens() {
        let provider = MockModelProvider::new();
        let response = generate(
            &provider,
            ModelRequest::new("propose approval for: quarterly ledger adjustment"),
        )
        .await;
        assert_eq!(
            response.text,
            "echo: propose approval for: quarterly ledger adjustment"
        );
        assert_eq!(response.tokens_in, 16);
        assert_eq!(response.tokens_out, 8);
    }

    #[tokio::test]
    async fn mock_max_tokens_clamps_output_count_only() {
        let provider = MockModelProvider::new();
        let response = generate(
            &provider,
            ModelRequest::new("summarize: docs").max_tokens(5),
        )
        .await;
        assert_eq!(response.tokens_out, 5);
        assert_eq!(response.tokens_in, 48);
        assert_eq!(response.text, MOCK_SUMMARY_TEXT);
        // A cap above the fixed count changes nothing.
        let response = generate(
            &provider,
            ModelRequest::new("summarize: docs").max_tokens(u32::MAX),
        )
        .await;
        assert_eq!(response.tokens_out, 12);
    }

    #[tokio::test]
    async fn mock_is_deterministic_no_clock_no_randomness() {
        let provider = MockModelProvider::new();
        let a = generate(
            &provider,
            ModelRequest::new("classify: x").variable("risk", "high"),
        )
        .await;
        let b = generate(
            &provider,
            ModelRequest::new("classify: x").variable("risk", "high"),
        )
        .await;
        assert_eq!(a, b);
    }

    #[test]
    fn mock_config_defaults_and_override() {
        assert_eq!(MockModelProvider::new().config(), &ProviderConfig::mock());
        let custom = ProviderConfig::mock().with_model(ModelRef {
            provider: "acme".to_string(),
            name: "acme-llm-9".to_string(),
            version: None,
        });
        assert_eq!(
            MockModelProvider::with_config(custom.clone()).config(),
            &custom
        );
    }

    #[test]
    fn cost_for_maps_tokens_to_micro_amounts() {
        let config = ProviderConfig::mock();
        // The three reference routes (see ProviderConfig docs).
        let summary = ModelResponse {
            text: String::new(),
            tokens_in: 48,
            tokens_out: 12,
        };
        assert_eq!(config.cost_for(&summary).amount_micros, Some(18));
        let classify = ModelResponse {
            text: String::new(),
            tokens_in: 24,
            tokens_out: 2,
        };
        assert_eq!(config.cost_for(&classify).amount_micros, Some(7));
        let echo = ModelResponse {
            text: String::new(),
            tokens_in: 16,
            tokens_out: 8,
        };
        assert_eq!(config.cost_for(&echo).amount_micros, Some(8));

        let cost = config.cost_for(&summary);
        assert_eq!(cost.currency.as_deref(), Some("USD"));
        assert_eq!(cost.tokens_in, Some(48));
        assert_eq!(cost.tokens_out, Some(12));
        assert_eq!(cost.duration_ms, None);
    }

    #[test]
    fn cost_for_uses_integer_division_and_zero_tokens() {
        let config = ProviderConfig::mock();
        let zero = ModelResponse {
            text: String::new(),
            tokens_in: 0,
            tokens_out: 0,
        };
        assert_eq!(config.cost_for(&zero).amount_micros, Some(0));
        // (1 * 250 + 0 * 500) / 1000 = 0 (truncating integer division).
        let one = ModelResponse {
            text: String::new(),
            tokens_in: 1,
            tokens_out: 0,
        };
        assert_eq!(config.cost_for(&one).amount_micros, Some(0));
    }

    #[tokio::test]
    async fn unconfigured_provider_fails_with_clear_guidance() {
        let err = UnconfiguredProvider
            .generate(ModelRequest::new("anything"))
            .await
            .expect_err("stub must fail");
        let message = err.to_string();
        assert!(message.starts_with("provider_unconfigured"));
        assert!(message.contains("MockModelProvider"));
        assert!(message.contains("OpenAiCompatProvider"));
        assert!(message.contains("http"));
    }
}
