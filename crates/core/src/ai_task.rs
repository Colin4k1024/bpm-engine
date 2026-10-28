//! AI task envelope (E3-1): typed execution metadata for AI-driven external
//! tasks.
//!
//! External-task variables remain the input/output channel of the engine
//! (E2-3, `HashMap<String, String>` with self-describing `__var:1:` value
//! envelopes). This module layers a **well-known, typed metadata envelope**
//! on top of that channel so AI workers and clients can carry, alongside the
//! payload variables:
//!
//! - task input/output JSON Schemas,
//! - a stable [`execution_id`] (task id + attempt),
//! - an idempotency key for side-effect dedup,
//! - timeout and retry category,
//! - the model and tool versions used,
//! - the cost of the execution ([`TaskCost`], E6-1),
//! - artifact references (E2-3 artifact envelopes).
//!
//! The envelope is stored as ONE variable under the reserved key
//! [`AI_TASK_ENVELOPE_KEY`], serialized as a `__var:1:` JSON envelope
//! (the whole [`AiTaskEnvelope`] is the `val`). The existing external task
//! API is untouched: complete/fail bodies are extended with OPTIONAL
//! `envelope`/`category` fields, and a worker that ignores the envelope
//! behaves exactly as before.
//!
//! This module is pure data + validation, like the rest of the variable
//! contract; the engine never interprets schema contents, never dereferences
//! artifact URIs, and does not change retry behavior based on
//! [`RetryCategory`] (see `docs/ai-task-envelope.md`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::variable_contract::{parse_envelope, VariableEnvelope, ENVELOPE_PREFIX};

/// Reserved variable key carrying the serialized [`AiTaskEnvelope`].
///
/// Reserved at every REST write boundary (start-instance, user-task
/// complete, external-task complete): a user-supplied variable under this
/// key is rejected with 4xx `reserved_variable_key` so the engine (and only
/// the engine) owns the envelope slot.
pub const AI_TASK_ENVELOPE_KEY: &str = "__task_envelope";

/// The only envelope schema version this build understands.
pub const AI_TASK_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Maximum accepted [`TaskCost::currency`] length in bytes (E6-1): enough
/// for any ISO 4217 code or provider credit denomination, bounded so the
/// field cannot be abused as a free-text channel.
pub const MAX_COST_CURRENCY_BYTES: usize = 8;

/// Derive the execution id: `"<task_id>:<attempt>"`.
///
/// `attempt` is worker-tracked (0 on the first execution; the engine's
/// `retries` counter decreases from its initial value, so workers should
/// count their own attempts). The task id alone is stable across retries —
/// the idempotency key, not the execution id, is the side-effect dedup key.
pub fn execution_id(task_id: &str, attempt: u32) -> String {
    format!("{}:{}", task_id, attempt)
}

/// Errors from the AI task envelope contract.
///
/// Every variant carries a stable machine-readable [`code`](Self::code);
/// REST boundaries report the envelope-level code `task_envelope_invalid`
/// with this sub-code in the message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AiTaskEnvelopeError {
    /// [`AiTaskEnvelope::schema_version`] is not
    /// [`AI_TASK_ENVELOPE_SCHEMA_VERSION`]. Future versions are rejected,
    /// never guessed at.
    #[error("unsupported schema version {version} (supported: {supported})")]
    UnsupportedVersion {
        /// The schema version as written in the envelope.
        version: u32,
        /// The schema version this build understands.
        supported: u32,
    },
    /// The stored envelope value could not be parsed into an
    /// [`AiTaskEnvelope`] (not a `__var:1:` JSON envelope, invalid JSON,
    /// wrong shape, or an unknown field — `deny_unknown_fields`).
    #[error("malformed task envelope: {reason}")]
    Malformed {
        /// Human-readable reason (included in REST error bodies).
        reason: String,
    },
    /// An artifact reference failed E2-3 URI/metadata validation
    /// (userinfo, fragment, length, scheme, sha256 shape).
    #[error("invalid artifact ref: {reason}")]
    InvalidArtifactRef {
        /// Human-readable reason from the underlying
        /// [`crate::variable_contract::VariableContractError`].
        reason: String,
    },
    /// The model reference is incomplete (empty provider/name/version).
    #[error("invalid model ref: {reason}")]
    InvalidModelRef {
        /// Human-readable reason naming the offending field.
        reason: String,
    },
    /// A tool reference is incomplete (empty name/version).
    #[error("invalid tool ref: {reason}")]
    InvalidToolRef {
        /// Human-readable reason naming the offending field.
        reason: String,
    },
    /// The envelope carries an empty `execution_id`.
    #[error("execution_id must not be empty")]
    EmptyExecutionId,
    /// The envelope carries an empty `idempotency_key`.
    #[error("idempotency_key must not be empty when present")]
    EmptyIdempotencyKey,
    /// The envelope carries a zero `timeout_secs`.
    #[error("timeout_secs must be greater than zero when present")]
    InvalidTimeout,
    /// `input_schema`/`output_schema` is present but not a JSON object.
    #[error("invalid JSON schema: {reason}")]
    InvalidSchema {
        /// Human-readable reason naming the offending field.
        reason: String,
    },
    /// The cost block is invalid (empty or oversized `currency`).
    #[error("invalid task cost: {reason}")]
    InvalidCost {
        /// Human-readable reason naming the offending field.
        reason: String,
    },
}

