use async_trait::async_trait;
use bpm_engine_storage::{OutboxEvent, OutboxRepo};
use std::sync::RwLock;

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

pub struct MemoryOutboxStore {
    events: RwLock<Vec<OutboxEvent>>,
}

impl MemoryOutboxStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }
}

impl Default for MemoryOutboxStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OutboxRepo for MemoryOutboxStore {
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
        self.events.write().unwrap().push(ev);
        Ok(id)
    }

    async fn list_pending(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<OutboxEvent>> {
        let out: Vec<OutboxEvent> = self
            .events
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
            .events
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
        let mut events = self.events.write().unwrap();
        let mut claimed = vec![];
        let limit = limit.min(100) as usize;
        for ev in events.iter_mut() {
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
            .events
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
