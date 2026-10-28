//! Unified in-memory store: ProcessInstance (with tokens), Outbox, Timer, ParallelJoin, Compensation, ExternalTask.
//! Implements all storage traits for runtime EngineContext.

use async_trait::async_trait;
use bpm_engine_core::{
    ApprovalDecision, ApprovalRecord, ApprovalStatus, ExternalTask, ExternalTaskState,
    InstanceState, ProcessInstance, Token, TokenStatus,
};
use bpm_engine_storage::{
    ApprovalDecideOutcome, ApprovalError, ApprovalStore, CompensationRecordRepo,
    CompensationRecordRow, ExternalTaskStore, HistoryEvent, HistoryRepo, OutboxEvent, OutboxRepo,
    ParallelJoinRepo, ProcessInstanceStore, TimerRecord, TimerStore, TokenStore,
};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn utc_now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[derive(Debug, Clone)]
struct ExternalTaskRow {
    task_id: String,
    token_id: String,
    process_instance_id: String,
    task_type: String,
    state: ExternalTaskState,
    lock_owner: Option<String>,
    lock_expire_at: Option<u64>,
    retries: i32,
    error_message: Option<String>,
    variables: HashMap<String, String>,
    created_at: String,
    updated_at: String,
}

impl ExternalTaskRow {
    fn to_external_task(&self) -> ExternalTask {
        ExternalTask {
            task_id: self.task_id.clone(),
            token_id: self.token_id.clone(),
            process_instance_id: self.process_instance_id.clone(),
            task_type: self.task_type.clone(),
            state: self.state,
            lock_owner: self.lock_owner.clone(),
            lock_expire_at: self.lock_expire_at.map(|t| t.to_string()),
            retries: self.retries,
            error_message: self.error_message.clone(),
            variables: self.variables.clone(),
            created_at: Some(self.created_at.clone()),
            updated_at: Some(self.updated_at.clone()),
        }
    }
}

#[derive(Clone)]
struct HistoryEventRow {
    id: String,
    instance_id: String,
    event_type: String,
    payload: String,
    occurred_at: String,
}

/// Unified in-memory repo: instances (with tokens inside), outbox, timers, parallel_join, compensation, external_tasks, history.
pub struct MemoryRepo {
    instances: RwLock<HashMap<String, ProcessInstance>>,
    outbox: RwLock<Vec<OutboxEvent>>,
    timers: RwLock<HashMap<String, TimerRecord>>,
    parallel_join: RwLock<HashMap<String, (u32, u32, bool)>>,
    compensation: RwLock<Vec<CompensationRecordRow>>,
    external_tasks: RwLock<HashMap<String, ExternalTaskRow>>,
    history_events: RwLock<Vec<HistoryEventRow>>,
    /// Engine-owned approval records (E5-2): id -> record.
    approvals: RwLock<HashMap<String, ApprovalRecord>>,
    /// Start-instance idempotency keys (E2-4): `(tenant_scope, key) ->
    /// instance_id`. The tenant scope is `""` for no tenant.
    idempotency_keys: RwLock<HashMap<(String, String), String>>,
}

impl MemoryRepo {
    pub fn new() -> Self {
        MemoryRepo {
            instances: RwLock::new(HashMap::new()),
            outbox: RwLock::new(Vec::new()),
            timers: RwLock::new(HashMap::new()),
            parallel_join: RwLock::new(HashMap::new()),
            compensation: RwLock::new(Vec::new()),
            external_tasks: RwLock::new(HashMap::new()),
            history_events: RwLock::new(Vec::new()),
            approvals: RwLock::new(HashMap::new()),
            idempotency_keys: RwLock::new(HashMap::new()),
        }
    }

    /// Return all instances visible to a tenant for the embedded REST console.
    pub fn list_all_instances(&self, tenant_id: Option<&str>) -> Vec<ProcessInstance> {
        let mut instances: Vec<ProcessInstance> = self
            .instances
            .read()
            .unwrap()
            .values()
            .filter(|instance| match (tenant_id, &instance.tenant_id) {
                (None, _) => true,
                (Some(t), Some(instance_tenant)) => t == instance_tenant,
                (Some(""), None) => true,
                (Some(_), None) => false,
            })
            .cloned()
            .collect();
        instances.sort_by(|a, b| a.id.cmp(&b.id));
        instances
    }
}

impl Default for MemoryRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProcessInstanceStore for MemoryRepo {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessInstance>> {
        Ok(self.instances.read().unwrap().get(id).cloned())
    }

    async fn save(&self, instance: &ProcessInstance) -> anyhow::Result<()> {
        self.instances
            .write()
            .unwrap()
            .insert(instance.id.clone(), instance.clone());
        Ok(())
    }

