use crate::model::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use rusqlite::{params, Connection, Result};
use std::collections::HashMap;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS process_instances (
    id TEXT PRIMARY KEY,
    process_def_id TEXT NOT NULL,
    variables TEXT NOT NULL,
    completed INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'Running',
    version INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tokens (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    waiting INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'Ready',
    mode TEXT NOT NULL DEFAULT 'Forward',
    version INTEGER NOT NULL DEFAULT 0,
    attempt INTEGER NOT NULL DEFAULT 0,
    parallel_group_id TEXT,
    updated_at TEXT,
    FOREIGN KEY (instance_id) REFERENCES process_instances(id)
);

CREATE INDEX IF NOT EXISTS idx_tokens_instance_id ON tokens(instance_id);
";

pub struct InstanceRepo {
    conn: Connection,
}

impl InstanceRepo {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn insert_instance(&self, instance: &ProcessInstance, process_def_id: &str) -> Result<()> {
        let variables_json = serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
        let created_at = chrono_utc_now();
        self.conn.execute(
            "INSERT INTO process_instances (id, process_def_id, variables, completed, state, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                instance.id,
                process_def_id,
                variables_json,
                if instance.completed() { 1 } else { 0 },
                match instance.state {
                    InstanceState::Running => "Running",
                    InstanceState::Completed => "Completed",
                    InstanceState::Terminated => "Terminated",
                },
                instance.version,
                created_at,
            ],
        )?;
        for token in &instance.tokens {
            self.insert_token(&instance.id, token)?;
        }
        Ok(())
    }

    fn insert_token(&self, instance_id: &str, token: &Token) -> Result<()> {
        let id = if token.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            token.id.clone()
        };
        let pg = token.parallel_group_id.as_deref().unwrap_or("");
        let updated_at = token.updated_at.as_deref().unwrap_or("");
        self.conn.execute(
            "INSERT INTO tokens (id, instance_id, node_id, waiting, status, mode, version, attempt, parallel_group_id, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                instance_id,
                token.node_id,
                if token.waiting() { 1 } else { 0 },
                match token.status {
                    TokenStatus::Created => "Created",
                    TokenStatus::Ready => "Ready",
                    TokenStatus::Executing => "Executing",
                    TokenStatus::Waiting => "Waiting",
                    TokenStatus::Suspended => "Suspended",
                    TokenStatus::Completed => "Completed",
                    TokenStatus::Terminated => "Terminated",
                },
                match token.mode {
                    TokenMode::Forward => "Forward",
                    TokenMode::Compensation => "Compensation",
                },
                token.version,
                token.attempt,
                pg,
                updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_instance(&self, id: &str) -> Result<Option<ProcessInstance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, process_def_id, variables, completed, state, version FROM process_instances WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };
        let id_val: String = row.get(0)?;
        let process_def_id: String = row.get(1)?;
        let variables_json: String = row.get(2)?;
        let variables: HashMap<String, String> =
            serde_json::from_str(&variables_json).unwrap_or_else(|_| HashMap::new());
        let _completed: i32 = row.get(3)?;
        let state_str: String = row.get(4).unwrap_or_else(|_| "Running".to_string());
        let state = match state_str.as_str() {
            "Completed" => InstanceState::Completed,
            "Terminated" => InstanceState::Terminated,
            _ => InstanceState::Running,
        };
        let version: u32 = row.get(5).unwrap_or(0);
        let tokens = self.get_tokens(id)?;
        Ok(Some(ProcessInstance {
            id: id_val,
            process_def_id,
            tenant_id: None,
            tokens,
            variables,
            state,
            version,
        }))
    }

    fn get_tokens(&self, instance_id: &str) -> Result<Vec<Token>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, waiting, status, mode, version, attempt, parallel_group_id, updated_at FROM tokens WHERE instance_id = ?1",
        )?;
        let rows = stmt.query_map(params![instance_id], |row| {
            let id: String = row.get(0)?;
            let node_id: String = row.get(1)?;
            let _w: i32 = row.get(2)?;
            let status_str: String = row.get(3).unwrap_or_else(|_| "Ready".to_string());
            let mode_str: String = row.get(4).unwrap_or_else(|_| "Forward".to_string());
            let version: u32 = row.get(5).unwrap_or(0);
            let attempt: u32 = row.get(6).unwrap_or(0);
            let pg: Option<String> = row.get(7).ok();
            let updated_at: Option<String> = row.get(8).ok();
            let status = match status_str.as_str() {
                "Created" => TokenStatus::Created,
                "Executing" => TokenStatus::Executing,
                "Waiting" => TokenStatus::Waiting,
                "Suspended" => TokenStatus::Suspended,
                "Completed" => TokenStatus::Completed,
                "Terminated" => TokenStatus::Terminated,
                _ => TokenStatus::Ready,
            };
            let mode = match mode_str.as_str() {
                "Compensation" => TokenMode::Compensation,
                _ => TokenMode::Forward,
            };
            Ok(Token {
                id,
                node_id,
                status,
                mode,
                version,
                attempt,
                parallel_group_id: pg.filter(|s| !s.is_empty()),
                updated_at: updated_at.filter(|s| !s.is_empty()),
            })
        })?;
        let mut tokens = vec![];
        for t in rows {
            tokens.push(t?);
        }
        Ok(tokens)
    }

    pub fn update_instance(&self, instance: &ProcessInstance) -> Result<()> {
        let variables_json = serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
        self.conn.execute(
            "UPDATE process_instances SET variables = ?1, completed = ?2, state = ?3, version = ?4 WHERE id = ?5",
            params![
                variables_json,
                if instance.completed() { 1 } else { 0 },
                match instance.state {
                    InstanceState::Running => "Running",
                    InstanceState::Completed => "Completed",
                    InstanceState::Terminated => "Terminated",
                },
                instance.version,
                instance.id,
            ],
        )?;
        self.conn.execute("DELETE FROM tokens WHERE instance_id = ?1", params![instance.id])?;
        for token in &instance.tokens {
            self.insert_token(&instance.id, token)?;
        }
        Ok(())
    }
}

fn chrono_utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}
