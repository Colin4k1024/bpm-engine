use async_trait::async_trait;
use bpm_storage::{TimerRecord, TimerRepo};
use std::collections::HashMap;
use std::sync::RwLock;

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

pub struct MemoryTimerStore {
    timers: RwLock<HashMap<String, TimerRecord>>,
}

impl MemoryTimerStore {
    pub fn new() -> Self {
        Self {
            timers: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryTimerStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TimerRepo for MemoryTimerStore {
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