impl AiTaskEnvelopeError {
    /// Stable machine-readable error code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            AiTaskEnvelopeError::UnsupportedVersion { .. } => {
                "ai_task_envelope_unsupported_version"
            }
            AiTaskEnvelopeError::Malformed { .. } => "ai_task_envelope_malformed",
            AiTaskEnvelopeError::InvalidArtifactRef { .. } => {
                "ai_task_envelope_invalid_artifact_ref"
            }
            AiTaskEnvelopeError::InvalidModelRef { .. } => "ai_task_envelope_invalid_model_ref",
            AiTaskEnvelopeError::InvalidToolRef { .. } => "ai_task_envelope_invalid_tool_ref",
            AiTaskEnvelopeError::EmptyExecutionId => "ai_task_envelope_invalid_execution_id",
            AiTaskEnvelopeError::EmptyIdempotencyKey => "ai_task_envelope_invalid_idempotency_key",
            AiTaskEnvelopeError::InvalidTimeout => "ai_task_envelope_invalid_timeout",
            AiTaskEnvelopeError::InvalidSchema { .. } => "ai_task_envelope_invalid_schema",
            AiTaskEnvelopeError::InvalidCost { .. } => "ai_task_envelope_invalid_cost",
        }
    }
}

/// Retry category reported by a worker for a failed attempt.
///
/// v1 semantics: **recorded only** — carried in the envelope / error message
/// for observability and the dead-letter queue; the engine's retry behavior
/// is unchanged (transient vs. permanent retry policy is E3-2 scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetryCategory {
    /// The failure may resolve on retry (network, rate limit, timeout).
    Transient,
    /// The failure will not resolve on retry (bad input, policy refusal).
    Permanent,
}

/// Reference to the model that executed the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// Model provider identifier (non-empty), e.g. `"openai"`.
    pub provider: String,
    /// Model name (non-empty), e.g. `"gpt-4o"`.
    pub name: String,
    /// Provider-side model version when known (non-empty when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Reference to a tool invoked by the worker while executing the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRef {
    /// Tool name (non-empty), e.g. `"web_search"`.
    pub name: String,
    /// Tool version when known (non-empty when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Reference to an artifact produced by (or consumed by) the task.
///
/// Uses the same reference shape as the E2-3 artifact envelope
/// (`uri`/`size`/`sha256`/`media_type`); validation reuses
/// [`crate::variable_contract::validate_artifact_uri`]. The engine never
/// dereferences `uri`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Absolute artifact URI (no userinfo, no fragment, ≤ 2048 bytes).
    pub uri: String,
    /// SHA-256 digest of the artifact content as exactly 64 hex chars when
    /// known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Artifact byte size when known (≥ 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// IANA media type of the artifact content when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Cost of one AI task execution (E6-1).
///
/// Reported by the worker inside the envelope so the step trace can join
/// model/tool versions with what the attempt cost (money and tokens).
/// Every field is optional; a partial report is valid. `amount_micros` is
/// a micro-unit amount in [`currency`](Self::currency) (e.g.
/// `amount_micros: 1_500_000`, `currency: "USD"` = 1.50 USD) — integers
/// only, never a float. All numeric fields are `u64`, so non-negative by
/// construction (a negative JSON number fails to deserialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCost {
    /// Currency code of [`amount_micros`](Self::amount_micros), e.g.
    /// `"USD"`. Non-empty, at most 8 bytes when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Billed amount in micro units of `currency` (1_000_000 = 1 unit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_micros: Option<u64>,
    /// Input tokens consumed by the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<u64>,
    /// Output tokens produced by the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<u64>,
    /// Wall-clock duration of the attempt in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Typed execution metadata for an AI external task (E3-1).
