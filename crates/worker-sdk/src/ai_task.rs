//! AI task envelope helpers for workers (E3-1).
//!
//! Task variables remain the engine's input/output channel (E2-3 contract,
//! `docs/variables-and-artifacts.md`): fetch-and-lock delivers them
//! verbatim, and `complete` merges the submitted map into the task and the
//! instance. This module gives workers typed access on the way IN
//! ([`TaskInput`]) and a builder on the way OUT ([`TaskOutput`]) that rides
//! the [`AiTaskEnvelope`] along in the reserved `__task_envelope` variable —
//! so a worker completes exactly as today, with the envelope traveling in
//! the same variables map (no new endpoint, no new wire format).
//!
//! Re-exports the envelope types from `bpm-engine-core` so workers need one
//! import site.

use std::collections::HashMap;

use bpm_engine_core::variable_contract::{parse_envelope, VariableEnvelope};
use bpm_engine_core::{
    AiTaskEnvelope, AiTaskEnvelopeError, ArtifactRef, AI_TASK_ENVELOPE_KEY,
    AI_TASK_ENVELOPE_SCHEMA_VERSION,
};

// One import site for workers: the envelope types come from core.
pub use bpm_engine_core::{execution_id, ModelRef, RetryCategory, ToolRef};

use crate::client::EngineClient;

/// Build a minimal, valid [`AiTaskEnvelope`] for one task attempt:
/// `schema_version` 1 and `execution_id` derived via
/// [`execution_id`](`bpm_engine_core::execution_id`).
///
/// Workers then fill in the fields they know (idempotency key, model,
/// tools, artifacts, schemas, retry category) — every field is public.
pub fn new_envelope(task_id: &str, attempt: u32) -> AiTaskEnvelope {
    AiTaskEnvelope {
        schema_version: AI_TASK_ENVELOPE_SCHEMA_VERSION,
        execution_id: execution_id(task_id, attempt),
        idempotency_key: None,
        timeout_secs: None,
        retry_category: None,
        model: None,
        tools: Vec::new(),
        artifacts: Vec::new(),
        input_schema: None,
        output_schema: None,
    }
}

/// Typed read access over one external task's input variables.
///
/// Constructed from the `variables` map of a fetched
/// [`ExternalTask`](crate::types::ExternalTask) (delivered verbatim per the
/// E2-3 contract). Accessors are total: absent or non-matching values
/// return `None`/`Err` rather than panicking.
#[derive(Debug, Clone)]
pub struct TaskInput {
    variables: HashMap<String, String>,
}

impl TaskInput {
    /// Wrap a task's variable map (fetch-and-lock delivers envelope strings
    /// verbatim; nothing is parsed eagerly).
    pub fn from_task_variables(variables: &HashMap<String, String>) -> TaskInput {
        TaskInput {
            variables: variables.clone(),
        }
    }

    /// The full variable map, verbatim (plain strings, `__var:1:` envelope
    /// strings, `__encrypted:` ciphertext).
    pub fn variables(&self) -> &HashMap<String, String> {
        &self.variables
    }

    /// A variable's raw storage string, verbatim, whatever its form.
    pub fn raw(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(String::as_str)
    }

    /// A variable's value when it is a **plain legacy string** (not a
    /// `__var:` envelope, not `__encrypted:` ciphertext).
    ///
    /// These are the values gateway conditions and the EL see; envelope
    /// values are opaque to them.
    pub fn plain(&self, name: &str) -> Option<&str> {
        let value = self.variables.get(name)?;
        match parse_envelope(value) {
            None if !value.starts_with(bpm_engine_core::ENCRYPTED_PREFIX) => Some(value.as_str()),
            _ => None,
        }
    }

    /// A variable's JSON value when it carries a `__var:1:` **json**
    /// envelope. Returns `None` for plain strings, artifact envelopes,
    /// ciphertext, and unparseable/unsupported envelope values (treat those
    /// as opaque).
    pub fn json(&self, name: &str) -> Option<serde_json::Value> {
        match parse_envelope(self.variables.get(name)?) {
            Some(Ok(VariableEnvelope::Json { val })) => Some(val),
            _ => None,
        }
    }

