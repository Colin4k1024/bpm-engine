//! Unified in-memory store: ProcessInstance (with tokens), Outbox, Timer, ParallelJoin, Compensation, ExternalTask.
//! Implements all storage traits for runtime EngineContext.

use async_trait::async_trait;
use bpm_engine_core::{
    ExternalTask, ExternalTaskState, InstanceState, ProcessInstance, Token, TokenStatus,
};
use bpm_engine_storage::{
    CompensationRecordRepo, CompensationRecordRow, ExternalTaskStore, HistoryEvent, HistoryRepo,
    OutboxEvent, OutboxRepo, ParallelJoinRepo, ProcessInstanceStore, TimerRecord, TimerStore,
    TokenStore,
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
        }
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
        r.retries -= 1;
        r.error_message = Some(error);
        r.lock_owner = None;
        r.lock_expire_at = None;
        r.updated_at = utc_now();
        if r.retries > 0 {
            r.state = ExternalTaskState::Ready;
        } else {
            r.state = ExternalTaskState::Failed;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn external_task_store_create_fetch_lock_complete() {
        let repo = MemoryRepo::new();
        let task_id = repo
            .create(
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

        let task = repo.get(&task_id).await.unwrap().unwrap();
        assert_eq!(task.state, ExternalTaskState::Completed);
    }

    #[tokio::test]
    async fn external_task_store_fail_retry_then_fail() {
        let repo = MemoryRepo::new();
        let task_id = repo
            .create("token-1", "instance-1", "notify", 2, 60, HashMap::new())
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
        repo.fail(&task_id, "worker-1", "timeout".to_string(), None)
            .await
            .unwrap();
        let task = repo.get(&task_id).await.unwrap().unwrap();
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
        repo.fail(&task_id, "worker-1", "again".to_string(), None)
            .await
            .unwrap();
        let task = repo.get(&task_id).await.unwrap().unwrap();
        assert_eq!(task.state, ExternalTaskState::Failed);
    }

    #[tokio::test]
    async fn external_task_store_reclaim_expired_locks() {
        let repo = MemoryRepo::new();
        let _ = repo
            .create("token-1", "instance-1", "job", 1, 60, HashMap::new())
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
}