///
/// Serialized with snake_case JSON keys (matching the variable contract and
/// DSL conventions); unknown fields are rejected (`deny_unknown_fields`).
/// The stored form is the E2-3 value envelope
/// `__var:1:{"t":"json","val":<envelope JSON>}` — see
/// `docs/ai-task-envelope.md` for the field table and worker guidance.
///
/// # Example
///
/// ```
/// use bpm_engine_core::{execution_id, AiTaskEnvelope, ArtifactRef, TaskCost};
///
/// let envelope = AiTaskEnvelope {
///     schema_version: 1,
///     execution_id: execution_id("task-1", 0),
///     idempotency_key: Some("hash-of-input+model".to_string()),
///     timeout_secs: Some(120),
///     retry_category: None,
///     model: None,
///     tools: vec![],
///     cost: Some(TaskCost {
///         currency: Some("USD".to_string()),
///         amount_micros: Some(1_500_000),
///         tokens_in: Some(1_204),
///         tokens_out: Some(37),
///         duration_ms: Some(980),
///     }),
///     artifacts: vec![ArtifactRef {
///         uri: "https://storage.example.com/out/report.md".to_string(),
///         sha256: None,
///         size: Some(2048),
///         media_type: Some("text/markdown".to_string()),
///     }],
///     input_schema: None,
///     output_schema: None,
/// };
/// assert!(envelope.validate().is_ok());
///
/// // Serializes as a __var:1: JSON envelope string.
/// let value = envelope.to_envelope_value();
/// assert!(value.starts_with("__var:1:"));
///
/// let mut vars = std::collections::HashMap::new();
/// vars.insert("__task_envelope".to_string(), value);
/// let parsed = AiTaskEnvelope::from_variables(&vars).unwrap().unwrap();
/// assert_eq!(parsed, envelope);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiTaskEnvelope {
    /// Envelope schema version; must equal
    /// [`AI_TASK_ENVELOPE_SCHEMA_VERSION`] (1).
    pub schema_version: u32,
    /// Stable execution identifier, `"<task_id>:<attempt>"` — derive with
    /// [`execution_id`].
    pub execution_id: String,
    /// Worker-chosen side-effect dedup key. Guidance: a deterministic hash
    /// over the task input + model + tool versions, so a redelivered
    /// attempt with the same key maps to the same side effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Worker-side timeout hint in seconds (informational in v1; lease +
    /// node timeout remain authoritative). Must be > 0 when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Retry category of the last attempt. Recorded only in v1 — retry
    /// behavior is unchanged (E3-2 scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_category: Option<RetryCategory>,
    /// The model that executed the task, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Tools invoked by the worker while executing the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolRef>,
    /// Cost of the execution (E6-1, additive optional field of envelope
    /// v1): currency + micro-unit amount, token counts, duration. A
    /// partial report is valid; all numeric fields are non-negative `u64`
    /// (negative JSON numbers fail to deserialize).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<TaskCost>,
    /// Artifacts produced by (or consumed by) the task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    /// JSON Schema fragment (a JSON object) describing the expected input
    /// variables of the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// JSON Schema fragment (a JSON object) describing the output variables
    /// the task promises to produce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