    /// A variable's artifact reference when it carries a `__var:1:`
    /// **artifact** envelope. Returns `None` for every other value form.
    /// The URI is never dereferenced by this SDK — resolution is the
    /// worker's job (E2-3 / ADR-007).
    pub fn artifact(&self, name: &str) -> Option<ArtifactRef> {
        match parse_envelope(self.variables.get(name)?) {
            Some(Ok(VariableEnvelope::Artifact {
                uri,
                size,
                sha256,
                media_type,
            })) => Some(ArtifactRef {
                uri,
                sha256,
                size,
                media_type,
            }),
            _ => None,
        }
    }

    /// The task's [`AiTaskEnvelope`] when the input variables carry one
    /// under [`AI_TASK_ENVELOPE_KEY`] (e.g. an envelope set by an upstream
    /// AI task in the same instance).
    ///
    /// - `None` — no envelope in the input (the common case).
    /// - `Some(Ok(envelope))` — parsed; call
    ///   [`validate`](AiTaskEnvelope::validate) before trusting it.
    /// - `Some(Err(_))` — the reserved key holds a value that does not
    ///   parse; treat as a contract violation, not as "no envelope".
    pub fn envelope(&self) -> Option<Result<AiTaskEnvelope, AiTaskEnvelopeError>> {
        AiTaskEnvelope::from_variables(&self.variables)
    }
}

/// Builder for the variables a worker submits on `complete`, with the
/// [`AiTaskEnvelope`] riding along.
///
/// Output variables may be any mix of plain strings, `__var:1:` json
/// envelopes ([`json_variable`](Self::json_variable)) and artifact envelopes
/// ([`artifact_variable`](Self::artifact_variable)); pre-encoded envelope
/// strings can be passed through [`variable`](Self::variable) verbatim.
///
/// [`build`](Self::build) validates the envelope (so an invalid one fails
/// in the worker, before side effects and before the HTTP call) and
/// serializes it into [`AI_TASK_ENVELOPE_KEY`]. The envelope owns that key:
/// a manually set variable at it is overwritten. The server independently
/// re-validates and rejects user-supplied values at the reserved key
/// (4xx `reserved_variable_key`).
///
/// # Example
///
/// ```
/// use bpm_engine_worker_sdk::ai_task::{new_envelope, TaskOutput};
///
/// let output = TaskOutput::new()
///     .variable("verdict", "approved")
///     .json_variable("scores", serde_json::json!({"overall": 0.87}))
///     .envelope(new_envelope("task-1", 0));
/// let variables = output.build().expect("valid envelope");
/// assert!(variables.contains_key("__task_envelope"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct TaskOutput {
    variables: HashMap<String, String>,
    envelope: Option<AiTaskEnvelope>,
}

impl TaskOutput {
    /// An empty output.
    pub fn new() -> Self {
        Self::default()
    }

    /// One output variable, verbatim (a plain string or a pre-encoded
    /// `__var:1:` envelope string).
    ///
    /// The reserved [`AI_TASK_ENVELOPE_KEY`] is owned by
    /// [`envelope`](Self::envelope) — a value set here under that key is
    /// overwritten by `build` when an envelope is present (and rejected by
    /// the server otherwise).
    pub fn variable(mut self, name: &str, value: impl Into<String>) -> Self {
        self.variables.insert(name.to_string(), value.into());
        self
    }

    /// One output variable carrying a `__var:1:` **json** envelope.
    pub fn json_variable(mut self, name: &str, value: serde_json::Value) -> Self {
        let storage = VariableEnvelope::Json { val: value }.to_storage_string();
        self.variables.insert(name.to_string(), storage);
        self
    }

    /// One output variable carrying a `__var:1:` **artifact** envelope
    /// referencing an externally stored artifact (the engine never
    /// dereferences it).
    pub fn artifact_variable(mut self, name: &str, artifact: ArtifactRef) -> Self {
        let storage = VariableEnvelope::Artifact {
            uri: artifact.uri,
            size: artifact.size,
            sha256: artifact.sha256,
            media_type: artifact.media_type,
        }
        .to_storage_string();
        self.variables.insert(name.to_string(), storage);
        self
    }

