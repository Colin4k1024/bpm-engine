//! The flow runner: `run_flow` drives one reference flow end to end over
//! the engine's REST API with a real worker.
//!
//! [`run_flow_with`] (and its default-options alias [`run_flow`]):
//!
//! 1. spawns one [`Worker`](bpm_engine_worker_sdk::Worker) whose single
//!    [`ReferenceHandler`](crate::ReferenceHandler) dispatches every task
//!    type of the flow (`Worker::spawn` — E3-2),
//! 2. starts a process instance with the flow's start variables,
//! 3. drives the instance to a terminal state (or the timeout — a blocked
//!    flow, e.g. an unapproved side-effect tool failing permanently, is a
//!    legitimate outcome, not an error), submitting user-task decisions
//!    when their tokens arrive,
//! 4. shuts the worker down gracefully (`WorkerHandle::shutdown` — E3-2),
//! 5. snapshots the final state: instance state, current nodes, final
//!    variables and the `/steps` timeline into a [`FlowOutcome`].
//!
//! All HTTP goes to `server_url` (localhost in tests/examples). The model
//! calls happen inside the worker's handler against the injected
//! [`ModelProvider`](crate::ModelProvider) — the mock provider keeps the
//! whole path offline.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use bpm_engine_worker_sdk::{EngineClient, Worker, WorkerConfig};
use serde_json::Value;

use crate::flows::FlowSpec;
use crate::handler::{HandlerDeps, ReferenceHandler};
use crate::provider::{ModelProvider, ProviderConfig};

/// Default overall run timeout (how long the runner waits for a terminal
/// instance state before returning the current state as the outcome).
pub const DEFAULT_FLOW_TIMEOUT: Duration = Duration::from_secs(30);

/// Runner configuration (all values have offline-friendly defaults).
#[derive(Debug, Clone)]
pub struct FlowOptions {
    /// Worker identity used when locking tasks.
    pub worker_id: String,
    /// How long to wait for a terminal instance state before returning the
    /// current state (blocked flows — e.g. a fail-closed approval gate —
    /// surface as `RUNNING` outcomes, not errors).
    pub timeout: Duration,
    /// Instance-state polling interval of the runner loop.
    pub poll_interval: Duration,
    /// Worker poll interval (task fetch).
    pub worker_poll_interval: Duration,
    /// Lease duration the worker's fetch-and-lock requests
    /// (`WorkerConfig::lock_duration`). Short leases make lease-expiry
    /// redelivery observable quickly — the E6-3 regression eval
    /// ([`crate::eval`]) runs with 2 s; the default keeps the historical
    /// 30 s. It is also the lease the crash-injection hook sleeps past
    /// (see `Injection::CrashAndRedeliver` in `crate::eval`).
    pub lock_duration: Duration,
    /// Env var the reference tool's credential is injected from
    /// (`BPM_REFERENCE_LEDGER_KEY` by default; tests use unique names).
    pub tool_env_var: String,
    /// Identity and price table recorded in model-task envelopes
    /// (default: the mock provider's).
    pub pricing: ProviderConfig,
    /// Timeout applied around every provider call (E3-2 `with_timeout`).
    pub model_timeout: Duration,
    /// Grace period for the worker's graceful shutdown.
    pub shutdown_grace: Duration,
}