impl AiTaskEnvelope {
    /// Validate this envelope's contract invariants.
    ///
    /// - `schema_version` must be [`AI_TASK_ENVELOPE_SCHEMA_VERSION`]
    /// - `execution_id` non-empty
    /// - `idempotency_key` non-empty when present
    /// - `timeout_secs` > 0 when present
    /// - `model`: non-empty provider/name (and version when present)
    /// - every tool: non-empty name (and version when present)
    /// - `cost.currency`: non-empty and at most 8 bytes when present
    ///   (numeric fields are `u64`, non-negative by construction)
    /// - every artifact: passes
    ///   [`validate_artifact_uri`](crate::variable_contract::validate_artifact_uri)
    ///   (URI rules + sha256 = 64 hex when present)
    /// - `input_schema`/`output_schema`: JSON objects when present
    ///
    /// Called at every write boundary that accepts an envelope; pure.
    pub fn validate(&self) -> Result<(), AiTaskEnvelopeError> {
        if self.schema_version != AI_TASK_ENVELOPE_SCHEMA_VERSION {
            return Err(AiTaskEnvelopeError::UnsupportedVersion {
                version: self.schema_version,
                supported: AI_TASK_ENVELOPE_SCHEMA_VERSION,
            });
        }
        if self.execution_id.is_empty() {
            return Err(AiTaskEnvelopeError::EmptyExecutionId);
        }
        if let Some(key) = &self.idempotency_key {
            if key.is_empty() {
                return Err(AiTaskEnvelopeError::EmptyIdempotencyKey);
            }
        }
        if let Some(timeout) = self.timeout_secs {
            if timeout == 0 {
                return Err(AiTaskEnvelopeError::InvalidTimeout);
            }
        }
        if let Some(model) = &self.model {
            if model.provider.is_empty() {
                return Err(AiTaskEnvelopeError::InvalidModelRef {
                    reason: "model.provider must not be empty".to_string(),
                });
            }
            if model.name.is_empty() {
                return Err(AiTaskEnvelopeError::InvalidModelRef {
                    reason: "model.name must not be empty".to_string(),
                });
            }
            if model.version.as_deref() == Some("") {
                return Err(AiTaskEnvelopeError::InvalidModelRef {
                    reason: "model.version must not be empty when present".to_string(),
                });
            }
        }
        for tool in &self.tools {
            if tool.name.is_empty() {
                return Err(AiTaskEnvelopeError::InvalidToolRef {
                    reason: "tools[].name must not be empty".to_string(),
                });
            }
            if tool.version.as_deref() == Some("") {
                return Err(AiTaskEnvelopeError::InvalidToolRef {
                    reason: "tools[].version must not be empty when present".to_string(),
                });
            }
        }
        if let Some(cost) = &self.cost {
            // Non-negativity of the numeric fields is enforced by the u64
            // types themselves (negative JSON fails to deserialize); here
            // only the currency shape is checked.
            if let Some(currency) = &cost.currency {
                if currency.is_empty() {
                    return Err(AiTaskEnvelopeError::InvalidCost {
                        reason: "cost.currency must not be empty when present".to_string(),
                    });
                }
                if currency.len() > MAX_COST_CURRENCY_BYTES {
                    return Err(AiTaskEnvelopeError::InvalidCost {
                        reason: format!(
                            "cost.currency must be at most {MAX_COST_CURRENCY_BYTES} bytes \
                             when present, got {}",
                            currency.len()
                        ),
                    });
                }
            }
        }
        for artifact in &self.artifacts {
            if let Err(e) = crate::variable_contract::validate_artifact_uri(
                &artifact.uri,
                artifact.size,
                artifact.sha256.as_deref(),
            ) {
                return Err(AiTaskEnvelopeError::InvalidArtifactRef {
                    reason: e.to_string(),
                });
            }
        }
        for (field, schema) in [
            ("input_schema", &self.input_schema),
            ("output_schema", &self.output_schema),
        ] {
            if let Some(schema) = schema {
                if !schema.is_object() {
                    return Err(AiTaskEnvelopeError::InvalidSchema {
                        reason: format!("{field} must be a JSON object"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Serialize into the storage string form: a `__var:1:` JSON envelope
    /// carrying the whole envelope as `val`.
    ///
    /// Infallible in practice: JSON values cannot hold non-finite floats,
    /// so `Value` serialization cannot fail. Does NOT run
    /// [`validate`](Self::validate) — boundaries validate before accepting.
    pub fn to_envelope_value(&self) -> String {
        let val = serde_json::to_value(self)
            .expect("AiTaskEnvelope serializes to a JSON Value infallibly");
        VariableEnvelope::Json { val }.to_storage_string()
    }

    /// Parse an envelope previously stored at [`AI_TASK_ENVELOPE_KEY`].
    ///
    /// Errors are [`AiTaskEnvelopeError::Malformed`] for every parse failure:
    /// the value is not envelope-family input (plain string or
    /// `__encrypted:` ciphertext), it carries an unsupported `__var:`
    /// version, it is an artifact envelope instead of a JSON envelope, its
    /// JSON body is invalid, or it does not deserialize into
    /// [`AiTaskEnvelope`] (missing/unknown fields — `deny_unknown_fields`).
    /// Validation of the parsed envelope is the caller's job
    /// ([`validate`](Self::validate)).
    pub fn from_envelope_value(value: &str) -> Result<AiTaskEnvelope, AiTaskEnvelopeError> {
        let malformed = |reason: String| AiTaskEnvelopeError::Malformed { reason };
        match parse_envelope(value) {
            // Plain string or __encrypted: ciphertext: not an envelope.
            None => Err(malformed(format!(
                "value at `{AI_TASK_ENVELOPE_KEY}` is not an envelope \
                 (expected `{ENVELOPE_PREFIX}{{\"t\":\"json\",...}}`)"
            ))),
            // E.g. __var:2: — unsupported envelope version: treat as opaque,
            // never interpret.
            Some(Err(e)) => Err(malformed(format!(
                "value at `{AI_TASK_ENVELOPE_KEY}` is not a usable envelope: {e}"
            ))),
            Some(Ok(VariableEnvelope::Artifact { .. })) => Err(malformed(
                "value at `__task_envelope` must be a json envelope, got an artifact envelope"
                    .to_string(),
            )),
            Some(Ok(VariableEnvelope::Json { val })) => {
                serde_json::from_value::<AiTaskEnvelope>(val).map_err(|e| {
                    malformed(format!(
                        "invalid AiTaskEnvelope JSON at `__task_envelope`: {e}"
                    ))
                })
            }
        }
    }

    /// Read an [`AiTaskEnvelope`] out of a variable map.
    ///
    /// Returns `None` when the map does not carry [`AI_TASK_ENVELOPE_KEY`]
    /// (the common, pre-E3-1 case). Returns `Some(Err(..))` when the key is
    /// present but its value does not parse into an envelope — the caller
    /// should treat that as a contract violation, not as "no envelope".
    pub fn from_variables(
        vars: &HashMap<String, String>,
    ) -> Option<Result<AiTaskEnvelope, AiTaskEnvelopeError>> {
        let raw = vars.get(AI_TASK_ENVELOPE_KEY)?;
        Some(Self::from_envelope_value(raw))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_envelope() -> AiTaskEnvelope {
        AiTaskEnvelope {
            schema_version: 1,
            execution_id: execution_id("task-1", 0),
            idempotency_key: Some("9f2c...e1".to_string()),
            timeout_secs: Some(120),
            retry_category: Some(RetryCategory::Transient),
            model: Some(ModelRef {
                provider: "openai".to_string(),
                name: "gpt-4o".to_string(),
                version: Some("2024-08-06".to_string()),
            }),
            tools: vec![
                ToolRef {
                    name: "web_search".to_string(),
                    version: Some("2.1".to_string()),
                },
                ToolRef {
                    name: "calculator".to_string(),
                    version: None,
                },
            ],
            cost: Some(TaskCost {
                currency: Some("USD".to_string()),
                amount_micros: Some(1_500_000),
                tokens_in: Some(1_204),
                tokens_out: Some(37),
                duration_ms: Some(980),
            }),
            artifacts: vec![ArtifactRef {
                uri: "https://storage.example.com/out/report.md".to_string(),
                sha256: Some("a".repeat(64)),
                size: Some(2048),
                media_type: Some("text/markdown".to_string()),
            }],
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "topic": { "type": "string" } },
                "required": ["topic"]
            })),
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "summary": { "type": "string" } }
            })),
        }
    }

    // --- Round trips ---------------------------------------------------------