    /// Set the whole [`AiTaskEnvelope`] (replaces any previously set one).
    /// See [`new_envelope`] for a minimal starting point.
    pub fn envelope(mut self, envelope: AiTaskEnvelope) -> Self {
        self.envelope = Some(envelope);
        self
    }

    /// The currently set envelope, if any (mutate its public fields in
    /// place — e.g. record the retry category before failing).
    pub fn envelope_mut(&mut self) -> Option<&mut AiTaskEnvelope> {
        self.envelope.as_mut()
    }

    /// Finish: validate the envelope (when present) and produce the final
    /// variable map for [`TaskResult::Complete`](crate::types::TaskResult).
    ///
    /// The serialized envelope is inserted at [`AI_TASK_ENVELOPE_KEY`] (the
    /// envelope owns that key). An invalid envelope is an
    /// [`AiTaskEnvelopeError`] here, in the worker, instead of a 4xx from
    /// the server after submission.
    pub fn build(self) -> Result<HashMap<String, String>, AiTaskEnvelopeError> {
        let TaskOutput {
            mut variables,
            envelope,
        } = self;
        if let Some(envelope) = envelope {
            envelope.validate()?;
            variables.insert(
                AI_TASK_ENVELOPE_KEY.to_string(),
                envelope.to_envelope_value(),
            );
        }
        Ok(variables)
    }
}