impl Default for FlowOptions {
    fn default() -> Self {
        Self {
            worker_id: "ai-reference-worker".to_string(),
            timeout: DEFAULT_FLOW_TIMEOUT,
            poll_interval: Duration::from_millis(100),
            worker_poll_interval: Duration::from_millis(50),
            lock_duration: Duration::from_secs(30),
            tool_env_var: "BPM_REFERENCE_LEDGER_KEY".to_string(),
            pricing: ProviderConfig::mock(),
            model_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

impl FlowOptions {
    /// Builder: override the overall run timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Builder: override the tool credential env var.
    pub fn tool_env_var(mut self, env_var: impl Into<String>) -> Self {
        self.tool_env_var = env_var.into();
        self
    }

    /// Builder: override the worker lease duration (short leases make
    /// lease-expiry redelivery observable quickly; see the field docs).
    pub fn lock_duration(mut self, lock_duration: Duration) -> Self {
        self.lock_duration = lock_duration;
        self
    }

    /// Builder: override the envelope pricing/model identity.
    pub fn pricing(mut self, pricing: ProviderConfig) -> Self {
        self.pricing = pricing;
        self
    }
}

/// One step of the outcome's timeline (parsed from
/// `GET /api/v1/process-instances/:id/steps`): event types only — the step
/// trace never exposes variable values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepSummary {
    /// 1-based position in the causal step order.
    pub step_no: u64,
    /// The node this step executed.
    pub node_id: String,
    /// Step kind: `node`, `external_task` or `timer`.
    pub kind: String,
    /// The step's history event types, in causal order.
    pub event_types: Vec<String>,
    /// Recorded error messages of the step's worker attempts (the E3-2
    /// structured failures — e.g. `tool_requires_approval: ...`).
    pub attempt_errors: Vec<String>,
    /// The step's envelope summary (model / tools / cost / execution id)
    /// when the step recorded an E3-1 envelope.
    pub envelope: Option<Value>,
}

impl StepSummary {
    /// Whether the step's work actually resolved (external task completed,
    /// user task completed, or the token left the node).
    pub fn executed(&self) -> bool {
        self.event_types.iter().any(|event| {
            matches!(
                event.as_str(),
                "ExternalTaskCompleted" | "UserTaskCompleted" | "TokenCompleted" | "TimerFired"
            )
        })
    }

    /// Whether any worker attempt on this step failed (with the structured
    /// error recorded).
    pub fn failed(&self) -> bool {
        !self.attempt_errors.is_empty()
            || self
                .event_types
                .iter()
                .any(|event| event == "ExternalTaskFailed")
    }
}

/// The outcome of one flow run: final instance state, variables and step
/// timeline.
#[derive(Debug, Clone)]
pub struct FlowOutcome {
    /// The flow's display name.
    pub flow_name: String,
    /// The instance that ran.
    pub instance_id: String,
    /// Terminal (or timeout) instance state: `COMPLETED` | `RUNNING` |
    /// `TERMINATED`.
    pub state: String,
    /// Nodes with waiting tokens at outcome time (empty once completed).
    pub current_nodes: Vec<String>,
    /// Final instance variables (raw storage strings — plain legacy strings
    /// plus `__var:1:` envelope values).
    pub variables: HashMap<String, String>,
    /// The step timeline.
    pub steps: Vec<StepSummary>,
}

impl FlowOutcome {
    /// Whether the instance completed.
    pub fn is_completed(&self) -> bool {
        self.state == "COMPLETED"
    }

    /// A final variable's raw storage string (for plain variables this IS
    /// the value; envelope-bearing variables need [`envelope`]).
    ///
    /// [`envelope`]: Self::envelope
    pub fn variable(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(String::as_str)
    }

    /// The instance's E3-1 task envelope (the LAST task's), when present:
    /// `None` — no envelope; `Some(Err(..))` — the reserved key holds a
    /// value that does not parse.
    pub fn envelope(
        &self,
    ) -> Option<Result<bpm_engine_core::AiTaskEnvelope, bpm_engine_core::AiTaskEnvelopeError>> {
        bpm_engine_core::AiTaskEnvelope::from_variables(&self.variables)
    }

    /// The step of `node_id`, if the node executed.
    pub fn step(&self, node_id: &str) -> Option<&StepSummary> {
        self.steps.iter().find(|s| s.node_id == node_id)
    }

    /// Whether a step exists for `node_id` (the node executed).
    pub fn has_step(&self, node_id: &str) -> bool {
        self.step(node_id).is_some()
    }

    /// Test-only constructor (deterministic, no server).
    #[cfg(test)]
    pub(crate) fn for_tests(instance_id: &str, state: &str) -> Self {
        Self {
            flow_name: "test".to_string(),
            instance_id: instance_id.to_string(),
            state: state.to_string(),
            current_nodes: Vec::new(),
            variables: HashMap::new(),
            steps: Vec::new(),
        }
    }
}

impl StepSummary {
    /// Test-only constructor (deterministic, no server).
    #[cfg(test)]
    pub(crate) fn for_tests(
        step_no: u64,
        node_id: &str,
        kind: &str,
        event_types: Vec<String>,
    ) -> Self {
        Self {
            step_no,
            node_id: node_id.to_string(),
            kind: kind.to_string(),
            event_types,
            attempt_errors: Vec::new(),
            envelope: None,
        }
    }
}

/// Run `flow` against the engine at `server_url` with default options.
///
/// The flow's definition must already be deployed (see
/// [`deploy_flow`](crate::deploy_flow)). See [`run_flow_with`] for the
/// tunable variant.
pub async fn run_flow(
    server_url: &str,
    flow: FlowSpec,
    provider: Arc<dyn ModelProvider>,
) -> anyhow::Result<FlowOutcome> {
    run_flow_with(server_url, flow, provider, FlowOptions::default()).await
}

/// Run `flow` against the engine at `server_url` with explicit options.
pub async fn run_flow_with(
    server_url: &str,
    flow: FlowSpec,
    provider: Arc<dyn ModelProvider>,
    options: FlowOptions,
) -> anyhow::Result<FlowOutcome> {
    let rest = Rest::new(server_url);
    let client = EngineClient::new(server_url);

    // 1. One dispatching handler, registered once per task type so the
    //    worker fetches every type the flow needs.
    let deps = Arc::new(HandlerDeps::new(provider, &options));
    let worker_config = WorkerConfig::new(options.worker_id.clone())
        .poll_interval(options.worker_poll_interval)
        .lock_duration(options.lock_duration)
        .fetch_retry_max(0)
        .shutdown_grace(options.shutdown_grace);
    let mut builder = Worker::builder().client(client).config(worker_config);
    for task_type in &flow.task_types {
        builder = builder.handler(ReferenceHandler::new(task_type.clone(), Arc::clone(&deps)));
    }
    let worker = builder.build();
    let handle = worker.spawn();

    // 2-3. Drive the instance; a drive failure still shuts the worker down.
    let driven = drive(&rest, &flow, &options).await;
    let drained = handle.shutdown(options.shutdown_grace).await;
    let instance_id = driven?;
    if !drained {
        // The flow still produced an outcome; surface the drain problem.
        tracing::warn!(
            flow = %flow.name,
            "worker did not drain cleanly within the grace (stragglers aborted)"
        );
    }

    // 5. Snapshot the outcome.
    let outcome = build_outcome(&rest, &flow.name, &instance_id).await?;
    Ok(outcome)
}

/// Start the instance and poll until it reaches a terminal state (or the
/// timeout — blocked flows are outcomes, not errors).
async fn drive(rest: &Rest, flow: &FlowSpec, options: &FlowOptions) -> anyhow::Result<String> {
    let instance_id = rest
        .start_instance(&flow.process_def_id, &flow.start_variables)
        .await
        .with_context(|| format!("starting flow '{}'", flow.name))?;
    let deadline = Instant::now() + options.timeout;
    let mut submitted: HashSet<String> = HashSet::new();

    loop {
        let instance = rest
            .get_instance(&instance_id)
            .await
            .with_context(|| format!("polling instance {instance_id}"))?;
        let state = instance
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state != "RUNNING" {
            return Ok(instance_id);
        }

        // Submit user-task decisions once their tokens arrive: the task id
        // is "<instance_id>:<node_id>" and the token is waiting when the
        // node appears in current_nodes.
        let current_nodes = string_array(instance.get("current_nodes"));
        for decision in &flow.user_decisions {
            if submitted.contains(&decision.node_id) {
                continue;
            }
            if current_nodes.iter().any(|n| n == &decision.node_id) {
                rest.complete_user_task(&instance_id, &decision.node_id, &decision.variables)
                    .await
                    .with_context(|| {
                        format!(
                            "completing user task '{}' of flow '{}'",
                            decision.node_id, flow.name
                        )
                    })?;
                submitted.insert(decision.node_id.clone());
            }
        }

        if Instant::now() >= deadline {
            // Timeout: return the still-running instance as the outcome.
            return Ok(instance_id);
        }
        tokio::time::sleep(options.poll_interval).await;
    }
}

/// Snapshot the final state, variables and step timeline.
pub(crate) async fn build_outcome(
    rest: &Rest,
    flow_name: &str,
    instance_id: &str,
) -> anyhow::Result<FlowOutcome> {
    let instance = rest
        .get_instance(instance_id)
        .await
        .with_context(|| format!("final GET of instance {instance_id}"))?;
    let steps_json = rest
        .get_steps(instance_id)
        .await
        .with_context(|| format!("GET /steps of instance {instance_id}"))?;

    let variables = instance
        .get("variables")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();

    let steps = steps_json
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| steps.iter().filter_map(parse_step).collect())
        .unwrap_or_default();

    Ok(FlowOutcome {
        flow_name: flow_name.to_string(),
        instance_id: instance_id.to_string(),
        state: instance
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        current_nodes: string_array(instance.get("current_nodes")),
        variables,
        steps,
    })
}

/// Parse one `/steps` entry into a [`StepSummary`].
fn parse_step(step: &Value) -> Option<StepSummary> {
    let node_id = step.get("node_id")?.as_str()?.to_string();
    Some(StepSummary {
        step_no: step.get("step_no").and_then(Value::as_u64).unwrap_or(0),
        node_id,
        kind: step
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("node")
            .to_string(),
        event_types: step
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|e| {
                        e.get("event_type")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        attempt_errors: step
            .get("attempts")
            .and_then(Value::as_array)
            .map(|attempts| {
                attempts
                    .iter()
                    .filter_map(|a| a.get("error").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        envelope: step
            .get("envelope_summary")
            .cloned()
            .filter(Value::is_object),
    })
}

/// `["a", "b"]` -> `["a", "b"]` (missing or non-array -> empty).
fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Minimal engine-REST client for the runner's instance lifecycle calls
/// (task calls go through the SDK's [`EngineClient`]).
///
/// Crate-visible: the E6-3 regression eval
/// (`crate::eval`) reuses the same plumbing for its case driver (start
/// instance, poll, submit user-task decisions, snapshot the outcome) plus
/// the dead-letter check that lets blocked cases stop waiting early.
pub(crate) struct Rest {
    http: reqwest::Client,
    base: String,
}

impl Rest {
    pub(crate) fn new(server_url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: server_url.trim_end_matches('/').to_string(),
        }
    }

    pub(crate) async fn start_instance(
        &self,
        process_def_id: &str,
        variables: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        #[derive(serde::Serialize)]
        struct StartRequest<'a> {
            process_def_id: &'a str,
            #[serde(skip_serializing_if = "HashMap::is_empty")]
            variables: &'a HashMap<String, String>,
        }
        let url = format!("{}/api/v1/process-instances", self.base);
        let response = self
            .http
            .post(&url)
            .json(&StartRequest {
                process_def_id,
                variables,
            })
            .send()
            .await
            .map_err(|e| anyhow!("POST {url} failed: {e}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| anyhow!("unparseable start-instance response: {e}"))?;
        if !status.is_success() {
            return Err(anyhow!("start instance returned HTTP {status}: {body}"));
        }
        body.get("instance_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("start-instance response missing instance_id: {body}"))
    }

    pub(crate) async fn get_instance(&self, instance_id: &str) -> anyhow::Result<Value> {
        let url = format!("{}/api/v1/process-instances/{instance_id}", self.base);
        self.get_json(&url).await
    }

    pub(crate) async fn get_steps(&self, instance_id: &str) -> anyhow::Result<Value> {
        let url = format!("{}/api/v1/process-instances/{instance_id}/steps", self.base);
        self.get_json(&url).await
    }

    /// Whether a dead-letter entry exists for `instance_id` (E6-3): in this
    /// engine a DLQ'd task means the instance is terminally blocked — its
    /// token stays at the failed node — so the eval driver treats a DLQ
    /// hit as the case's terminal outcome instead of waiting for the run
    /// timeout.
    pub(crate) async fn has_dead_letter(&self, instance_id: &str) -> anyhow::Result<bool> {
        let url = format!("{}/api/v1/dead-letters", self.base);
        let body = self.get_json(&url).await?;
        let hit = body
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries.iter().any(|entry| {
                    entry.get("process_instance_id").and_then(Value::as_str) == Some(instance_id)
                })
            })
            .unwrap_or(false);
        Ok(hit)
    }

    pub(crate) async fn complete_user_task(
        &self,
        instance_id: &str,
        node_id: &str,
        variables: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct CompleteRequest<'a> {
            variables: &'a HashMap<String, String>,
        }
        // User task ids are "<instance_id>:<node_id>".
        let url = format!(
            "{}/api/v1/tasks/{instance_id}:{node_id}/complete",
            self.base
        );
        let response = self
            .http
            .post(&url)
            .json(&CompleteRequest { variables })
            .send()
            .await
            .map_err(|e| anyhow!("POST {url} failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "completing user task '{node_id}' returned HTTP {status}: {body}"
            ));
        }
        Ok(())
    }

    pub(crate) async fn get_json(&self, url: &str) -> anyhow::Result<Value> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("GET {url} failed: {e}"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| anyhow!("unparseable response from {url}: {e}"))?;
        if !status.is_success() {
            return Err(anyhow!("GET {url} returned HTTP {status}: {body}"));
        }
        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_options_defaults_are_offline_friendly() {
        let options = FlowOptions::default();
        assert_eq!(options.worker_id, "ai-reference-worker");
        assert_eq!(options.timeout, DEFAULT_FLOW_TIMEOUT);
        assert_eq!(options.tool_env_var, "BPM_REFERENCE_LEDGER_KEY");
        assert_eq!(options.pricing, ProviderConfig::mock());
        // The historical 30 s lease is the default; the eval profile
        // overrides it to 2 s.
        assert_eq!(options.lock_duration, Duration::from_secs(30));
        // Builders.
        let options = FlowOptions::default()
            .timeout(Duration::from_secs(3))
            .tool_env_var("OTHER")
            .lock_duration(Duration::from_secs(1))
            .pricing(
                ProviderConfig::mock().with_model(bpm_engine_core::ModelRef {
                    provider: "p".to_string(),
                    name: "m".to_string(),
                    version: None,
                }),
            );
        assert_eq!(options.timeout, Duration::from_secs(3));
        assert_eq!(options.tool_env_var, "OTHER");
        assert_eq!(options.lock_duration, Duration::from_secs(1));
        assert_eq!(options.pricing.model.name, "m");
    }

    #[test]
    fn parse_step_extracts_summary_fields() {
        let json = serde_json::json!({
            "step_no": 2,
            "node_id": "summarize",
            "kind": "external_task",
            "token_id": "tok-9",
            "events": [
                { "event_type": "TokenArrived", "occurred_at": "100" },
                { "event_type": "ExternalTaskLocked", "occurred_at": "100" },
                { "event_type": "ExternalTaskCompleted", "occurred_at": "101" }
            ],
            "attempts": [
                {
                    "worker_id": "w",
                    "outcome": "completed",
                    "locked_at": "100",
                    "completed_or_failed": "101",
                    "error": null
                }
            ],
            "envelope_summary": {
                "model": { "provider": "mock", "name": "mock-model-v1" },
                "cost": { "amount_micros": 18 }
            },
            "variables_touched": ["summary", "__task_envelope"]
        });
        let step = parse_step(&json).expect("parses");
        assert_eq!(step.step_no, 2);
        assert_eq!(step.node_id, "summarize");
        assert_eq!(step.kind, "external_task");
        assert_eq!(
            step.event_types,
            [
                "TokenArrived",
                "ExternalTaskLocked",
                "ExternalTaskCompleted"
            ]
        );
        assert!(step.attempt_errors.is_empty());
        assert!(step.executed());
        assert!(!step.failed());
        assert_eq!(
            step.envelope.as_ref().unwrap()["model"]["name"],
            "mock-model-v1"
        );

        // A failed attempt records its error and marks the step failed.
        let json = serde_json::json!({
            "step_no": 3,
            "node_id": "act",
            "kind": "external_task",
            "events": [
                { "event_type": "TokenArrived", "occurred_at": "100" },
                { "event_type": "ExternalTaskFailed", "occurred_at": "101" }
            ],
            "attempts": [
                {
                    "worker_id": "w",
                    "outcome": "failed",
                    "error": "tool_requires_approval: ..."
                }
            ]
        });
        let step = parse_step(&json).expect("parses");
        assert!(!step.executed());
        assert!(step.failed());
        assert_eq!(step.attempt_errors, ["tool_requires_approval: ..."]);
        assert!(step.envelope.is_none());
    }

    #[test]
    fn string_array_tolerates_missing_fields() {
        assert!(string_array(None).is_empty());
        assert!(string_array(Some(&serde_json::json!("RUNNING"))).is_empty());
        assert_eq!(
            string_array(Some(&serde_json::json!(["a", "b"]))),
            ["a", "b"]
        );
    }

    #[test]
    fn outcome_helpers() {
        let mut outcome = FlowOutcome::for_tests("inst-1", "COMPLETED");
        outcome
            .variables
            .insert("summary".to_string(), "s".to_string());
        outcome.steps.push(StepSummary::for_tests(
            1,
            "summarize",
            "external_task",
            vec!["ExternalTaskCompleted".to_string()],
        ));
        assert!(outcome.is_completed());
        assert_eq!(outcome.variable("summary"), Some("s"));
        assert!(outcome.step("summarize").is_some());
        assert!(outcome.has_step("summarize"));
        assert!(outcome.envelope().is_none());
        assert!(outcome.step("missing").is_none());
    }
}
