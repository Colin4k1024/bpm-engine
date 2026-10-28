//! OpenAI-compatible HTTP model provider (crate feature `http`, **off by
//! default**).
//!
//! The reference suite always runs offline against
//! [`MockModelProvider`](crate::MockModelProvider); this module is the
//! swappable *real* provider for applications. It speaks the
//! OpenAI-compatible chat-completions wire format, so it works against
//! OpenAI itself and any compatible gateway (vLLM, Ollama's OpenAI
//! endpoint, Azure-style proxies, ...).
//!
//! Configuration is **environment-only and read exactly once, at
//! construction** ([`OpenAiCompatProvider::from_env`]) — never during
//! [`generate`](ModelProvider::generate), so tests can never accidentally
//! depend on ambient credentials:
//!
//! | env var | meaning | default |
//! |---|---|---|
//! | `OPENAI_BASE_URL` | API root (without `/chat/completions`) | `https://api.openai.com/v1` |
//! | `OPENAI_API_KEY` | bearer token (**required** — construction fails without it) | — |
//! | `OPENAI_MODEL` | model name sent in requests | `gpt-4o-mini` |
//!
//! Enable with `bpm-engine-ai-reference = { features = ["http"] }`; the
//! feature also turns on TLS for the provider client. The token counts of
//! each response come from the API's `usage` block and map onto the E3-1
//! envelope cost via [`ProviderConfig`](crate::ProviderConfig) exactly like
//! the mock's.

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ModelProvider, ModelRequest, ModelResponse, ProviderConfig};

/// Env var carrying the API base URL.
pub const OPENAI_BASE_URL_ENV: &str = "OPENAI_BASE_URL";

/// Env var carrying the API key (required).
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Env var overriding the model name.
pub const OPENAI_MODEL_ENV: &str = "OPENAI_MODEL";

/// Default API base URL (`OPENAI_BASE_URL` unset).
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Default model name (`OPENAI_MODEL` unset).
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

/// Error code prefix of provider-side failures.
pub const OPENAI_PROVIDER_CODE: &str = "openai_provider_error";

/// An OpenAI-compatible chat-completions provider.
#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    config: ProviderConfig,
}

impl OpenAiCompatProvider {
    /// Construct from the worker process environment.
    ///
    /// Reads [`OPENAI_BASE_URL_ENV`] / [`OPENAI_API_KEY_ENV`] /
    /// [`OPENAI_MODEL_ENV`] **once, here** (never again); a missing API key
    /// is a deployment error surfaced at construction, not per task.
    ///
    /// The provider's `ProviderConfig` is the MOCK default (identity and
    /// mock pricing): applications wiring a real provider must pass their
    /// own pricing via [`Self::new`] (or `FlowOptions::pricing` on the
    /// runner) so envelopes report the real model and cost — `from_env`
    /// keeps the reference suite free of any price-table policy.
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var(OPENAI_API_KEY_ENV)
            .map_err(|_| anyhow!("{OPENAI_PROVIDER_CODE}: {OPENAI_API_KEY_ENV} is not set — cannot construct the real provider (the reference suite does not need it: use MockModelProvider)"))?;
        let base_url = std::env::var(OPENAI_BASE_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.to_string());
        let model =
            std::env::var(OPENAI_MODEL_ENV).unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());
        Ok(Self::new(base_url, api_key, model, ProviderConfig::mock()))
    }

    /// Construct explicitly (tests and applications that do not use env
    /// config).
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        config: ProviderConfig,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            config,
        }
    }

    /// The identity and price table recorded in task envelopes.
    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[async_trait]
impl ModelProvider for OpenAiCompatProvider {
    async fn generate(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages: [ChatMessage {
                role: "user",
                content: &request.prompt,
            }],
            max_tokens: request.max_tokens,
        };
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{OPENAI_PROVIDER_CODE}: request to {url} failed"))?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "{OPENAI_PROVIDER_CODE}: {url} returned HTTP {status}: {message}"
            ));
        }
        let parsed: ChatResponse = response
            .json()
            .await
            .with_context(|| format!("{OPENAI_PROVIDER_CODE}: unparseable response from {url}"))?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| {
                anyhow!("{OPENAI_PROVIDER_CODE}: response carried no message content")
            })?;
        let usage = parsed.usage;
        let (tokens_in, tokens_out) = match &usage {
            Some(usage) => (
                usage.prompt_tokens.unwrap_or(0),
                usage.completion_tokens.unwrap_or(0),
            ),
            None => (0, 0),
        };
        Ok(ModelResponse {
            text,
            tokens_in,
            tokens_out,
        })
    }
}
