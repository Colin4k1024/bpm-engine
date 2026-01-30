//! Full in-memory implementation of all repo traits (design: overview §5, plan v1.0).
//! Enables EngineContext without SQLite for tests and embedding.

use crate::model::{InstanceState, ProcessInstance, Token, TokenStatus};
use std::collections::HashMap;
use std::sync::RwLock;

use super::repo::{
    CompensationRecordRepo, CompensationRecordRow, OutboxEvent, OutboxRepo, ParallelJoinRepo,
    ProcessInstanceRepo, TimerRecord, TimerRepo, TokenRepo, TransactionScope,
};

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

/// In-memory store implementing ProcessInstanceRepo, TokenRepo, OutboxRepo,
/// TimerRepo, ParallelJoinRepo, CompensationRecordRepo.
/// Use with Arc for TransactionScope (with_tx passes cloned Arc as both repos).
pub struct MemoryRepo {
    instances: RwLock<HashMap<String, ProcessInstance>>,
    outbox: RwLock<Vec<OutboxEvent>>,
    timers: RwLock<HashMap<String, TimerRecord>>,
    /// group_id -> (expected, arrived_count, joined)
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

impl ProcessInstanceRepo for MemoryRepo {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        self.instances.read().unwrap().get(id).cloned()
    }

    fn save(&self, instance: &ProcessInstance) {
        self.instances
            .write()
            .unwrap()
            .insert(instance.id.clone(), instance.clone());
    }

    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        self.instances
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
            .collect()
    }
}

impl TokenRepo for MemoryRepo {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        self.instances
            .read()
            .unwrap()
            .get(instance_id)
            .map(|i| i.tokens.clone())
            .unwrap_or_default()
    }

    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        if let Some(inst) = self.instances.write().unwrap().get_mut(instance_id) {
            inst.tokens = tokens.to_vec();
        }
    }

    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        let mut guard = self.instances.write().unwrap();
        let inst = match guard.get_mut(instance_id) {
            Some(i) => i,
            None => return false,
        };
        let pos = match inst.tokens.iter().position(|t| t.id == token.id) {
            Some(p) => p,
            None => return false,
        };
        if inst.tokens[pos].version != token.version {
            return false;
        }
        inst.tokens[pos] = token.clone();
        true
    }

    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        let mut guard = self.instances.write().unwrap();
        let inst = match guard.get_mut(instance_id) {
            Some(i) => i,
            None => return false,
        };
        let pos = match inst.tokens.iter().position(|t| t.id == token_id) {
            Some(p) => p,
            None => return false,
        };
        let t = &inst.tokens[pos];
        if t.status != TokenStatus::Ready || t.version != version {
            return false;
        }
        inst.tokens[pos].status = TokenStatus::Executing;
        inst.tokens[pos].version += 1;
        inst.tokens[pos].updated_at = Some(utc_now());
        true
    }
}

impl OutboxRepo for MemoryRepo {
    fn insert_pending(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

    fn list_pending(&self, tenant_id: Option<&str>) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let out: Vec<_> = self
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

    fn mark_published(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut outbox = self.outbox.write().unwrap();
        let e = outbox.iter_mut().find(|ev| ev.id == id);
        match e {
            Some(ev) => {
                ev.status = "Published".to_string();
                Ok(())
            }
            None => Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "outbox event not found",
            ))),
        }
    }

    fn claim_pending(
        &self,
        _worker_id: &str,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
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

    fn release_claimed(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut outbox = self.outbox.write().unwrap();
        if let Some(ev) = outbox.iter_mut().find(|ev| ev.id == id && ev.status == "Dispatched") {
            ev.status = "Pending".to_string();
        }
        Ok(())
    }
}

impl TimerRepo for MemoryRepo {
    fn get_by_id(&self, id: &str) -> Option<TimerRecord> {
        self.timers.read().unwrap().get(id).cloned()
    }