impl EngineClient {
    /// Complete a task with an AI task envelope riding along.
    ///
    /// The envelope is serialized into [`AI_TASK_ENVELOPE_KEY`] of the
    /// variables map (overwriting any value at that key) and submitted via
    /// the **existing** [`complete`](EngineClient::complete) call — same
    /// wire format, no new endpoint semantics. The envelope is not
    /// re-validated here; the server validates it (4xx
    /// `task_envelope_invalid`), so prefer validating client-side via
    /// [`TaskOutput::build`].
    pub async fn complete_with_envelope(
        &self,
        task_id: &str,
        worker_id: &str,
        mut variables: HashMap<String, String>,
        envelope: &AiTaskEnvelope,
    ) -> Result<(), crate::client::ClientError> {
        variables.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            envelope.to_envelope_value(),
        );
        self.complete(task_id, worker_id, variables).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn input_map() -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("plain".to_string(), "hello".to_string());
        vars.insert(
            "payload".to_string(),
            r#"__var:1:{"t":"json","val":{"items":[1,2]}}"#.to_string(),
        );
        vars.insert(
            "doc".to_string(),
            r#"__var:1:{"t":"artifact","uri":"https://x.example.com/doc.pdf","size":10,"sha256":null,"media_type":"application/pdf"}"#
                .to_string(),
        );
        vars.insert("cipher".to_string(), "__encrypted:QUFB:QkJC".to_string());
        vars
    }

    #[test]
    fn task_input_plain_and_raw() {
        let input = TaskInput::from_task_variables(&input_map());
        assert_eq!(input.plain("plain"), Some("hello"));
        assert_eq!(input.plain("missing"), None);
        // Envelopes and ciphertext are not plain values.
        assert_eq!(input.plain("payload"), None);
        assert_eq!(input.plain("doc"), None);
        assert_eq!(input.plain("cipher"), None);
        // raw() is verbatim for every form.
        assert_eq!(input.raw("cipher"), Some("__encrypted:QUFB:QkJC"));
        assert_eq!(
            input.raw("payload"),
            Some(r#"__var:1:{"t":"json","val":{"items":[1,2]}}"#)
        );
    }

    #[test]
    fn task_input_json() {
        let input = TaskInput::from_task_variables(&input_map());
        assert_eq!(
            input.json("payload"),
            Some(serde_json::json!({"items": [1, 2]}))
        );
        // Non-json-envelope forms are None.
        assert_eq!(input.json("plain"), None);
        assert_eq!(input.json("doc"), None);
        assert_eq!(input.json("cipher"), None);
        assert_eq!(input.json("missing"), None);
    }

    #[test]
    fn task_input_artifact() {
        let input = TaskInput::from_task_variables(&input_map());
        assert_eq!(
            input.artifact("doc"),
            Some(ArtifactRef {
                uri: "https://x.example.com/doc.pdf".to_string(),
                sha256: None,
                size: Some(10),
                media_type: Some("application/pdf".to_string()),
            })
        );
        // Non-artifact forms are None.
        assert_eq!(input.artifact("plain"), None);
        assert_eq!(input.artifact("payload"), None);
        assert_eq!(input.artifact("missing"), None);
    }

    #[test]
    fn task_input_unsupported_and_malformed_values_are_opaque() {
        let mut vars = input_map();
        vars.insert(
            "future".to_string(),
            r#"__var:2:{"t":"json","val":1}"#.to_string(),
        );
        vars.insert("broken".to_string(), "__var:1:{oops".to_string());
        let input = TaskInput::from_task_variables(&vars);
        assert_eq!(input.json("future"), None);
        assert_eq!(input.json("broken"), None);
        assert_eq!(input.artifact("future"), None);
        // raw() still delivers them verbatim.
        assert!(input.raw("future").unwrap().starts_with("__var:2:"));
    }

    #[test]
    fn task_input_envelope_absent() {
        let input = TaskInput::from_task_variables(&input_map());
        assert!(input.envelope().is_none());
    }

    #[test]
    fn task_input_envelope_present_and_valid() {
        let mut vars = input_map();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            new_envelope("task-9", 2).to_envelope_value(),
        );
        let input = TaskInput::from_task_variables(&vars);
        let envelope = input.envelope().expect("envelope present").unwrap();
        assert_eq!(envelope.execution_id, execution_id("task-9", 2));
        assert_eq!(envelope.schema_version, 1);
    }

    #[test]
    fn task_input_envelope_malformed() {
        let mut vars = input_map();
        vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            "not-an-envelope".to_string(),
        );
        let input = TaskInput::from_task_variables(&vars);
        let err = input.envelope().expect("key present").unwrap_err();
        assert!(matches!(err, AiTaskEnvelopeError::Malformed { .. }));
    }

    #[test]
    fn new_envelope_is_minimal_and_valid() {
        let envelope = new_envelope("task-1", 0);
        assert_eq!(envelope.schema_version, 1);
        assert_eq!(envelope.execution_id, "task-1:0");
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn task_output_builds_mixed_variables() {
        let output = TaskOutput::new()
            .variable("verdict", "approved")
            .json_variable("scores", serde_json::json!({"overall": 0.87}))
            .artifact_variable(
                "report",
                ArtifactRef {
                    uri: "artifact://reports/q3".to_string(),
                    sha256: None,
                    size: None,
                    media_type: None,
                },
            );
        let variables = output.build().expect("no envelope, always valid");
        assert_eq!(variables.get("verdict").unwrap(), "approved");
        assert_eq!(
            variables.get("scores").unwrap(),
            r#"__var:1:{"t":"json","val":{"overall":0.87}}"#
        );
        assert_eq!(
            variables.get("report").unwrap(),
            r#"__var:1:{"t":"artifact","uri":"artifact://reports/q3"}"#
        );
        assert!(!variables.contains_key(AI_TASK_ENVELOPE_KEY));
    }

    #[test]
    fn task_output_without_envelope_is_backward_compatible() {
        let variables = TaskOutput::new().variable("k", "v").build().expect("valid");
        assert_eq!(variables.get("k"), Some(&"v".to_string()));
        assert!(!variables.contains_key(AI_TASK_ENVELOPE_KEY));
    }

    #[test]
    fn task_output_envelope_rides_in_reserved_key() {
        let mut envelope = new_envelope("task-1", 0);
        envelope.idempotency_key = Some("idem-1".to_string());
        envelope.model = Some(ModelRef {
            provider: "openai".to_string(),
            name: "gpt-4o".to_string(),
            version: None,
        });
        let output = TaskOutput::new()
            .variable("verdict", "approved")
            .envelope(envelope.clone());
        let variables = output.build().expect("valid envelope");
        let stored = variables
            .get(AI_TASK_ENVELOPE_KEY)
            .expect("envelope stored");
        assert!(stored.starts_with("__var:1:"));
        assert_eq!(AiTaskEnvelope::from_envelope_value(stored), Ok(envelope));
    }

    #[test]
    fn task_output_envelope_owns_reserved_key() {
        // A manually set variable at the reserved key is overwritten by the
        // envelope (documented; the server rejects user values there).
        let output = TaskOutput::new()
            .variable(AI_TASK_ENVELOPE_KEY, "user-supplied")
            .envelope(new_envelope("task-1", 0));
        let variables = output.build().expect("valid envelope");
        let stored = variables.get(AI_TASK_ENVELOPE_KEY).unwrap();
        assert_eq!(
            AiTaskEnvelope::from_envelope_value(stored).unwrap(),
            new_envelope("task-1", 0)
        );
    }

    #[test]
    fn task_output_build_validates_envelope() {
        let mut envelope = new_envelope("task-1", 0);
        envelope.artifacts = vec![ArtifactRef {
            uri: "not-a-uri".to_string(),
            sha256: None,
            size: None,
            media_type: None,
        }];
        let err = TaskOutput::new()
            .envelope(envelope)
            .build()
            .expect_err("invalid artifact URI must fail client-side");
        assert_eq!(err.code(), "ai_task_envelope_invalid_artifact_ref");

        let mut zero_timeout = new_envelope("task-1", 0);
        zero_timeout.timeout_secs = Some(0);
        let err = TaskOutput::new()
            .envelope(zero_timeout)
            .build()
            .expect_err("zero timeout must fail client-side");
        assert_eq!(err.code(), "ai_task_envelope_invalid_timeout");
    }

    #[test]
    fn task_output_envelope_mut_allows_late_field_updates() {
        let mut output = TaskOutput::new();
        output = output.envelope(new_envelope("task-1", 1));
        output.envelope_mut().unwrap().retry_category = Some(RetryCategory::Transient);
        let variables = output.build().expect("valid");
        let envelope =
            AiTaskEnvelope::from_envelope_value(variables.get(AI_TASK_ENVELOPE_KEY).unwrap())
                .unwrap();
        assert_eq!(envelope.retry_category, Some(RetryCategory::Transient));
    }

    /// The full worker-side round trip required by the E3-1 contract:
    /// TaskOutput -> variables -> complete body -> server-side parse (core)
    /// -> back to AiTaskEnvelope.
    #[test]
    fn task_output_full_round_trip_through_complete_body() {
        let mut envelope = new_envelope("task-1", 0);
        envelope.idempotency_key = Some("hash(input|model|tools)".to_string());
        envelope.timeout_secs = Some(120);
        envelope.retry_category = Some(RetryCategory::Permanent);
        envelope.model = Some(ModelRef {
            provider: "anthropic".to_string(),
            name: "claude-sonnet".to_string(),
            version: Some("2026-01".to_string()),
        });
        envelope.tools = vec![ToolRef {
            name: "web_search".to_string(),
            version: Some("2.1".to_string()),
        }];
        envelope.artifacts = vec![ArtifactRef {
            uri: "https://storage.example.com/out/report.md".to_string(),
            sha256: Some("b".repeat(64)),
            size: Some(4096),
            media_type: Some("text/markdown".to_string()),
        }];
        envelope.output_schema = Some(serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"]
        }));

        // Worker builds the output...
        let output = TaskOutput::new()
            .variable("summary", "Q3 revenue grew 12%.")
            .json_variable("metrics", serde_json::json!({"growth": 0.12}))
            .envelope(envelope.clone());
        let variables = output.build().expect("valid envelope");

        // ...serialized into the same complete-body shape the client sends...
        let body = serde_json::json!({
            "worker_id": "worker-1",
            "variables": variables,
        });
        let delivered: HashMap<String, String> =
            serde_json::from_value(body.get("variables").cloned().expect("variables present"))
                .expect("variables are a string map");

        // ...and the server side (core) parses the envelope back out.
        let parsed = AiTaskEnvelope::from_variables(&delivered)
            .expect("envelope key present")
            .expect("valid envelope");
        assert_eq!(parsed, envelope);
        assert!(parsed.validate().is_ok());
        // The payload variables ride along unchanged.
        assert_eq!(
            delivered.get("summary"),
            Some(&"Q3 revenue grew 12%.".to_string())
        );
        assert_eq!(
            TaskInput::from_task_variables(&delivered).json("metrics"),
            Some(serde_json::json!({"growth": 0.12}))
        );
    }
}
