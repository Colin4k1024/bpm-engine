//! TokenArrivedHandler: TokenArrived -> advance token, emit TokenArrived/ProcessCompleted.
//! Handles ParallelFork / ParallelJoin.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, InstanceState, NodeType, TokenStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::{debug, info_span, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::{
    evaluate_exclusive_gateway, move_token, move_token_preserving_group, move_token_with_group,
};

/// Main handler for TokenArrived events — executes node logic and advances tokens.
pub struct TokenArrivedHandler {
    join_state: Mutex<HashMap<String, (usize, HashSet<String>)>>,
}

impl TokenArrivedHandler {
    /// Create a new handler with empty join state.
    pub fn new() -> Self {
        TokenArrivedHandler {
            join_state: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for TokenArrivedHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for TokenArrivedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenArrived(e) = event else {
            return vec![];
        };
        let span = info_span!(
            "token.transition",
            instance_id = %e.instance_id,
            token_id = %e.token_id,
            node_id = %e.node_id,
        );
        let _guard = span.enter();
        let process_store = &ctx.process_store;
        drop(_guard);
        let process_def_store = &ctx.process_def_store;
        let Ok(Some(mut instance)) = process_store.load(&e.instance_id).await else {
            return vec![];
        };
        let Ok(Some(def)) = process_def_store.load(&instance.process_def_id).await else {
            return vec![];
        };
        let Some(token_idx) = instance.tokens.iter().position(|t| t.id == e.token_id) else {
            return vec![];
        };
        // Extract group_id early to avoid borrow conflicts
        let parallel_group_id = instance.tokens[token_idx].parallel_group_id.clone();
        let token_store = &ctx.token_store;
        if !token_store
            .claim_token(
                &e.instance_id,
                &e.token_id,
                instance.tokens[token_idx].version,
            )
            .await
            .unwrap_or(false)
        {
            warn!(instance_id = %e.instance_id, token_id = %e.token_id, "token claim failed (CAS)");
            return vec![];
        }
        instance.tokens[token_idx].status = TokenStatus::Executing;
        instance.tokens[token_idx].version += 1;
        debug!(instance_id = %e.instance_id, token_id = %e.token_id, node_id = %e.node_id, "token arrived");
        let node = match def.nodes.get(e.node_id.as_str()) {
            Some(n) => n,
            None => return vec![],
        };

        let mut out = vec![];

        match &node.node_type {
            NodeType::Start => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::ServiceTask(service) => {
                service(&mut instance);
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token_preserving_group(node, parallel_group_id.clone());
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::UserTask { .. } => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                // Schedule boundary event timers for this host node
                if let Some(boundary_defs) = def.boundary_events.get(e.node_id.as_str()) {
                    for bdef in boundary_defs {
                        if let Some(bnode) = def.nodes.get(bdef.node_id) {
                            if let NodeType::BoundaryTimer { duration, .. } = &bnode.node_type {
                                if let Some(ref timer_store) = ctx.timer_store {
                                    let fire_at = parse_duration_to_epoch(duration);
                                    let timer_id = uuid::Uuid::new_v4().to_string();
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        .to_string();
                                    let record = bpm_engine_storage::TimerRecord {
                                        id: timer_id,
                                        token_id: e.token_id.clone(),
                                        instance_id: e.instance_id.clone(),
                                        node_id: bdef.node_id.to_string(),
                                        due_at: fire_at.to_string(),
                                        status: "Scheduled".to_string(),
                                        created_at: now,
                                    };
                                    let _ = timer_store.insert(&record).await;
                                }
                            }
                        }
                    }
                }
            }
            NodeType::ExternalTask {
                task_type,
                retries,
                timeout_secs,
            } => {
                // E5-2 approval gate (ADR-008): an approval-required step
                // (task_type in the configured set, or a record already
                // exists for this subject) creates the external task only
                // AFTER an operator approves — task-creation-after-approval.
                // Until then the token WAITS (like a user task) and NO task
                // row exists, so workers never see unapproved work.
                let mut engine_approved = false;
                if let Some(ref approval_store) = ctx.approval_store {
                    let required = ctx.approval_required_task_types.contains(task_type);
                    let existing = approval_store
                        .list_by_instance(&e.instance_id)
                        .await
                        .ok()
                        .and_then(|records| {
                            records
                                .into_iter()
                                .find(|r| r.node_id == e.node_id && r.token_id == e.token_id)
                        });
                    if required || existing.is_some() {
                        match existing.map(|r| r.status) {
                            None => {
                                // First arrival at an approval-required node:
                                // persist the Pending record and wait for the
                                // operator decision (the decide endpoint
                                // creates the task on approve).
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs()
                                    .to_string();
                                let approval_id = uuid::Uuid::new_v4().to_string();
                                let _ = approval_store
                                    .create(&bpm_engine_core::ApprovalRecord {
                                        id: approval_id.clone(),
                                        instance_id: e.instance_id.clone(),
                                        node_id: e.node_id.clone(),
                                        token_id: e.token_id.clone(),
                                        task_type: task_type.clone(),
                                        status: bpm_engine_core::ApprovalStatus::Pending,
                                        effect: bpm_engine_core::ApprovalEffect::Pending,
                                        decided_by: None,
                                        decided_at: None,
                                        note: None,
                                        created_at: now.clone(),
                                    })
                                    .await;
                                if let Some(ref history_repo) = ctx.history_repo {
                                    let payload = serde_json::json!({
                                        "approval_id": approval_id,
                                        "instance_id": e.instance_id,
                                        "node_id": e.node_id,
                                        "token_id": e.token_id,
                                        "task_type": task_type,
                                    });
                                    let _ = history_repo
                                        .append(&e.instance_id, "ApprovalCreated", &payload, &now)
                                        .await;
                                }
                                instance.tokens[token_idx].status = TokenStatus::Waiting;
                                let _ = process_store.save(&instance).await;
                                return vec![];
                            }
                            Some(bpm_engine_core::ApprovalStatus::Pending) => {
                                // Re-arrival while the decision is still
                                // pending: keep waiting (the record is the
                                // recovery state — no duplicate requirement).
                                instance.tokens[token_idx].status = TokenStatus::Waiting;
                                let _ = process_store.save(&instance).await;
                                return vec![];
                            }
                            Some(bpm_engine_core::ApprovalStatus::Rejected) => {
                                // A rejected subject never creates its task;
                                // the decide endpoint already failed the
                                // token (TokenFailed + DLQ).
                                instance.tokens[token_idx].status = TokenStatus::Waiting;
                                let _ = process_store.save(&instance).await;
                                return vec![];
                            }
                            Some(bpm_engine_core::ApprovalStatus::Approved) => {
                                // Post-approval (re-)arrival — e.g. event
                                // re-delivery: create the task now, with the
                                // ENGINE-written `__approval` truth.
                                engine_approved = true;
                            }
                        }
                    }
                }
                if let Some(ref ext_store) = ctx.external_task_store {
                    let mut variables = instance.variables.clone();
                    if engine_approved {
                        // E5-2: the engine injects the reserved approval
                        // variable — the worker-side E3-3 gate then reads
                        // engine-written truth (engine truth wins over any
                        // client-supplied value of the same name).
                        variables.insert(
                            bpm_engine_core::APPROVAL_VARIABLE.to_string(),
                            "true".to_string(),
                        );
                    }
                    let _ = ext_store
                        .create(
                            &e.token_id,
                            &e.instance_id,
                            task_type,
                            *retries,
                            *timeout_secs,
                            variables,
                        )
                        .await;
                }
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                // Schedule boundary event timers for this host node
                if let Some(boundary_defs) = def.boundary_events.get(e.node_id.as_str()) {
                    for bdef in boundary_defs {
                        if let Some(bnode) = def.nodes.get(bdef.node_id) {
                            if let NodeType::BoundaryTimer { duration, .. } = &bnode.node_type {
                                if let Some(ref timer_store) = ctx.timer_store {
                                    let fire_at = parse_duration_to_epoch(duration);
                                    let timer_id = uuid::Uuid::new_v4().to_string();
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        .to_string();
                                    let record = bpm_engine_storage::TimerRecord {
                                        id: timer_id,
                                        token_id: e.token_id.clone(),
                                        instance_id: e.instance_id.clone(),
                                        node_id: bdef.node_id.to_string(),
                                        due_at: fire_at.to_string(),
                                        status: "Scheduled".to_string(),
                                        created_at: now,
                                    };
                                    let _ = timer_store.insert(&record).await;
                                }
                            }
                        }
                    }
                }
            }
            NodeType::ExclusiveGateway => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                if let Some(t) = evaluate_exclusive_gateway(node, &instance.variables) {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                    instance.tokens.push(t);
                }
            }
            NodeType::End => {
                instance.state = InstanceState::Completed;
                instance.tokens.remove(token_idx);
                let _ = process_store.save(&instance).await;
                return vec![EngineEvent::ProcessCompleted(payloads::ProcessCompleted {
                    instance_id: e.instance_id.clone(),
                })];
            }
            NodeType::ParallelFork => {
                let group_id = uuid::Uuid::new_v4().to_string();
                if let Some(ref join_repo) = ctx.parallel_join_repo {
                    let expected = node.outgoing_edges.len() as u32;
                    let _ = join_repo.ensure_group(&group_id, expected).await;
                }
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token_with_group(node, group_id.clone());
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::ParallelJoin { expected } => {
                let group_id = instance.tokens[token_idx]
                    .parallel_group_id
                    .clone()
                    .unwrap_or_default();
                let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
                    join_repo.try_join(&group_id).await.unwrap_or(false)
                } else {
                    let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
                    let mut state = self.join_state.lock().unwrap();
                    let (exp, arrived) = state
                        .entry(key.clone())
                        .or_insert((*expected, HashSet::new()));
                    arrived.insert(e.token_id.clone());
                    let done = arrived.len() >= *exp;
                    if done {
                        state.remove(&key);
                    }
                    done
                };

                if done {
                    instance.tokens.retain(|t| {
                        !(t.node_id == e.node_id
                            && t.parallel_group_id.as_deref() == Some(group_id.as_str()))
                    });
                    let new_tokens = move_token(node);
                    for t in &new_tokens {
                        out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: e.instance_id.clone(),
                            token_id: t.id.clone(),
                            node_id: t.node_id.clone(),
                        }));
                    }
                    instance.tokens.extend(new_tokens);
                } else {
                    instance.tokens[token_idx].status = TokenStatus::Waiting;
                }
            }
            NodeType::TimerIntermediateCatch { timer_definition } => {
                // Schedule the persistent timer for this catch event: the
                // token waits until the background scheduler (ADR-004) picks
                // up the record from TimerStore::list_due and emits
                // TimerFired. Without the record the token would wait forever.
                if let Some(ref timer_store) = ctx.timer_store {
                    let fire_at = parse_duration_to_epoch(timer_definition);
                    let timer_id = uuid::Uuid::new_v4().to_string();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .to_string();
                    let record = bpm_engine_storage::TimerRecord {
                        id: timer_id,
                        token_id: e.token_id.clone(),
                        instance_id: e.instance_id.clone(),
                        node_id: e.node_id.to_string(),
                        due_at: fire_at.to_string(),
                        status: "Scheduled".to_string(),
                        created_at: now,
                    };
                    let _ = timer_store.insert(&record).await;
                }
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::BoundaryTimer { .. } | NodeType::BoundaryError { .. } => {
                // Boundary events are not directly activated by token arrival;
                // they are handled by the host activity's event processing.
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::CallActivity { called_process_key } => {
                // Look up the called process definition by key
                let active = process_def_store
                    .get_active(called_process_key)
                    .await
                    .ok()
                    .flatten();
                if active.is_none() {
                    warn!(
                        instance_id = %e.instance_id,
                        called_key = %called_process_key,
                        "callActivity: no active process definition found"
                    );
                    instance.tokens[token_idx].status = TokenStatus::Waiting;
                    let _ = process_store.save(&instance).await;
                    return vec![EngineEvent::TokenFailed(payloads::TokenFailed {
                        instance_id: e.instance_id.clone(),
                        token_id: e.token_id.clone(),
                        node_id: e.node_id.clone(),
                        reason: format!(
                            "CallActivity: no active process definition for key '{}'",
                            called_process_key
                        ),
                    })];
                }
                let child_instance_id = uuid::Uuid::new_v4().to_string();
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let _ = process_store.save(&instance).await;
                out.push(EngineEvent::ProcessStarted(payloads::ProcessStarted {
                    process_id: active.unwrap().id.clone(),
                    instance_id: child_instance_id.clone(),
                    initial_variables: Some(instance.variables.clone()),
                }));
                out.push(EngineEvent::CallActivityStarted(
                    payloads::CallActivityStarted {
                        parent_instance_id: e.instance_id.clone(),
                        parent_token_id: e.token_id.clone(),
                        child_instance_id,
                        child_process_key: called_process_key.clone(),
                    },
                ));
            }
            NodeType::MessageIntermediateCatch { .. }
            | NodeType::SignalIntermediateCatch { .. } => {
                // Wait for the message/signal to arrive; token stays in Waiting.
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::MessageIntermediateThrow { message_name } => {
                // Send the message and continue.
                out.push(EngineEvent::MessageSent(payloads::MessageSent {
                    instance_id: e.instance_id.clone(),
                    message_name: message_name.clone(),
                }));
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::SignalIntermediateThrow { signal_name } => {
                // Fire the signal and continue.
                out.push(EngineEvent::SignalSent(payloads::SignalSent {
                    instance_id: e.instance_id.clone(),
                    signal_name: signal_name.clone(),
                }));
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::TerminateEnd => {
                // Terminate all active tokens in the instance.
                instance.state = InstanceState::Terminated;
                instance.tokens.clear();
                let _ = process_store.save(&instance).await;
                return vec![EngineEvent::ProcessTerminated(
                    payloads::ProcessTerminated {
                        instance_id: e.instance_id.clone(),
                    },
                )];
            }
        }

        let _ = process_store.save(&instance).await;
        out
    }
}

/// Parse an ISO 8601 duration string (e.g. "PT30S", "PT1H", "PT2M") to a unix epoch timestamp.
///
/// Returns the absolute time (in seconds since epoch) when the timer should fire.
/// Falls back to 60 seconds from now if parsing fails.
fn parse_duration_to_epoch(duration: &str) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let secs = parse_iso8601_duration_secs(duration).unwrap_or(60);
    now + secs
}

/// Parse ISO 8601 duration to seconds. Supports PT{N}S, PT{N}M, PT{N}H.
fn parse_iso8601_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim().strip_prefix('P')?.strip_prefix('T')?;
    let mut total = 0u64;
    let mut num = 0u64;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num = num * 10 + c.to_digit(10)? as u64;
        } else {
            match c {
                'S' => total += num,
                'M' => total += num * 60,
                'H' => total += num * 3600,
                _ => return None,
            }
            num = 0;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    //! E5-2 approval-gate unit tests (ADR-008): the ExternalTask arrival
    //! branch must (1) create a Pending ApprovalRecord and create NO task
    //! row while pending, (2) create the task with the engine-written
    //! `__approval=true` once the record is Approved, and (3) never create
    //! the task for a Rejected subject.

    use super::*;
    use bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
    use bpm_engine_core::payloads;
    use bpm_engine_storage::{ApprovalStore, ExternalTaskStore, HistoryRepo, ProcessInstanceStore};
    use std::collections::HashSet;
    use std::sync::Arc;

    fn approval_flow_def() -> bpm_engine_core::ProcessDefinition {
        use bpm_engine_core::{Node, NodeType, OutgoingEdge};
        bpm_engine_core::ProcessDefinition {
            id: "approval-flow",
            start: "start",
            boundary_events: HashMap::new(),
            nodes: HashMap::from([
                (
                    "start",
                    Node {
                        id: "start",
                        node_type: NodeType::Start,
                        outgoing_edges: vec![OutgoingEdge {
                            target: "charge",
                            condition: None,
                        }],
                    },
                ),
                (
                    "charge",
                    Node {
                        id: "charge",
                        node_type: NodeType::ExternalTask {
                            task_type: "payment_charge".into(),
                            retries: 3,
                            timeout_secs: 60,
                        },
                        outgoing_edges: vec![OutgoingEdge {
                            target: "end",
                            condition: None,
                        }],
                    },
                ),
                (
                    "end",
                    Node {
                        id: "end",
                        node_type: NodeType::End,
                        outgoing_edges: vec![],
                    },
                ),
            ]),
        }
    }

    struct TestSetup {
        ctx: EngineContext,
        repo: Arc<MemoryRepo>,
        instance_id: String,
        token_id: String,
    }

    /// Build a context whose engine arrival gate is armed for
    /// "payment_charge" with a token ALREADY Waiting at the charge node
    /// (claimed through one prior arrival).
    async fn setup() -> TestSetup {
        let repo = Arc::new(MemoryRepo::new());
        let def_store = Arc::new(ProcessDefStore::new());
        def_store.register(approval_flow_def());
        let instance = bpm_engine_core::ProcessInstance {
            id: "inst-1".into(),
            process_def_id: "approval-flow".to_string(),
            tenant_id: None,
            tokens: vec![bpm_engine_core::Token {
                id: "tok-charge".into(),
                node_id: "charge".to_string(),
                status: TokenStatus::Ready,
                mode: bpm_engine_core::TokenMode::Forward,
                version: 0,
                attempt: 0,
                parallel_group_id: None,
                updated_at: None,
            }],
            variables: HashMap::from([("amount".to_string(), "100".to_string())]),
            state: bpm_engine_core::InstanceState::Running,
            version: 1,
            parent_instance_id: None,
            parent_token_id: None,
        };
        repo.save(&instance).await.unwrap();
        let ctx = EngineContext::builder(
            repo.clone() as Arc<dyn bpm_engine_storage::ProcessInstanceStore>,
            repo.clone() as Arc<dyn bpm_engine_storage::TokenStore>,
            def_store as Arc<dyn bpm_engine_storage::ProcessDefinitionStore>,
        )
        .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
        .approval_store(repo.clone() as Arc<dyn ApprovalStore>)
        .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
        .approval_required_task_types(Arc::new(HashSet::from(["payment_charge".to_string()])))
        .build();
        TestSetup {
            ctx,
            repo,
            instance_id: "inst-1".into(),
            token_id: "tok-charge".into(),
        }
    }

    fn arrived(setup: &TestSetup) -> EngineEvent {
        EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: setup.instance_id.clone(),
            token_id: setup.token_id.clone(),
            node_id: "charge".to_string(),
        })
    }

    async fn refetch_token(setup: &TestSetup) -> bpm_engine_core::Token {
        let inst = setup.repo.load(&setup.instance_id).await.unwrap().unwrap();
        inst.tokens
            .into_iter()
            .find(|t| t.id == setup.token_id)
            .unwrap()
    }

    #[tokio::test]
    async fn approval_required_arrival_creates_pending_record_and_no_task() {
        let mut setup = setup().await;
        TokenArrivedHandler::new()
            .handle(&arrived(&setup), &mut setup.ctx)
            .await;

        // A Pending record exists for the subject...
        let records = ApprovalStore::list_by_instance(&*setup.repo, "inst-1")
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, bpm_engine_core::ApprovalStatus::Pending);
        assert_eq!(records[0].node_id, "charge");
        assert_eq!(records[0].token_id, "tok-charge");

        // ...and NO external task was created (nothing fetchable).
        let tasks = ExternalTaskStore::fetch_and_lock(
            &*setup.repo,
            "w",
            &["payment_charge".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(tasks.is_empty(), "unapproved work must not be fetchable");

        // The token waits (like a user task) and the history explains why.
        assert_eq!(refetch_token(&setup).await.status, TokenStatus::Waiting);
        let history = HistoryRepo::list_by_instance(&*setup.repo, "inst-1", None, None)
            .await
            .unwrap();
        assert!(history.iter().any(|h| h.event_type == "ApprovalCreated"));
    }

    #[tokio::test]
    async fn approved_arrival_creates_task_with_engine_written_approval() {
        // Pre-approve the record (as the decide endpoint would), then let the
        // token (re-)arrive: the task is created fetchable and carries
        // `__approval=true` written by the ENGINE (overriding a client-set
        // instance variable of the same name — engine truth wins).
        let mut setup = setup().await;
        let mut inst = setup.repo.load(&setup.instance_id).await.unwrap().unwrap();
        inst.variables
            .insert("__approval".to_string(), "false".to_string());
        setup.repo.save(&inst).await.unwrap();
        let record = bpm_engine_core::ApprovalRecord {
            id: "appr-1".into(),
            instance_id: "inst-1".into(),
            node_id: "charge".to_string(),
            token_id: "tok-charge".into(),
            task_type: "payment_charge".into(),
            status: bpm_engine_core::ApprovalStatus::Approved,
            effect: bpm_engine_core::ApprovalEffect::Applied,
            decided_by: Some("alice".into()),
            decided_at: Some("1".into()),
            note: None,
            created_at: "1".into(),
        };
        ApprovalStore::create(&*setup.repo, &record).await.unwrap();

        // Flip the token back to Ready so the arrival can claim it.
        let mut inst = setup.repo.load(&setup.instance_id).await.unwrap().unwrap();
        inst.tokens[0].status = TokenStatus::Ready;
        inst.tokens[0].version = 0;
        setup.repo.save(&inst).await.unwrap();

        TokenArrivedHandler::new()
            .handle(&arrived(&setup), &mut setup.ctx)
            .await;

        let tasks = ExternalTaskStore::fetch_and_lock(
            &*setup.repo,
            "w",
            &["payment_charge".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].variables.get("__approval").map(String::as_str),
            Some("true"),
            "the engine must overwrite a forged __approval with its truth"
        );
        assert_eq!(
            tasks[0].variables.get("amount").map(String::as_str),
            Some("100")
        );
    }

    #[tokio::test]
    async fn rejected_subject_never_creates_a_task() {
        let mut setup = setup().await;
        let record = bpm_engine_core::ApprovalRecord {
            id: "appr-2".into(),
            instance_id: "inst-1".into(),
            node_id: "charge".to_string(),
            token_id: "tok-charge".into(),
            task_type: "payment_charge".into(),
            status: bpm_engine_core::ApprovalStatus::Rejected,
            effect: bpm_engine_core::ApprovalEffect::Applied,
            decided_by: Some("bob".into()),
            decided_at: Some("1".into()),
            note: None,
            created_at: "1".into(),
        };
        ApprovalStore::create(&*setup.repo, &record).await.unwrap();

        TokenArrivedHandler::new()
            .handle(&arrived(&setup), &mut setup.ctx)
            .await;

        let tasks = ExternalTaskStore::fetch_and_lock(
            &*setup.repo,
            "w",
            &["payment_charge".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(
            tasks.is_empty(),
            "a rejected subject must not create a task"
        );
    }

    #[tokio::test]
    async fn ungated_task_type_keeps_pre_e52_behavior() {
        // A task type NOT in the required set (and no record) creates its
        // task immediately — no record, no __approval injection.
        let mut setup = setup().await;
        setup.ctx.approval_required_task_types = Arc::new(HashSet::new());
        TokenArrivedHandler::new()
            .handle(&arrived(&setup), &mut setup.ctx)
            .await;

        let records = ApprovalStore::list_by_instance(&*setup.repo, "inst-1")
            .await
            .unwrap();
        assert!(records.is_empty());
        let tasks = ExternalTaskStore::fetch_and_lock(
            &*setup.repo,
            "w",
            &["payment_charge".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(!tasks[0].variables.contains_key("__approval"));
    }
}