    async fn list_running(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>> {
        let ids: Vec<String> = self
            .instances
            .read()
            .unwrap()
            .iter()
            .filter(|(_, i)| {
                i.state == InstanceState::Running
                    && match (tenant_id, &i.tenant_id) {
                        (None, _) => true,
                        (Some(t), Some(ti)) => t == ti.as_str(),
                        (Some(""), None) => true,
                        (Some(_), None) => false,
                    }
            })
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    async fn list_all(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>> {
        // Delegate to list_all_instances to keep the same tenant filter and
        // id ordering.
        Ok(self
            .list_all_instances(tenant_id)
            .into_iter()
            .map(|i| i.id)
            .collect())
    }

    async fn list_page(
        &self,
        tenant_id: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<Vec<String>> {
        // Same tenant filter and deterministic id ordering as list_all;
        // the window is applied in-memory.
        let mut ids = self.list_all(tenant_id).await?;
        let offset = (offset as usize).min(ids.len());
        ids.drain(..offset);
        ids.truncate(limit as usize);
        Ok(ids)
    }

    async fn find_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let scope = tenant_id.unwrap_or("");
        Ok(self
            .idempotency_keys
            .read()
            .unwrap()
            .get(&(scope.to_string(), key.to_string()))
            .cloned())
    }

    async fn save_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
        instance_id: &str,
    ) -> anyhow::Result<()> {
        let scope = tenant_id.unwrap_or("");
        self.idempotency_keys.write().unwrap().insert(
            (scope.to_string(), key.to_string()),
            instance_id.to_string(),
        );
        Ok(())
    }
}

#[async_trait]
impl TokenStore for MemoryRepo {
    async fn load_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<Token>> {
        Ok(self
            .instances
            .read()
            .unwrap()
            .get(instance_id)
            .map(|i| i.tokens.clone())
            .unwrap_or_default())
    }

    async fn save_tokens(&self, instance_id: &str, tokens: &[Token]) -> anyhow::Result<()> {
        if let Some(inst) = self.instances.write().unwrap().get_mut(instance_id) {
            inst.tokens = tokens.to_vec();
        }
        Ok(())
    }

    async fn update_token_cas(&self, instance_id: &str, token: &Token) -> anyhow::Result<bool> {
        let mut guard = self.instances.write().unwrap();
        let inst = match guard.get_mut(instance_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let pos = match inst.tokens.iter().position(|t| t.id == token.id) {
            Some(p) => p,
            None => return Ok(false),
        };
        if inst.tokens[pos].version != token.version {
            return Ok(false);
        }
        inst.tokens[pos] = token.clone();
        Ok(true)
    }

    async fn claim_token(
        &self,
        instance_id: &str,
        token_id: &str,
        version: u32,
    ) -> anyhow::Result<bool> {
        let mut guard = self.instances.write().unwrap();
        let inst = match guard.get_mut(instance_id) {
            Some(i) => i,
            None => return Ok(false),
        };
        let pos = match inst.tokens.iter().position(|t| t.id == token_id) {
            Some(p) => p,
            None => return Ok(false),
        };
        let t = &inst.tokens[pos];
        if t.status != TokenStatus::Ready || t.version != version {
            return Ok(false);
        }
        inst.tokens[pos].status = TokenStatus::Executing;
        inst.tokens[pos].version += 1;
        inst.tokens[pos].updated_at = Some(utc_now());
        Ok(true)
    }
}

#[async_trait]
impl OutboxRepo for MemoryRepo {
    async fn insert_pending(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: &str,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let ev = OutboxEvent {
            id: id.clone(),
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            status: "Pending".to_string(),
            tenant_id: tenant_id.map(String::from),
            created_at: Some(utc_now()),
        };
        self.outbox.write().unwrap().push(ev);
        Ok(id)
    }

    async fn list_pending(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<OutboxEvent>> {
        let out: Vec<OutboxEvent> = self
            .outbox
            .read()
            .unwrap()
            .iter()
            .filter(|e| {
                e.status == "Pending"
                    && match (tenant_id, &e.tenant_id) {
                        (None, _) => true,
                        (Some(t), Some(ti)) => t == ti.as_str(),
                        (Some(""), None) => true,
                        (Some(_), None) => false,
                    }
            })
            .cloned()
            .collect();
        Ok(out)
    }

    async fn mark_published(&self, id: &str) -> anyhow::Result<()> {
        if let Some(ev) = self
            .outbox
            .write()
            .unwrap()
            .iter_mut()
            .find(|ev| ev.id == id)
        {
            ev.status = "Published".to_string();
        }
        Ok(())
    }

    async fn claim_pending(
        &self,
        _worker_id: &str,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<OutboxEvent>> {
        let mut outbox = self.outbox.write().unwrap();
        let mut claimed = vec![];
        let limit = limit.min(100) as usize;
        for ev in outbox.iter_mut() {
            if ev.status != "Pending" {
                continue;
            }
            let matches = match (tenant_id, &ev.tenant_id) {
                (None, _) => true,
                (Some(t), Some(ti)) => t == ti.as_str(),
                (Some(""), None) => true,
                (Some(_), None) => false,
            };
            if matches {
                ev.status = "Dispatched".to_string();
                claimed.push(ev.clone());
                if claimed.len() >= limit {
                    break;
                }
            }
        }
        Ok(claimed)
    }

    async fn release_claimed(&self, id: &str) -> anyhow::Result<()> {
        if let Some(ev) = self
            .outbox
            .write()
            .unwrap()
            .iter_mut()
            .find(|ev| ev.id == id && ev.status == "Dispatched")
        {
            ev.status = "Pending".to_string();
        }
        Ok(())
    }
}

#[async_trait]
impl TimerStore for MemoryRepo {
    async fn get_by_id(&self, id: &str) -> anyhow::Result<Option<TimerRecord>> {
        Ok(self.timers.read().unwrap().get(id).cloned())
    }

    async fn mark_fired(&self, id: &str) -> anyhow::Result<()> {
        if let Some(r) = self.timers.write().unwrap().get_mut(id) {
            r.status = "Fired".to_string();
        }
        Ok(())
    }

    async fn insert(&self, record: &TimerRecord) -> anyhow::Result<()> {
        self.timers
            .write()
            .unwrap()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn list_due(&self, now_iso: &str, limit: u32) -> anyhow::Result<Vec<TimerRecord>> {
        let limit = limit.min(100) as usize;
        let due: Vec<TimerRecord> = self
            .timers
            .read()
            .unwrap()
            .values()
            .filter(|r| r.status == "Scheduled" && r.due_at.as_str() <= now_iso)
            .take(limit)
            .cloned()
            .collect();
        Ok(due)
    }
}

#[async_trait]
impl ParallelJoinRepo for MemoryRepo {
    async fn ensure_group(&self, group_id: &str, expected: u32) -> anyhow::Result<()> {
        self.parallel_join
            .write()
            .unwrap()
            .entry(group_id.to_string())
            .or_insert((expected, 0, false));
        Ok(())
    }

    async fn try_join(&self, group_id: &str) -> anyhow::Result<bool> {
        let mut guard = self.parallel_join.write().unwrap();
        let entry = match guard.get_mut(group_id) {
            Some(e) => e,
            None => return Ok(false),
        };
        entry.1 += 1;
        if entry.1 >= entry.0 && !entry.2 {
            entry.2 = true;
            return Ok(true);
        }
        Ok(false)
    }
}

#[async_trait]
impl CompensationRecordRepo for MemoryRepo {
    async fn add(&self, record: &CompensationRecordRow) -> anyhow::Result<()> {
        self.compensation.write().unwrap().push(record.clone());
        Ok(())
    }

    async fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        let mut out: Vec<CompensationRecordRow> = self
            .compensation
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.instance_id == instance_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| r.order);
        out
    }
}

#[async_trait]
impl ExternalTaskStore for MemoryRepo {
    async fn create(
        &self,
        token_id: &str,
        process_instance_id: &str,
        task_type: &str,
        retries: i32,
        _timeout_secs: u64,
        variables: HashMap<String, String>,
    ) -> anyhow::Result<String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let now = utc_now();
        let _now_secs = unix_secs();
        let row = ExternalTaskRow {
            task_id: task_id.clone(),
            token_id: token_id.to_string(),
            process_instance_id: process_instance_id.to_string(),
            task_type: task_type.to_string(),
            state: ExternalTaskState::Ready,
            lock_owner: None,
            lock_expire_at: None,
            retries,
            error_message: None,
            variables,
            created_at: now.clone(),
            updated_at: now,
        };
        self.external_tasks
            .write()
            .unwrap()
            .insert(task_id.clone(), row);
        Ok(task_id)
    }

    async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
    ) -> anyhow::Result<Vec<ExternalTask>> {
        let _ = self.reclaim_expired_locks().await?;
        let expire_at = unix_secs() + lock_duration.as_secs();
        let task_types: std::collections::HashSet<_> = task_types.iter().cloned().collect();
        // Atomically filter, sort, select, and lock — single WriteLock critical section
        // eliminates TOCTOU race where two workers could acquire the same task.
        let mut guard = self.external_tasks.write().unwrap();
        let mut order: Vec<(String, String)> = guard
            .iter()
            .filter(|(_, r)| {
                r.state == ExternalTaskState::Ready && task_types.contains(&r.task_type)
            })
            .map(|(id, r)| (id.clone(), r.created_at.clone()))
            .collect();
        order.sort_by(|a, b| a.1.cmp(&b.1));
        let take: Vec<String> = order
            .into_iter()
            .take(max_tasks)
            .map(|(id, _)| id)
            .collect();
        let mut out = vec![];
        for task_id in take {
            if let Some(r) = guard.get_mut(&task_id) {
                r.state = ExternalTaskState::Locked;
                r.lock_owner = Some(worker_id.to_string());
                r.lock_expire_at = Some(expire_at);
                r.updated_at = utc_now();
                out.push(r.to_external_task());
            }
        }
        Ok(out)
    }

    async fn fetch_and_lock_scoped(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
        tenant_id: &str,
    ) -> anyhow::Result<Vec<ExternalTask>> {
        let _ = self.reclaim_expired_locks().await?;
        let expire_at = unix_secs() + lock_duration.as_secs();
        let task_types: std::collections::HashSet<_> = task_types.iter().cloned().collect();
        // Tenant scoping INSIDE the select-and-lock section (E5-1): a task of
        // another tenant is never selected, so no lease is taken for it. The
        // instance read lock is taken before the task write lock; no
        // MemoryRepo method takes the two locks in the opposite order, so
        // this cannot deadlock.
        let instances = self.instances.read().unwrap();
        let tenant_ok = |instance_id: &str| {
            instances
                .get(instance_id)
                .is_some_and(|i| i.tenant_id.as_deref() == Some(tenant_id))
        };
        let mut guard = self.external_tasks.write().unwrap();
        let mut order: Vec<(String, String)> = guard
            .iter()
            .filter(|(_, r)| {
                r.state == ExternalTaskState::Ready
                    && task_types.contains(&r.task_type)
                    && tenant_ok(&r.process_instance_id)
            })
            .map(|(id, r)| (id.clone(), r.created_at.clone()))
            .collect();
        order.sort_by(|a, b| a.1.cmp(&b.1));
        let take: Vec<String> = order
            .into_iter()
            .take(max_tasks)
            .map(|(id, _)| id)
            .collect();
        let mut out = vec![];
        for task_id in take {
            if let Some(r) = guard.get_mut(&task_id) {
                r.state = ExternalTaskState::Locked;
                r.lock_owner = Some(worker_id.to_string());
                r.lock_expire_at = Some(expire_at);
                r.updated_at = utc_now();
                out.push(r.to_external_task());
            }
        }
        Ok(out)
    }

    async fn complete(
        &self,
        task_id: &str,
        worker_id: &str,
        variables: HashMap<String, String>,
    ) -> Result<(), bpm_engine_storage::ExternalTaskError> {
        let now_secs = unix_secs();
        let mut guard = self.external_tasks.write().unwrap();
        let r = guard.get_mut(task_id).ok_or_else(|| {
            bpm_engine_storage::ExternalTaskError::TaskNotFound {
                task_id: task_id.to_string(),
            }
        })?;
        if r.state != ExternalTaskState::Locked {
            return Err(bpm_engine_storage::ExternalTaskError::TaskNotLocked {
                task_id: task_id.to_string(),
            });
        }
        if r.lock_owner.as_deref() != Some(worker_id) {
            return Err(bpm_engine_storage::InvariantViolation::new(
                bpm_engine_storage::InvariantViolationKind::ExternalTaskLeaseConflict,
                format!(
                    "task_id={} expected_owner={:?} actual_worker={}",
                    task_id,
                    r.lock_owner.as_deref(),
                    worker_id
                ),
            )
            .into());
        }
        if let Some(exp) = r.lock_expire_at {
            if exp <= now_secs {
                return Err(bpm_engine_storage::ExternalTaskError::LockExpired {
                    task_id: task_id.to_string(),
                });
            }
        }
        r.state = ExternalTaskState::Completed;
        for (k, v) in variables {
            r.variables.insert(k, v);
        }
        r.lock_owner = None;
        r.lock_expire_at = None;
        r.updated_at = utc_now();
        Ok(())
    }

    async fn fail(
        &self,
        task_id: &str,
        worker_id: &str,
        error: String,
        _retry_after: Option<Duration>,
        permanent: bool,
    ) -> Result<(), bpm_engine_storage::ExternalTaskError> {
        let mut guard = self.external_tasks.write().unwrap();
        let r = guard.get_mut(task_id).ok_or_else(|| {
            bpm_engine_storage::ExternalTaskError::TaskNotFound {
                task_id: task_id.to_string(),
            }
        })?;
        if r.state != ExternalTaskState::Locked {
            return Err(bpm_engine_storage::ExternalTaskError::TaskNotLocked {
                task_id: task_id.to_string(),
            });
        }
        if r.lock_owner.as_deref() != Some(worker_id) {
            return Err(bpm_engine_storage::InvariantViolation::new(
                bpm_engine_storage::InvariantViolationKind::ExternalTaskLeaseConflict,
                format!(
                    "task_id={} expected_owner={:?} actual_worker={}",
                    task_id,
                    r.lock_owner.as_deref(),
                    worker_id
                ),
            )
            .into());
        }
        r.error_message = Some(error);
        r.lock_owner = None;
        r.lock_expire_at = None;
        r.updated_at = utc_now();
        if permanent {
            // E3-2 structured failure: skip the retry loop entirely — no
            // decrement, no requeue; the task goes straight to the terminal
            // Failed state (same downstream path as retry exhaustion).
            r.state = ExternalTaskState::Failed;
        } else {
            r.retries -= 1;
            if r.retries > 0 {
                r.state = ExternalTaskState::Ready;
            } else {
                r.state = ExternalTaskState::Failed;
            }
        }
        Ok(())
    }

    async fn reclaim_expired_locks(&self) -> anyhow::Result<usize> {
        let now_secs = unix_secs();
        let mut guard = self.external_tasks.write().unwrap();
        let mut n = 0;
        for r in guard.values_mut() {
            if r.state == ExternalTaskState::Locked {
                if let Some(exp) = r.lock_expire_at {
                    if exp <= now_secs {
                        r.state = ExternalTaskState::Ready;
                        r.lock_owner = None;
                        r.lock_expire_at = None;
                        r.updated_at = utc_now();
                        n += 1;
                    }
                }
            }
        }
        Ok(n)
    }

    async fn extend_lock(
        &self,
        task_id: &str,
        worker_id: &str,
        extension: Duration,
    ) -> anyhow::Result<bool> {
        let mut guard = self.external_tasks.write().unwrap();
        let r = match guard.get_mut(task_id) {
            Some(r) => r,
            None => return Ok(false),
        };
        if r.state != ExternalTaskState::Locked {
            return Ok(false);
        }
        if r.lock_owner.as_deref() != Some(worker_id) {
            return Ok(false);
        }
        let new_expire = unix_secs() + extension.as_secs();
        r.lock_expire_at = Some(new_expire);
        r.updated_at = utc_now();
        Ok(true)
    }

    async fn get(&self, task_id: &str) -> anyhow::Result<Option<ExternalTask>> {
        Ok(self
            .external_tasks
            .read()
            .unwrap()
            .get(task_id)
            .map(|r| r.to_external_task()))
    }

    async fn get_by_token(&self, token_id: &str) -> anyhow::Result<Option<ExternalTask>> {
        // E5-2 grant-effect idempotency: any task row for the token (any
        // state) proves the approval-grant effect already created it.
        let guard = self.external_tasks.read().unwrap();
        let found = guard
            .values()
            .find(|r| r.token_id == token_id)
            .map(|r| r.to_external_task());
        Ok(found)
    }
}

#[async_trait]
impl HistoryRepo for MemoryRepo {
    async fn append(
        &self,
        instance_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
        occurred_at: &str,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let row = HistoryEventRow {
            id: id.clone(),
            instance_id: instance_id.to_string(),
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            occurred_at: occurred_at.to_string(),
        };
        self.history_events.write().unwrap().push(row);
        Ok(id)
    }

    async fn list_by_instance(
        &self,
        instance_id: &str,
        token_id_filter: Option<&str>,
        event_type_filter: Option<&str>,
    ) -> anyhow::Result<Vec<HistoryEvent>> {
        let guard = self.history_events.read().unwrap();
        let mut out: Vec<HistoryEvent> = guard
            .iter()
            .filter(|r| r.instance_id == instance_id)
            .filter(|r| event_type_filter.is_none_or(|f| r.event_type == f))
            .filter(|r| {
                token_id_filter.is_none_or(|tid| {
                    let tid_in_payload: Option<String> =
                        serde_json::from_str::<serde_json::Value>(&r.payload)
                            .ok()
                            .and_then(|v| {
                                v.get("token_id").and_then(|t| t.as_str()).map(String::from)
                            });
                    tid_in_payload.as_deref() == Some(tid)
                })
            })
            .map(|r| HistoryEvent {
                id: r.id.clone(),
                instance_id: r.instance_id.clone(),
                event_type: r.event_type.clone(),
                payload: serde_json::from_str(&r.payload).unwrap_or(serde_json::Value::Null),
                occurred_at: r.occurred_at.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.occurred_at.cmp(&b.occurred_at));
        Ok(out)
    }

    async fn delete_older_than(&self, cutoff: &str) -> anyhow::Result<u64> {
        // Numeric comparison of unix-seconds strings (a plain text compare
        // would misorder e.g. "999" vs "1000"). Rows whose occurred_at is
        // not a unix-seconds number are KEPT: retention must never delete
        // a row it cannot age-assess.
        let cutoff: Option<u64> = cutoff.parse().ok();
        let mut guard = self.history_events.write().unwrap();
        let before = guard.len();
        guard.retain(|r| {
            match (cutoff, r.occurred_at.parse::<u64>().ok()) {
                (Some(cutoff), Some(occurred)) => occurred >= cutoff,
                // Unparseable cutoff: delete nothing (fail safe).
                // Unparseable row: keep (never delete what cannot be aged).
                _ => true,
            }
        });
        let deleted = (before - guard.len()) as u64;
        Ok(deleted)
    }
}

#[async_trait]
impl ApprovalStore for MemoryRepo {
    async fn create(&self, record: &ApprovalRecord) -> anyhow::Result<()> {
        // Idempotent by subject (instance_id, node_id, token_id): an
        // existing record is KEPT — a re-arrival of the same token at the
        // same node (crash recovery, event re-delivery) must not create a
        // second approval requirement. The whole check-insert runs inside
        // one write-lock critical section, so concurrent arrivals also
        // collapse to a single record.
        let mut guard = self.approvals.write().unwrap();
        let subject_exists = guard.values().any(|r| {
            r.instance_id == record.instance_id
                && r.node_id == record.node_id
                && r.token_id == record.token_id
        });
        if !subject_exists {
            guard.insert(record.id.clone(), record.clone());
        }
        Ok(())
    }

    async fn get(&self, approval_id: &str) -> anyhow::Result<Option<ApprovalRecord>> {
        Ok(self.approvals.read().unwrap().get(approval_id).cloned())
    }

    async fn list_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<ApprovalRecord>> {
        let mut out: Vec<ApprovalRecord> = self
            .approvals
            .read()
            .unwrap()
            .values()
            .filter(|r| r.instance_id == instance_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    async fn decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
    ) -> Result<ApprovalDecideOutcome, ApprovalError> {
        // Only-once, atomically: the status check and the write share ONE
        // write-lock critical section, so two concurrent decides (or a
        // decide racing a retry) can never both flip the record — exactly
        // one observes Pending and writes, the other reads the outcome.
        //
        // The DECISION is durable after this critical section; the record's
        // EFFECT stays Pending until the caller applies it and confirms
        // with mark_effect_applied (ADR-008 §7.1's re-drivable unit).
        let mut guard = self.approvals.write().unwrap();
        let record = guard
            .get_mut(approval_id)
            .ok_or_else(|| ApprovalError::NotFound {
                approval_id: approval_id.to_string(),
            })?;
        if record.status != ApprovalStatus::Pending {
            return Ok(ApprovalDecideOutcome::AlreadyDecided(record.clone()));
        }
        record.status = decision.resulting_status();
        record.effect = bpm_engine_core::ApprovalEffect::Pending;
        record.decided_by = Some(actor.to_string());
        record.decided_at = Some(utc_now());
        record.note = note.map(str::to_string);
        Ok(ApprovalDecideOutcome::Decided(record.clone()))
    }

    async fn mark_effect_applied(&self, approval_id: &str) -> anyhow::Result<()> {
        // Idempotent: an already-applied record stays applied; an unknown
        // id is a no-op (the caller loaded the record first).
        if let Some(record) = self.approvals.write().unwrap().get_mut(approval_id) {
            record.effect = bpm_engine_core::ApprovalEffect::Applied;
        }
        Ok(())
    }

    async fn list_decided_with_pending_effect(&self) -> anyhow::Result<Vec<ApprovalRecord>> {
        // The startup sweep's work list: decisions whose effect has not
        // (fully) been applied yet, in creation order.
        let mut out: Vec<ApprovalRecord> = self
            .approvals
            .read()
            .unwrap()
            .values()
            .filter(|r| {
                r.status != ApprovalStatus::Pending
                    && r.effect == bpm_engine_core::ApprovalEffect::Pending
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_core::ProcessInstance;
    use std::time::Duration;

    #[tokio::test]
    async fn list_all_includes_completed_instances_sorted_by_id() {
        let repo = MemoryRepo::new();
        let make_instance =
            |id: &str, state: InstanceState, tenant: Option<&str>| ProcessInstance {
                id: id.to_string(),
                process_def_id: "def-1".to_string(),
                tenant_id: tenant.map(String::from),
                tokens: vec![],
                variables: HashMap::new(),
                state,
                version: 1,
                parent_instance_id: None,
                parent_token_id: None,
            };
        repo.save(&make_instance("inst-b", InstanceState::Completed, None))
            .await
            .unwrap();
        repo.save(&make_instance("inst-a", InstanceState::Running, Some("t1")))
            .await
            .unwrap();
        repo.save(&make_instance("inst-c", InstanceState::Terminated, None))
            .await
            .unwrap();

        // All states included, sorted by id
        let all = repo.list_all(None).await.unwrap();
        assert_eq!(all, vec!["inst-a", "inst-b", "inst-c"]);

        // list_running only includes the running one
        let running = repo.list_running(None).await.unwrap();
        assert_eq!(running, vec!["inst-a"]);

        // Tenant filter applies to list_all as well
        let tenant = repo.list_all(Some("t1")).await.unwrap();
        assert_eq!(tenant, vec!["inst-a"]);
        let other = repo.list_all(Some("t2")).await.unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn external_task_store_create_fetch_lock_complete() {
        let repo = MemoryRepo::new();
        let task_id = ExternalTaskStore::create(
            &repo,
            "token-1",
            "instance-1",
            "payment",
            3,
            60,
            HashMap::from([("amount".to_string(), "100".to_string())]),
        )
        .await
        .unwrap();
        assert!(!task_id.is_empty());

        let tasks = repo
            .fetch_and_lock(
                "worker-1",
                &["payment".to_string()],
                10,
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, task_id);
        assert_eq!(tasks[0].state, ExternalTaskState::Locked);
        assert_eq!(tasks[0].lock_owner.as_deref(), Some("worker-1"));

        repo.complete(
            &task_id,
            "worker-1",
            HashMap::from([("result".to_string(), "ok".to_string())]),
        )
        .await
        .unwrap();

        let task = ExternalTaskStore::get(&repo, &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.state, ExternalTaskState::Completed);
    }

    #[tokio::test]
    async fn external_task_store_fail_retry_then_fail() {
        let repo = MemoryRepo::new();
        let task_id = ExternalTaskStore::create(
            &repo,
            "token-1",
            "instance-1",
            "notify",
            2,
            60,
            HashMap::new(),
        )
        .await
        .unwrap();

        repo.fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        repo.fail(&task_id, "worker-1", "timeout".to_string(), None, false)
            .await
            .unwrap();
        let task = ExternalTaskStore::get(&repo, &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.state, ExternalTaskState::Ready);
        assert_eq!(task.retries, 1);

        repo.fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        repo.fail(&task_id, "worker-1", "again".to_string(), None, false)
            .await
            .unwrap();
        let task = ExternalTaskStore::get(&repo, &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.state, ExternalTaskState::Failed);
    }

    #[tokio::test]
    async fn external_task_store_fail_permanent_skips_retries() {
        // E3-2 structured failure: permanent fail goes straight to Failed —
        // no retry decrement, no requeue (fetch finds nothing).
        let repo = MemoryRepo::new();
        let task_id = ExternalTaskStore::create(
            &repo,
            "token-1",
            "instance-1",
            "notify",
            3,
            60,
            HashMap::new(),
        )
        .await
        .unwrap();

        repo.fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        repo.fail(
            &task_id,
            "worker-1",
            r#"{"retry_category":"permanent"}: bad input"#.to_string(),
            None,
            true,
        )
        .await
        .unwrap();
        let task = ExternalTaskStore::get(&repo, &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.state, ExternalTaskState::Failed);
        assert_eq!(task.retries, 3, "permanent fail must not decrement retries");
        assert!(task.error_message.unwrap().contains("permanent"));

        // No redelivery: the terminal task is never fetchable again.
        let tasks = repo
            .fetch_and_lock(
                "worker-2",
                &["notify".to_string()],
                10,
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(tasks.is_empty(), "failed task must not be redelivered");
    }

    #[tokio::test]
    async fn external_task_store_reclaim_expired_locks() {
        let repo = MemoryRepo::new();
        let _ =
            ExternalTaskStore::create(&repo, "token-1", "instance-1", "job", 1, 60, HashMap::new())
                .await
                .unwrap();
        repo.fetch_and_lock("worker-1", &["job".to_string()], 10, Duration::from_secs(0))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let n = repo.reclaim_expired_locks().await.unwrap();
        assert!(n >= 1);
        let tasks = repo
            .fetch_and_lock(
                "worker-2",
                &["job".to_string()],
                10,
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
    }

    // --- ApprovalStore (E5-2) ------------------------------------------------

    fn make_approval(id: &str, token_id: &str) -> ApprovalRecord {
        ApprovalRecord {
            id: id.to_string(),
            instance_id: "inst-1".to_string(),
            node_id: "charge".to_string(),
            token_id: token_id.to_string(),
            task_type: "payment_charge".to_string(),
            status: ApprovalStatus::Pending,
            effect: bpm_engine_core::ApprovalEffect::Pending,
            decided_by: None,
            decided_at: None,
            note: None,
            created_at: "1000".to_string(),
        }
    }

    #[tokio::test]
    async fn approval_create_is_idempotent_by_subject() {
        // Two arrivals of the same token at the same node (crash recovery /
        // re-delivery) collapse to ONE record: the first id and status win.
        let repo = MemoryRepo::new();
        ApprovalStore::create(&repo, &make_approval("appr-a", "tok-1"))
            .await
            .unwrap();
        let second = make_approval("appr-b", "tok-1");
        ApprovalStore::create(&repo, &second).await.unwrap();

        let all = ApprovalStore::list_by_instance(&repo, "inst-1")
            .await
            .unwrap();
        assert_eq!(all.len(), 1, "same subject must not create a second record");
        assert_eq!(all[0].id, "appr-a", "the original record is kept");

        // A DIFFERENT token at the same node is a different subject.
        ApprovalStore::create(&repo, &make_approval("appr-c", "tok-2"))
            .await
            .unwrap();
        assert_eq!(
            ApprovalStore::list_by_instance(&repo, "inst-1")
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn approval_decide_is_only_once_and_returns_original_outcome() {
        let repo = MemoryRepo::new();
        ApprovalStore::create(&repo, &make_approval("appr-1", "tok-1"))
            .await
            .unwrap();

        // First decide wins.
        match repo
            .decide("appr-1", ApprovalDecision::Approve, "alice", Some("ok"))
            .await
            .unwrap()
        {
            ApprovalDecideOutcome::Decided(r) => {
                assert_eq!(r.status, ApprovalStatus::Approved);
                assert_eq!(r.decided_by.as_deref(), Some("alice"));
                assert_eq!(r.note.as_deref(), Some("ok"));
                assert!(r.decided_at.is_some());
                // The DECISION is durable but the EFFECT is still pending —
                // the caller applies it and confirms (ADR-008 §7.1).
                assert_eq!(r.effect, bpm_engine_core::ApprovalEffect::Pending);
            }
            other => panic!("expected Decided, got {other:?}"),
        }

        // Retried SAME decision: replay with the original outcome.
        match repo
            .decide("appr-1", ApprovalDecision::Approve, "alice", None)
            .await
            .unwrap()
        {
            ApprovalDecideOutcome::AlreadyDecided(r) => {
                assert_eq!(r.status, ApprovalStatus::Approved);
                assert_eq!(r.decided_by.as_deref(), Some("alice"));
                // The note of the winning decide survives, not the retry's.
                assert_eq!(r.note.as_deref(), Some("ok"));
            }
            other => panic!("expected AlreadyDecided, got {other:?}"),
        }

        // A conflicting decision never flips the record.
        let outcome = repo
            .decide("appr-1", ApprovalDecision::Reject, "bob", None)
            .await
            .unwrap();
        assert!(matches!(outcome, ApprovalDecideOutcome::AlreadyDecided(_)));
        assert_eq!(
            ApprovalStore::get(&repo, "appr-1")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::Approved
        );
    }

    #[tokio::test]
    async fn approval_decide_reject_path_and_not_found() {
        let repo = MemoryRepo::new();
        ApprovalStore::create(&repo, &make_approval("appr-2", "tok-9"))
            .await
            .unwrap();

        match repo
            .decide("appr-2", ApprovalDecision::Reject, "bob", Some("no"))
            .await
            .unwrap()
        {
            ApprovalDecideOutcome::Decided(r) => {
                assert_eq!(r.status, ApprovalStatus::Rejected);
            }
            other => panic!("expected Decided, got {other:?}"),
        }

        let err = repo
            .decide("missing", ApprovalDecision::Approve, "x", None)
            .await
            .unwrap_err();
        match err {
            ApprovalError::NotFound { approval_id } => assert_eq!(approval_id, "missing"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_list_by_instance_is_creation_ordered() {
        let repo = MemoryRepo::new();
        let mut first = make_approval("appr-b", "tok-1");
        first.created_at = "1002".to_string();
        let mut second = make_approval("appr-a", "tok-2");
        second.created_at = "1001".to_string();
        let mut other_instance = make_approval("appr-x", "tok-3");
        other_instance.instance_id = "inst-2".to_string();
        ApprovalStore::create(&repo, &first).await.unwrap();
        ApprovalStore::create(&repo, &second).await.unwrap();
        ApprovalStore::create(&repo, &other_instance).await.unwrap();

        let ids: Vec<String> = ApprovalStore::list_by_instance(&repo, "inst-1")
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["appr-a", "appr-b"]);
    }

    #[tokio::test]
    async fn approval_effect_lifecycle_and_sweep_list() {
        // E5-2 (ADR-008 §7.1): decide leaves the effect Pending;
        // mark_effect_applied flips it (idempotently);
        // list_decided_with_pending_effect is the sweep's work list —
        // undecided records and applied effects are NOT on it.
        let repo = MemoryRepo::new();
        ApprovalStore::create(&repo, &make_approval("appr-1", "tok-1"))
            .await
            .unwrap();
        ApprovalStore::create(&repo, &make_approval("appr-2", "tok-2"))
            .await
            .unwrap();
        ApprovalStore::create(&repo, &make_approval("appr-3", "tok-3"))
            .await
            .unwrap();

        repo.decide("appr-1", ApprovalDecision::Approve, "a", None)
            .await
            .unwrap();
        repo.decide("appr-2", ApprovalDecision::Reject, "b", None)
            .await
            .unwrap();
        // appr-3 stays undecided.

        let pending_effect = repo.list_decided_with_pending_effect().await.unwrap();
        assert_eq!(
            pending_effect
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["appr-1", "appr-2"],
            "both decisions have a pending effect; the undecided record does not"
        );

        // The caller applied appr-1's effect: confirmed, idempotently.
        repo.mark_effect_applied("appr-1").await.unwrap();
        repo.mark_effect_applied("appr-1").await.unwrap();
        assert_eq!(
            ApprovalStore::get(&repo, "appr-1")
                .await
                .unwrap()
                .unwrap()
                .effect,
            bpm_engine_core::ApprovalEffect::Applied
        );
        // An unknown id is a tolerated no-op (the route loads first).
        repo.mark_effect_applied("missing").await.unwrap();

        let pending_effect = repo.list_decided_with_pending_effect().await.unwrap();
        assert_eq!(
            pending_effect
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["appr-2"],
            "only the unconfirmed decision remains on the sweep list"
        );
    }

    #[tokio::test]
    async fn external_task_get_by_token_finds_any_state() {
        // E5-2 grant-effect idempotency guard: any task row for the token
        // (here: Locked after a fetch) proves the grant effect already ran.
        let repo = MemoryRepo::new();
        assert!(
            repo.get_by_token("tok-1").await.unwrap().is_none(),
            "no task exists yet for the token"
        );
        ExternalTaskStore::create(&repo, "tok-1", "inst-1", "job", 1, 60, HashMap::new())
            .await
            .unwrap();
        repo.fetch_and_lock("w-1", &["job".to_string()], 10, Duration::from_secs(30))
            .await
            .unwrap();

        let found = repo.get_by_token("tok-1").await.unwrap().unwrap();
        assert_eq!(found.token_id, "tok-1");
        assert_eq!(found.process_instance_id, "inst-1");
        assert!(
            repo.get_by_token("tok-other").await.unwrap().is_none(),
            "another token has no task"
        );
    }

    #[tokio::test]
    async fn history_delete_older_than_keeps_recent_and_unparseable() {
        // E5-3 retention: strictly-older rows are deleted; recent rows,
        // other instances' rows, and rows whose occurred_at is not a
        // unix-seconds number are kept. Numeric comparison, not text.
        let repo = MemoryRepo::new();
        for (id, instance, occurred_at) in [
            ("h-old", "inst-1", "1000"),
            ("h-recent", "inst-1", "2000"),
            ("h-other", "inst-2", "1000"),
            ("h-weird", "inst-1", "not-a-number"),
        ] {
            HistoryRepo::append(
                &repo,
                instance,
                "TokenArrived",
                &serde_json::json!({ "id": id }),
                occurred_at,
            )
            .await
            .unwrap();
        }

        let deleted = repo.delete_older_than("1500").await.unwrap();
        assert_eq!(deleted, 2, "h-old and h-other are strictly older");
        let mut kept: Vec<String> = HistoryRepo::list_by_instance(&repo, "inst-1", None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.payload["id"].as_str().unwrap().to_string())
            .collect();
        kept.sort();
        assert_eq!(kept, vec!["h-recent".to_string(), "h-weird".to_string()]);

        // Boundary: equal to the cutoff is KEPT (strictly-older deletion).
        let deleted = repo.delete_older_than("2000").await.unwrap();
        assert_eq!(deleted, 0, "the cutoff row itself is not old");
        // An unparseable cutoff deletes nothing (fail safe).
        let deleted = repo.delete_older_than("oops").await.unwrap();
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn fetch_and_lock_scoped_delivers_only_the_tenants_tasks() {
        // E5-1: tenant scoping happens inside the select-and-lock section —
        // another tenant's task is neither delivered NOR leased, and a
        // tenant-less instance is not visible to a scoped tenant.
        let repo = MemoryRepo::new();
        let make_instance = |id: &str, tenant: Option<&str>| ProcessInstance {
            id: id.to_string(),
            process_def_id: "def-1".to_string(),
            tenant_id: tenant.map(String::from),
            tokens: vec![],
            variables: HashMap::new(),
            state: InstanceState::Running,
            version: 1,
            parent_instance_id: None,
            parent_token_id: None,
        };
        repo.save(&make_instance("inst-a", Some("tenant-a")))
            .await
            .unwrap();
        repo.save(&make_instance("inst-b", Some("tenant-b")))
            .await
            .unwrap();
        repo.save(&make_instance("inst-none", None)).await.unwrap();
        ExternalTaskStore::create(&repo, "tok-a", "inst-a", "job", 1, 60, HashMap::new())
            .await
            .unwrap();
        ExternalTaskStore::create(&repo, "tok-b", "inst-b", "job", 1, 60, HashMap::new())
            .await
            .unwrap();
        ExternalTaskStore::create(&repo, "tok-n", "inst-none", "job", 1, 60, HashMap::new())
            .await
            .unwrap();

        // Tenant-a's worker: gets only its own task.
        let tasks = repo
            .fetch_and_lock_scoped(
                "w-a",
                &["job".to_string()],
                10,
                Duration::from_secs(30),
                "tenant-a",
            )
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].process_instance_id, "inst-a");

        // The other two tasks were never leased: they stay fetchable for
        // their own (or any unscoped) worker.
        let all = repo
            .fetch_and_lock("w-any", &["job".to_string()], 10, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(
            all.len(),
            2,
            "scoped fetch must not lease other tenants' tasks"
        );
    }
}