    #[test]
    fn round_trip_full_envelope() {
        let envelope = valid_envelope();
        let value = envelope.to_envelope_value();
        assert!(value.starts_with(ENVELOPE_PREFIX));
        let mut vars = HashMap::new();
        vars.insert(AI_TASK_ENVELOPE_KEY.to_string(), value);
        assert_eq!(
            AiTaskEnvelope::from_variables(&vars),
            Some(Ok(envelope.clone()))
        );
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn round_trip_minimal_envelope() {
        let envelope = AiTaskEnvelope {
            schema_version: 1,
            execution_id: execution_id("t", 7),
            idempotency_key: None,
            timeout_secs: None,
            retry_category: None,
            model: None,
            tools: vec![],
            cost: None,
            artifacts: vec![],
            input_schema: None,
            output_schema: None,
        };
        let value = envelope.to_envelope_value();
        assert!(value.starts_with(ENVELOPE_PREFIX));
        // Compact form: optional fields and empty lists are omitted, so the
        // only keys are schema_version and execution_id (key order inside
        // `val` follows serde_json's default map ordering — deterministic
        // per build; round trips do not depend on it).
        assert!(value.contains("schema_version"));
        assert!(value.contains("\"execution_id\":\"t:7\""));
        assert_eq!(
            AiTaskEnvelope::from_envelope_value(&value),
            Ok(envelope.clone())
        );
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn to_envelope_value_is_a_json_envelope() {
        let value = valid_envelope().to_envelope_value();
        match parse_envelope(&value) {
            Some(Ok(VariableEnvelope::Json { val })) => {
                // The whole AiTaskEnvelope is the val.
                assert_eq!(val["execution_id"], "task-1:0");
                assert_eq!(val["schema_version"], 1);
            }
            other => panic!("expected a json envelope, got {other:?}"),
        }
    }

    // --- cost (E6-1, additive optional field of envelope v1) ---------------

    #[test]
    fn round_trip_envelope_with_cost() {
        let mut envelope = valid_envelope();
        envelope.cost = Some(TaskCost {
            currency: Some("USD".to_string()),
            amount_micros: Some(2_750_000),
            tokens_in: Some(9_000),
            tokens_out: Some(120),
            duration_ms: Some(1_400),
        });
        let value = envelope.to_envelope_value();
        let mut vars = HashMap::new();
        vars.insert(AI_TASK_ENVELOPE_KEY.to_string(), value);
        assert_eq!(
            AiTaskEnvelope::from_variables(&vars),
            Some(Ok(envelope.clone()))
        );
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn old_envelope_without_cost_still_parses() {
        // Additive-field compatibility (E6-1): an envelope serialized by a
        // pre-cost writer carries no `cost` key and must parse with
        // cost = None.
        let mut envelope = valid_envelope();
        envelope.cost = None;
        let value = envelope.to_envelope_value();
        let parsed =
            AiTaskEnvelope::from_envelope_value(&value).expect("old envelope (no cost) must parse");
        assert_eq!(parsed.cost, None);
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn partial_cost_is_valid() {
        let mut envelope = valid_envelope();
        envelope.cost = Some(TaskCost {
            currency: None,
            amount_micros: None,
            tokens_in: Some(42),
            tokens_out: None,
            duration_ms: None,
        });
        assert!(envelope.validate().is_ok());
        let mut envelope = valid_envelope();
        envelope.cost = Some(TaskCost {
            currency: Some("EUR".to_string()),
            amount_micros: None,
            tokens_in: None,
            tokens_out: None,
            duration_ms: None,
        });
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn validate_empty_cost_currency() {
        let mut envelope = valid_envelope();
        envelope.cost = Some(TaskCost {
            currency: Some(String::new()),
            amount_micros: None,
            tokens_in: None,
            tokens_out: None,
            duration_ms: None,
        });
        let err = envelope
            .validate()
            .expect_err("empty cost.currency must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidCost { .. }));
        assert_eq!(err.code(), "ai_task_envelope_invalid_cost");
        assert!(err.to_string().contains("currency"));
    }

    #[test]
    fn validate_oversized_cost_currency() {
        let mut envelope = valid_envelope();
        envelope.cost = Some(TaskCost {
            currency: Some("CREDITS!!".to_string()),
            amount_micros: None,
            tokens_in: None,
            tokens_out: None,
            duration_ms: None,
        });
        let err = envelope
            .validate()
            .expect_err("9-byte cost.currency must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidCost { .. }));
        assert!(err.to_string().contains('8'));
        // Exactly 8 bytes is allowed.
        envelope.cost.as_mut().unwrap().currency = Some("CREDITS!".to_string());
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn cost_numbers_must_be_non_negative() {
        // Non-negativity is enforced by the u64 types: a negative JSON
        // number fails to DESERIALIZE (malformed), not validate.
        for json in [-1_i64, -12_i64, -3_i64, -100_i64] {
            let mut envelope = serde_json::to_value(valid_envelope()).unwrap();
            envelope["cost"] = serde_json::json!({ "tokens_in": json });
            let raw = VariableEnvelope::Json { val: envelope }.to_storage_string();
            let err = AiTaskEnvelope::from_envelope_value(&raw)
                .expect_err("negative cost numbers must be rejected at parse time");
            assert!(matches!(err, AiTaskEnvelopeError::Malformed { .. }));
            assert!(
                err.to_string().contains("expected u64"),
                "the malformed reason must name the u64 field type: {err}"
            );
        }
    }

    #[test]
    fn cost_denies_unknown_fields() {
        let mut envelope = serde_json::to_value(valid_envelope()).unwrap();
        envelope["cost"] = serde_json::json!({ "currency": "USD", "surcharge": 1 });
        let raw = VariableEnvelope::Json { val: envelope }.to_storage_string();
        let err = AiTaskEnvelope::from_envelope_value(&raw)
            .expect_err("unknown cost fields must be rejected");
        assert!(matches!(err, AiTaskEnvelopeError::Malformed { .. }));
        assert!(err.to_string().contains("surcharge"));
        // Top level: `cost` itself is a known field of v1 now.
        assert!(serde_json::from_value::<TaskCost>(serde_json::json!({
            "amount_micros": 5
        }))
        .is_ok());
    }

    // --- execution_id helper ---------------------------------------------------

    #[test]
    fn execution_id_format() {
        assert_eq!(execution_id("task-42", 0), "task-42:0");
        assert_eq!(execution_id("task-42", 3), "task-42:3");
        assert_eq!(execution_id("", 0), ":0");
    }

    // --- from_variables classification ------------------------------------------

    #[test]
    fn from_variables_without_key_is_none() {
        let vars = HashMap::new();
        assert!(AiTaskEnvelope::from_variables(&vars).is_none());
        let mut other = HashMap::new();
        other.insert("unrelated".to_string(), "value".to_string());
        assert!(AiTaskEnvelope::from_variables(&other).is_none());
    }

    #[test]
    fn from_variables_plain_value_is_malformed() {
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            "just a string".to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for plain string at the reserved key");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
        assert_eq!(e.code(), "ai_task_envelope_malformed");
        assert!(e.to_string().contains("not an envelope"));
    }

    #[test]
    fn from_variables_encrypted_value_is_malformed() {
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            "__encrypted:QUFB:QkJC".to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for ciphertext at the reserved key");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
    }

    #[test]
    fn from_variables_unsupported_envelope_version_is_malformed() {
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            r#"__var:2:{"t":"json","val":{}}"#.to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for a future envelope version");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
        assert!(e.to_string().contains("unsupported envelope version"));
    }

    #[test]
    fn from_variables_artifact_envelope_is_malformed() {
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            r#"__var:1:{"t":"artifact","uri":"https://x/y"}"#.to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for an artifact envelope at the reserved key");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
        assert!(e.to_string().contains("artifact envelope"));
    }

    #[test]
    fn from_variables_invalid_json_body_is_malformed() {
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            "__var:1:{oops".to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for invalid JSON");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
    }

    #[test]
    fn from_variables_missing_required_field_is_malformed() {
        // schema_version and execution_id are required.
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            r#"__var:1:{"t":"json","val":{"schema_version":1}}"#.to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for a missing execution_id");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
        assert!(e.to_string().contains("execution_id"));
    }

    #[test]
    fn from_variables_unknown_field_is_malformed() {
        // deny_unknown_fields: a v2-style body fails parsing, not guessing.
        let mut vars = HashMap::new();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            r#"__var:1:{"t":"json","val":{"schema_version":1,"execution_id":"t:0","future_field":true}}"#
                .to_string(),
        );
        let Some(Err(e)) = AiTaskEnvelope::from_variables(&vars) else {
            panic!("expected error for an unknown field");
        };
        assert!(matches!(e, AiTaskEnvelopeError::Malformed { .. }));
        assert!(e.to_string().contains("unknown field"));
        assert!(e.to_string().contains("future_field"));
    }

    #[test]
    fn from_variables_valid_envelope_parses() {
        let value = valid_envelope().to_envelope_value();
        let mut vars = HashMap::new();
        vars.insert(AI_TASK_ENVELOPE_KEY.to_string(), value);
        let parsed = AiTaskEnvelope::from_variables(&vars)
            .expect("key present")
            .expect("valid envelope");
        // Parsed but not yet validated: same object, so validation agrees.
        assert!(parsed.validate().is_ok());
    }

    // --- validate: every error ------------------------------------------------

    #[test]
    fn validate_unsupported_schema_version() {
        let mut envelope = valid_envelope();
        envelope.schema_version = 2;
        let err = envelope.validate().expect_err("schema_version 2 must fail");
        assert_eq!(
            err,
            AiTaskEnvelopeError::UnsupportedVersion {
                version: 2,
                supported: 1
            }
        );
        assert_eq!(err.code(), "ai_task_envelope_unsupported_version");
        assert!(err.to_string().contains('2'));
    }

    #[test]
    fn validate_empty_execution_id() {
        let mut envelope = valid_envelope();
        envelope.execution_id = String::new();
        let err = envelope
            .validate()
            .expect_err("empty execution_id must fail");
        assert_eq!(err, AiTaskEnvelopeError::EmptyExecutionId);
        assert_eq!(err.code(), "ai_task_envelope_invalid_execution_id");
    }

    #[test]
    fn validate_empty_idempotency_key() {
        let mut envelope = valid_envelope();
        envelope.idempotency_key = Some(String::new());
        let err = envelope
            .validate()
            .expect_err("empty idempotency_key must fail");
        assert_eq!(err, AiTaskEnvelopeError::EmptyIdempotencyKey);
        assert_eq!(err.code(), "ai_task_envelope_invalid_idempotency_key");
    }

    #[test]
    fn validate_zero_timeout() {
        let mut envelope = valid_envelope();
        envelope.timeout_secs = Some(0);
        let err = envelope.validate().expect_err("zero timeout must fail");
        assert_eq!(err, AiTaskEnvelopeError::InvalidTimeout);
        assert_eq!(err.code(), "ai_task_envelope_invalid_timeout");
    }

    #[test]
    fn validate_empty_model_provider() {
        let mut envelope = valid_envelope();
        envelope.model = Some(ModelRef {
            provider: String::new(),
            name: "gpt-4o".to_string(),
            version: None,
        });
        let err = envelope.validate().expect_err("empty provider must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidModelRef { .. }));
        assert_eq!(err.code(), "ai_task_envelope_invalid_model_ref");
        assert!(err.to_string().contains("provider"));
    }

    #[test]
    fn validate_empty_model_name() {
        let mut envelope = valid_envelope();
        envelope.model = Some(ModelRef {
            provider: "openai".to_string(),
            name: String::new(),
            version: None,
        });
        let err = envelope.validate().expect_err("empty model name must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidModelRef { .. }));
        assert!(err.to_string().contains("model.name"));
    }

    #[test]
    fn validate_empty_model_version() {
        let mut envelope = valid_envelope();
        envelope.model = Some(ModelRef {
            provider: "openai".to_string(),
            name: "gpt-4o".to_string(),
            version: Some(String::new()),
        });
        let err = envelope
            .validate()
            .expect_err("empty model version must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidModelRef { .. }));
        assert!(err.to_string().contains("model.version"));
    }

    #[test]
    fn validate_empty_tool_name() {
        let mut envelope = valid_envelope();
        envelope.tools = vec![ToolRef {
            name: String::new(),
            version: None,
        }];
        let err = envelope.validate().expect_err("empty tool name must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidToolRef { .. }));
        assert_eq!(err.code(), "ai_task_envelope_invalid_tool_ref");
        assert!(err.to_string().contains("tools[].name"));
    }

    #[test]
    fn validate_empty_tool_version() {
        let mut envelope = valid_envelope();
        envelope.tools = vec![ToolRef {
            name: "web_search".to_string(),
            version: Some(String::new()),
        }];
        let err = envelope
            .validate()
            .expect_err("empty tool version must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidToolRef { .. }));
        assert!(err.to_string().contains("tools[].version"));
    }

    #[test]
    fn validate_relative_artifact_uri() {
        let mut envelope = valid_envelope();
        envelope.artifacts = vec![ArtifactRef {
            uri: "not-a-uri".to_string(),
            sha256: None,
            size: None,
            media_type: None,
        }];
        let err = envelope
            .validate()
            .expect_err("relative artifact URI must fail");
        assert!(matches!(
            err,
            AiTaskEnvelopeError::InvalidArtifactRef { .. }
        ));
        assert_eq!(err.code(), "ai_task_envelope_invalid_artifact_ref");
        // Underlying E2-3 reason is preserved.
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn validate_artifact_uri_with_userinfo() {
        let mut envelope = valid_envelope();
        envelope.artifacts = vec![ArtifactRef {
            uri: "https://user@x.example.com/a".to_string(),
            sha256: None,
            size: None,
            media_type: None,
        }];
        let err = envelope
            .validate()
            .expect_err("artifact userinfo must fail");
        assert!(matches!(
            err,
            AiTaskEnvelopeError::InvalidArtifactRef { .. }
        ));
        assert!(err.to_string().contains("userinfo"));
    }

    #[test]
    fn validate_artifact_bad_sha256_shape() {
        let mut envelope = valid_envelope();
        envelope.artifacts = vec![ArtifactRef {
            uri: "https://x.example.com/a".to_string(),
            sha256: Some("a".repeat(63)),
            size: None,
            media_type: None,
        }];
        let err = envelope.validate().expect_err("63-char sha256 must fail");
        assert!(matches!(
            err,
            AiTaskEnvelopeError::InvalidArtifactRef { .. }
        ));
        assert!(err.to_string().contains("64"));
    }

    #[test]
    fn validate_non_object_input_schema() {
        let mut envelope = valid_envelope();
        envelope.input_schema = Some(serde_json::json!(42));
        let err = envelope
            .validate()
            .expect_err("non-object input_schema must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidSchema { .. }));
        assert_eq!(err.code(), "ai_task_envelope_invalid_schema");
        assert!(err.to_string().contains("input_schema"));
    }

    #[test]
    fn validate_non_object_output_schema() {
        let mut envelope = valid_envelope();
        envelope.output_schema = Some(serde_json::json!([1, 2]));
        let err = envelope
            .validate()
            .expect_err("non-object output_schema must fail");
        assert!(matches!(err, AiTaskEnvelopeError::InvalidSchema { .. }));
        assert!(err.to_string().contains("output_schema"));
    }

    #[test]
    fn validate_happy_paths() {
        assert!(valid_envelope().validate().is_ok());
        // A reserved artifact:// scheme is valid-but-opaque (E2-3).
        let mut envelope = valid_envelope();
        envelope.artifacts = vec![ArtifactRef {
            uri: "artifact://reports/2026/q3".to_string(),
            sha256: None,
            size: None,
            media_type: None,
        }];
        assert!(envelope.validate().is_ok());
        // Empty schemas list / object schemas pass.
        let mut minimal = AiTaskEnvelope {
            schema_version: 1,
            execution_id: execution_id("t", 0),
            idempotency_key: None,
            timeout_secs: None,
            retry_category: None,
            model: None,
            tools: vec![],
            cost: None,
            artifacts: vec![],
            input_schema: Some(serde_json::json!({})),
            output_schema: None,
        };
        assert!(minimal.validate().is_ok());
        minimal.retry_category = Some(RetryCategory::Permanent);
        assert!(minimal.validate().is_ok());
    }

    // --- serde shape ----------------------------------------------------------

    #[test]
    fn retry_category_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&RetryCategory::Transient).unwrap(),
            r#""transient""#
        );
        assert_eq!(
            serde_json::to_string(&RetryCategory::Permanent).unwrap(),
            r#""permanent""#
        );
        assert_eq!(
            serde_json::from_str::<RetryCategory>(r#""permanent""#).unwrap(),
            RetryCategory::Permanent
        );
        assert!(serde_json::from_str::<RetryCategory>(r#""Permanent""#).is_err());
        assert!(serde_json::from_str::<RetryCategory>(r#""other""#).is_err());
    }

    #[test]
    fn envelope_json_uses_snake_case_keys() {
        let envelope = AiTaskEnvelope {
            schema_version: 1,
            execution_id: "t:0".to_string(),
            idempotency_key: Some("k".to_string()),
            timeout_secs: None,
            retry_category: Some(RetryCategory::Permanent),
            model: Some(ModelRef {
                provider: "p".to_string(),
                name: "m".to_string(),
                version: None,
            }),
            tools: vec![],
            cost: None,
            artifacts: vec![],
            input_schema: None,
            output_schema: None,
        };
        let val = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            val,
            serde_json::json!({
                "schema_version": 1,
                "execution_id": "t:0",
                "idempotency_key": "k",
                "retry_category": "permanent",
                "model": { "provider": "p", "name": "m" }
            })
        );
        // The storage form is a __var:1: json envelope whose val is exactly
        // that object.
        let value = envelope.to_envelope_value();
        assert!(value.starts_with(ENVELOPE_PREFIX));
        match parse_envelope(&value) {
            Some(Ok(VariableEnvelope::Json { val })) => {
                assert_eq!(val, serde_json::to_value(&envelope).unwrap());
            }
            other => panic!("expected a json envelope, got {other:?}"),
        }
    }

    #[test]
    fn dto_deny_unknown_fields() {
        // Top level.
        assert!(serde_json::from_value::<AiTaskEnvelope>(serde_json::json!({
            "schema_version": 1,
            "execution_id": "t:0",
            "surprise": true
        }))
        .is_err());
        // Nested refs.
        assert!(serde_json::from_value::<ModelRef>(serde_json::json!({
            "provider": "p", "name": "m", "extra": 1
        }))
        .is_err());
        assert!(serde_json::from_value::<ToolRef>(serde_json::json!({
            "name": "t", "extra": 1
        }))
        .is_err());
        assert!(serde_json::from_value::<ArtifactRef>(serde_json::json!({
            "uri": "https://x/y", "extra": 1
        }))
        .is_err());
    }

    #[test]
    fn error_codes_are_stable_snake_case() {
        let cases: Vec<AiTaskEnvelopeError> = vec![
            AiTaskEnvelopeError::UnsupportedVersion {
                version: 2,
                supported: 1,
            },
            AiTaskEnvelopeError::Malformed {
                reason: "x".to_string(),
            },
            AiTaskEnvelopeError::InvalidArtifactRef {
                reason: "x".to_string(),
            },
            AiTaskEnvelopeError::InvalidModelRef {
                reason: "x".to_string(),
            },
            AiTaskEnvelopeError::InvalidToolRef {
                reason: "x".to_string(),
            },
            AiTaskEnvelopeError::EmptyExecutionId,
            AiTaskEnvelopeError::EmptyIdempotencyKey,
            AiTaskEnvelopeError::InvalidTimeout,
            AiTaskEnvelopeError::InvalidSchema {
                reason: "x".to_string(),
            },
            AiTaskEnvelopeError::InvalidCost {
                reason: "x".to_string(),
            },
        ];
        let codes: Vec<&str> = cases.iter().map(|e| e.code()).collect();
        assert_eq!(
            codes,
            vec![
                "ai_task_envelope_unsupported_version",
                "ai_task_envelope_malformed",
                "ai_task_envelope_invalid_artifact_ref",
                "ai_task_envelope_invalid_model_ref",
                "ai_task_envelope_invalid_tool_ref",
                "ai_task_envelope_invalid_execution_id",
                "ai_task_envelope_invalid_idempotency_key",
                "ai_task_envelope_invalid_timeout",
                "ai_task_envelope_invalid_schema",
                "ai_task_envelope_invalid_cost",
            ]
        );
    }
}
