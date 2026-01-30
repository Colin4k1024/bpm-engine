//! Unified in-memory store: ProcessInstance (with tokens), Outbox, Timer, ParallelJoin, Compensation.
//! Implements all storage traits for runtime EngineContext.

use async_trait::async_trait;
use bpm_core::{InstanceState, ProcessInstance, Token, TokenStatus};
use bpm_storage::{
    CompensationRecordRepo, CompensationRecordRow, OutboxEvent, OutboxRepo, ParallelJoinRepo,
    ProcessInstanceRepo, TimerRecord, TimerRepo, TokenRepo,
};
use std::collections::HashMap;
use std::sync::RwLock;

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

/// Unified in-memory repo: instances (with tokens inside), outbox, timers, parallel_join, compensation.
pub struct MemoryRepo {
    instances: RwLock<HashMap<String, ProcessInstance>>,
    outbox: RwLock<Vec<OutboxEvent>>,
    timers: RwLock<HashMap<String, TimerRecord>>,
    parallel_join: RwLock<HashMap<String, (u32, u32, bool)>>,
    compensation: RwLock<Vec<CompensationRecordRow>>,
}

impl MemoryRepo {
    pub fn new() -> Self {
        MemoryRepo {
            instances: RwLock::new(HashMap::new()),
            outbox: RwLock::new(Vec::new()),
            timers: RwLock::new(HashMap::new()),
            parallel_join: RwLock::new(HashMap::new()),
            compensation: RwLock::new(Vec::new()),
        }
    }
}

impl Default for MemoryRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProcessInstanceRepo for MemoryRepo {
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
impl TokenRepo for MemoryRepo {
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

    async fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> anyhow::Result<bool> {
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
        if let Some(ev) = self.outbox.write().unwrap().iter_mut().find(|ev| ev.id == id) {
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
impl TimerRepo for MemoryRepo {
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