    fn mark_fired(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self.timers.write().unwrap().get_mut(id) {
            Some(r) => {
                r.status = "Fired".to_string();
                Ok(())
            }
            None => Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "timer not found",
            ))),
        }
    }

    fn insert(&self, record: &TimerRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.timers
            .write()
            .unwrap()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    fn list_due(&self, now_iso: &str, limit: u32) -> Result<Vec<TimerRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let limit = limit.min(100) as usize;
        let timers = self.timers.read().unwrap();
        let mut out: Vec<_> = timers
            .values()
            .filter(|r| r.status == "Scheduled" && r.due_at.as_str() <= now_iso)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.due_at.cmp(&b.due_at));
        out.truncate(limit);
        Ok(out)
    }
}

impl ParallelJoinRepo for MemoryRepo {
    fn ensure_group(
        &self,
        group_id: &str,
        expected: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.parallel_join
            .write()
            .unwrap()
            .entry(group_id.to_string())
            .or_insert((expected, 0, false));
        Ok(())
    }

    fn try_join(&self, group_id: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
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

impl CompensationRecordRepo for MemoryRepo {
    fn add(
        &self,
        record: &CompensationRecordRow,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.compensation.write().unwrap().push(record.clone());
        Ok(())
    }

    fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        let mut out: Vec<_> = self
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

impl ProcessInstanceRepo for std::sync::Arc<MemoryRepo> {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        (**self).load(id)
    }
    fn save(&self, instance: &ProcessInstance) {
        (**self).save(instance)
    }
    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        (**self).list_running(tenant_id)
    }
}

impl TokenRepo for std::sync::Arc<MemoryRepo> {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        (**self).load_by_instance(instance_id)
    }
    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        (**self).save_tokens(instance_id, tokens)
    }
    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        (**self).update_token_cas(instance_id, token)
    }
    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        (**self).claim_token(instance_id, token_id, version)
    }
}

impl OutboxRepo for std::sync::Arc<MemoryRepo> {
    fn insert_pending(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        (**self).insert_pending(tenant_id, event_type, payload)
    }
    fn list_pending(&self, tenant_id: Option<&str>) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        (**self).list_pending(tenant_id)
    }
    fn mark_published(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).mark_published(id)
    }
    fn claim_pending(
        &self,
        worker_id: &str,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        (**self).claim_pending(worker_id, tenant_id, limit)
    }
    fn release_claimed(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).release_claimed(id)
    }
}

impl TimerRepo for std::sync::Arc<MemoryRepo> {
    fn get_by_id(&self, id: &str) -> Option<TimerRecord> {
        (**self).get_by_id(id)
    }
    fn mark_fired(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).mark_fired(id)
    }
    fn insert(&self, record: &TimerRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).insert(record)
    }
    fn list_due(&self, now_iso: &str, limit: u32) -> Result<Vec<TimerRecord>, Box<dyn std::error::Error + Send + Sync>> {
        (**self).list_due(now_iso, limit)
    }
}

impl ParallelJoinRepo for std::sync::Arc<MemoryRepo> {
    fn ensure_group(
        &self,
        group_id: &str,
        expected: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).ensure_group(group_id, expected)
    }
    fn try_join(&self, group_id: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        (**self).try_join(group_id)
    }
}

impl CompensationRecordRepo for std::sync::Arc<MemoryRepo> {
    fn add(
        &self,
        record: &CompensationRecordRow,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        (**self).add(record)
    }
    fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        (**self).list_by_instance(instance_id)
    }
}

impl TransactionScope for std::sync::Arc<MemoryRepo> {
    fn with_tx<'r, F, R>(
        &'r self,
        f: F,
    ) -> std::result::Result<R, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce(
            Box<dyn ProcessInstanceRepo + 'r>,
            Box<dyn TokenRepo + 'r>,
        ) -> R,
    {
        let process_repo: Box<dyn ProcessInstanceRepo + 'r> =
            Box::new(std::sync::Arc::clone(self));
        let token_repo: Box<dyn TokenRepo + 'r> = Box::new(std::sync::Arc::clone(self));
        Ok(f(process_repo, token_repo))
    }
}
