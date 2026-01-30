//! SQLite implementation of repos (migrated from db.rs).
//! Whitepaper §4: state, token status/mode/version/updated_at.

use crate::model::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use rusqlite::{params, Connection, Result};
use std::collections::HashMap;
use std::sync::Arc;

use super::repo::{CompensationRecordRepo, CompensationRecordRow, LeaderLeaseRepo, OutboxEvent, OutboxRepo, ParallelJoinRepo, ProcessInstanceRepo, TimerRecord, TimerRepo, TokenRepo, TransactionScope};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS process_instances (
    id TEXT PRIMARY KEY,
    process_def_id TEXT NOT NULL,
    tenant_id TEXT,
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
CREATE INDEX IF NOT EXISTS idx_tokens_status_updated_at ON tokens(status, updated_at);
CREATE INDEX IF NOT EXISTS idx_tokens_parallel_group_id ON tokens(parallel_group_id);

-- Whitepaper §11.6 + docs_database_schema §5: Event Outbox. v3: tenant_id, worker_id, claimed_at for distributed claim.
CREATE TABLE IF NOT EXISTS outbox_event (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL DEFAULT '',
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    tenant_id TEXT,
    worker_id TEXT,
    claimed_at TEXT,
    created_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_outbox_status ON outbox_event(status);

-- docs_database_schema §6: timer (id, token_id, instance_id, due_at, status, created_at).
CREATE TABLE IF NOT EXISTS timer (
    id TEXT PRIMARY KEY,
    token_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    due_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Scheduled',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_timer_due_at ON timer(due_at);
CREATE INDEX IF NOT EXISTS idx_timer_status ON timer(status);

-- docs_database_schema §7: compensation_record (id, instance_id, node_id, handler_ref, order, status, created_at).
CREATE TABLE IF NOT EXISTS compensation_record (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    handler_ref TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_compensation_instance_id ON compensation_record(instance_id);

-- Whitepaper §11.7: parallel join (group_id unique, expected, arrived_count, joined).
CREATE TABLE IF NOT EXISTS parallel_join (
    group_id TEXT PRIMARY KEY,
    expected INTEGER NOT NULL,
    arrived_count INTEGER NOT NULL DEFAULT 0,
    joined INTEGER NOT NULL DEFAULT 0
);

-- v3: Leader lease (role, worker_id, expires_at) for outbox_publisher / timer_poller.
CREATE TABLE IF NOT EXISTS leader_lease (
    role TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
";

fn migrate_add_columns(conn: &Connection) -> Result<()> {
    // Add state to process_instances if missing (old DBs)
    let has_state = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('process_instances') WHERE name='state'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_state.is_none() {
        conn.execute("ALTER TABLE process_instances ADD COLUMN state TEXT NOT NULL DEFAULT 'Running'", [])?;
        conn.execute("UPDATE process_instances SET state = 'Completed' WHERE completed = 1", [])?;
    }
    // docs_database_schema §2: process_instance.version
    let has_version = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('process_instances') WHERE name='version'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_version.is_none() {
        conn.execute("ALTER TABLE process_instances ADD COLUMN version INTEGER NOT NULL DEFAULT 0", [])?;
    }
    // Add token columns if missing
    let has_status = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('tokens') WHERE name='status'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_status.is_none() {
        conn.execute("ALTER TABLE tokens ADD COLUMN status TEXT NOT NULL DEFAULT 'Ready'", [])?;
        conn.execute("ALTER TABLE tokens ADD COLUMN mode TEXT NOT NULL DEFAULT 'Forward'", [])?;
        conn.execute("ALTER TABLE tokens ADD COLUMN version INTEGER NOT NULL DEFAULT 0", [])?;
        conn.execute("ALTER TABLE tokens ADD COLUMN updated_at TEXT", [])?;
        conn.execute(
            "UPDATE tokens SET status = 'Waiting', version = 0 WHERE waiting = 1",
            [],
        )?;
        conn.execute("UPDATE tokens SET status = 'Ready' WHERE waiting = 0", [])?;
    }
    // docs_database_schema §3: token.attempt
    let has_attempt = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('tokens') WHERE name='attempt'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_attempt.is_none() {
        conn.execute("ALTER TABLE tokens ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0", [])?;
    }
    // docs_database_schema §5: event_outbox.event_type
    let has_event_type = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('outbox_event') WHERE name='event_type'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_event_type.is_none() {
        conn.execute("ALTER TABLE outbox_event ADD COLUMN event_type TEXT NOT NULL DEFAULT ''", [])?;
    }
    // v3: tenant_id for multi-tenancy
    let has_tenant = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('process_instances') WHERE name='tenant_id'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_tenant.is_none() {
        conn.execute("ALTER TABLE process_instances ADD COLUMN tenant_id TEXT", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_process_instances_tenant_state ON process_instances(tenant_id, state)", [])?;
    }
    let has_outbox_tenant = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('outbox_event') WHERE name='tenant_id'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?;
    if has_outbox_tenant.is_none() {
        conn.execute("ALTER TABLE outbox_event ADD COLUMN tenant_id TEXT", [])?;
        conn.execute("ALTER TABLE outbox_event ADD COLUMN worker_id TEXT", [])?;
        conn.execute("ALTER TABLE outbox_event ADD COLUMN claimed_at TEXT", [])?;
        conn.execute("CREATE INDEX IF NOT EXISTS idx_outbox_tenant_status ON outbox_event(tenant_id, status)", [])?;
    }
    // v3: leader_lease table (may already exist from SCHEMA)
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS leader_lease (role TEXT PRIMARY KEY, worker_id TEXT NOT NULL, expires_at TEXT NOT NULL)",
        [],
    );
    // Indexes for recovery (docs_database_schema §3)
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_tokens_status_updated_at ON tokens(status, updated_at)", []);
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_tokens_parallel_group_id ON tokens(parallel_group_id)", []);
    Ok(())
}

trait OptionalResult {
    fn optional(self) -> Result<Option<i32>>;
}
impl OptionalResult for Result<i32, rusqlite::Error> {
    fn optional(self) -> Result<Option<i32>> {
        match self {
            Ok(x) => Ok(Some(x)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

pub struct InstanceRepo {
    conn: Connection,
}

fn state_to_str(s: InstanceState) -> &'static str {
    match s {
        InstanceState::Running => "Running",
        InstanceState::Completed => "Completed",
        InstanceState::Terminated => "Terminated",
    }
}

fn str_to_state(s: &str) -> InstanceState {
    match s {
        "Completed" => InstanceState::Completed,
        "Terminated" => InstanceState::Terminated,
        _ => InstanceState::Running,
    }
}

fn token_status_to_str(s: TokenStatus) -> &'static str {
    match s {
        TokenStatus::Created => "Created",
        TokenStatus::Ready => "Ready",
        TokenStatus::Executing => "Executing",
        TokenStatus::Waiting => "Waiting",
        TokenStatus::Suspended => "Suspended",
        TokenStatus::Completed => "Completed",
        TokenStatus::Terminated => "Terminated",
    }
}

fn str_to_token_status(s: &str) -> TokenStatus {
    match s {
        "Created" => TokenStatus::Created,
        "Executing" => TokenStatus::Executing,
        "Waiting" => TokenStatus::Waiting,
        "Suspended" => TokenStatus::Suspended,
        "Completed" => TokenStatus::Completed,
        "Terminated" => TokenStatus::Terminated,
        _ => TokenStatus::Ready,
    }
}

fn token_mode_to_str(m: TokenMode) -> &'static str {
    match m {
        TokenMode::Forward => "Forward",
        TokenMode::Compensation => "Compensation",
    }
}

fn str_to_token_mode(s: &str) -> TokenMode {
    match s {
        "Compensation" => TokenMode::Compensation,
        _ => TokenMode::Forward,
    }
}

impl InstanceRepo {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA)?;
        migrate_add_columns(&conn)?;
        Ok(Self { conn })
    }

    pub fn insert_instance(&self, instance: &ProcessInstance, process_def_id: &str) -> Result<()> {
        let variables_json =
            serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
        let created_at = chrono_utc_now();
        let tenant_id: Option<&str> = instance.tenant_id.as_deref();
        self.conn.execute(
            "INSERT INTO process_instances (id, process_def_id, tenant_id, variables, completed, state, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                instance.id,
                process_def_id,
                tenant_id,
                variables_json,
                if instance.completed() { 1 } else { 0 },
                state_to_str(instance.state),
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
                token_status_to_str(token.status),
                token_mode_to_str(token.mode),
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
            "SELECT id, process_def_id, tenant_id, variables, completed, state, version FROM process_instances WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };
        let id_val: String = row.get(0)?;
        let process_def_id: String = row.get(1)?;
        let tenant_id: Option<String> = row.get(2).ok();
        let variables_json: String = row.get(3)?;
        let variables: HashMap<String, String> =
            serde_json::from_str(&variables_json).unwrap_or_else(|_| HashMap::new());
        let _completed: i32 = row.get(4)?;
        let state_str: String = row.get(5).unwrap_or_else(|_| "Running".to_string());
        let state = str_to_state(&state_str);
        let version: u32 = row.get(6).unwrap_or(0);
        let tokens = self.get_tokens(id)?;
        Ok(Some(ProcessInstance {
            id: id_val,
            process_def_id,
            tenant_id,
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
            let _waiting: i32 = row.get(2)?;
            let status_str: String = row.get(3).unwrap_or_else(|_| "Ready".to_string());
            let mode_str: String = row.get(4).unwrap_or_else(|_| "Forward".to_string());
            let version: u32 = row.get(5).unwrap_or(0);
            let attempt: u32 = row.get(6).unwrap_or(0);
            let pg: Option<String> = row.get(7).ok();
            let updated_at: Option<String> = row.get(8).ok();
            Ok(Token {
                id,
                node_id,
                status: str_to_token_status(&status_str),
                mode: str_to_token_mode(&mode_str),
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

    /// Whitepaper §11.3: update instance row and tokens using CAS for existing tokens.
    pub fn update_instance(&self, instance: &ProcessInstance) -> Result<()> {
        let variables_json =
            serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
        let tenant_id: Option<&str> = instance.tenant_id.as_deref();
        self.conn.execute(
            "UPDATE process_instances SET variables = ?1, completed = ?2, state = ?3, version = ?4, tenant_id = ?5 WHERE id = ?6",
            params![
                variables_json,
                if instance.completed() { 1 } else { 0 },
                state_to_str(instance.state),
                instance.version,
                tenant_id,
                instance.id,
            ],
        )?;
        let current = self.get_tokens(&instance.id)?;
        let current_ids: std::collections::HashSet<_> = current.iter().map(|t| t.id.as_str()).collect();
        let instance_ids: std::collections::HashSet<_> = instance.tokens.iter().map(|t| t.id.as_str()).collect();
        for token in &instance.tokens {
            if current_ids.contains(token.id.as_str()) {
                let ok = self.update_token_cas(&instance.id, token);
                if !ok {
                    return Err(rusqlite::Error::ExecuteReturnedResults);
                }
            } else {
                self.insert_token(&instance.id, token)?;
            }
        }
        for id in current_ids.difference(&instance_ids) {
            self.conn.execute("DELETE FROM tokens WHERE instance_id = ?1 AND id = ?2", params![instance.id, id])?;
        }
        Ok(())
    }
}

impl ProcessInstanceRepo for InstanceRepo {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        self.get_instance(id).ok().flatten()
    }

    fn save(&self, instance: &ProcessInstance) {
        if self.get_instance(&instance.id).ok().flatten().is_some() {
            let _ = self.update_instance(instance);
        } else {
            let _ = self.insert_instance(instance, &instance.process_def_id);
        }
    }

    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        let rows: Vec<String> = match tenant_id {
            Some(t) => {
                let mut stmt = self.conn
                    .prepare("SELECT id FROM process_instances WHERE state = 'Running' AND (tenant_id = ?1 OR (tenant_id IS NULL AND ?1 = ''))")
                    .unwrap();
                stmt.query_map([t], |row| row.get::<_, String>(0)).unwrap().filter_map(Result::ok).collect()
            }
            None => {
                let mut stmt = self.conn
                    .prepare("SELECT id FROM process_instances WHERE state = 'Running'")
                    .unwrap();
                stmt.query_map([], |row| row.get::<_, String>(0)).unwrap().filter_map(Result::ok).collect()
            }
        };
        rows
    }
}

impl InstanceRepo {
    /// Public so Arc<InstanceRepo> can delegate TokenRepo.
    pub fn save_tokens_impl(&self, instance_id: &str, tokens: &[Token]) {
        let _ = self
            .conn
            .execute("DELETE FROM tokens WHERE instance_id = ?1", params![instance_id]);
        for token in tokens {
            let _ = self.insert_token(instance_id, token);
        }
    }
}

impl TokenRepo for InstanceRepo {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        self.get_tokens(instance_id).unwrap_or_default()
    }

    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        self.save_tokens_impl(instance_id, tokens);
    }

    /// Whitepaper §11.3 + docs_recovery §5.3: UPDATE token SET status=?, mode=?, attempt=?, version=version+1, updated_at=? WHERE instance_id=? AND id=? AND version=?.
    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        let updated_at = token.updated_at.clone().unwrap_or_else(|| chrono_utc_now());
        let n = self
            .conn
            .execute(
                "UPDATE tokens SET status = ?1, mode = ?2, attempt = ?3, version = version + 1, updated_at = ?4 WHERE instance_id = ?5 AND id = ?6 AND version = ?7",
                params![
                    token_status_to_str(token.status),
                    token_mode_to_str(token.mode),
                    token.attempt,
                    updated_at,
                    instance_id,
                    token.id,
                    token.version,
                ],
            )
            .unwrap_or(0);
        n == 1
    }

    /// Whitepaper §11.4: Claim Ready -> Executing. UPDATE ... WHERE status='Ready' AND version=?.
    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        let updated_at = chrono_utc_now();
        let n = self
            .conn
            .execute(
                "UPDATE tokens SET status = 'Executing', version = version + 1, updated_at = ?1 WHERE instance_id = ?2 AND id = ?3 AND status = 'Ready' AND version = ?4",
                params![updated_at, instance_id, token_id, version],
            )
            .unwrap_or(0);
        n == 1
    }
}

fn chrono_utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

// --- Connection-scoped helpers for use by both InstanceRepo and InstanceRepoTx ---

fn get_tokens_conn(conn: &Connection, instance_id: &str) -> Result<Vec<Token>> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, waiting, status, mode, version, attempt, parallel_group_id, updated_at FROM tokens WHERE instance_id = ?1",
    )?;
    let rows = stmt.query_map(params![instance_id], |row| {
        let id: String = row.get(0)?;
        let node_id: String = row.get(1)?;
        let _waiting: i32 = row.get(2)?;
        let status_str: String = row.get(3).unwrap_or_else(|_| "Ready".to_string());
        let mode_str: String = row.get(4).unwrap_or_else(|_| "Forward".to_string());
        let version: u32 = row.get(5).unwrap_or(0);
        let attempt: u32 = row.get(6).unwrap_or(0);
        let pg: Option<String> = row.get(7).ok();
        let updated_at: Option<String> = row.get(8).ok();
        Ok(Token {
            id,
            node_id,
            status: str_to_token_status(&status_str),
            mode: str_to_token_mode(&mode_str),
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

fn get_instance_conn(conn: &Connection, id: &str) -> Result<Option<ProcessInstance>> {
    let mut stmt = conn.prepare(
        "SELECT id, process_def_id, variables, completed, state, version, tenant_id FROM process_instances WHERE id = ?1",
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
    let state = str_to_state(&state_str);
    let version: u32 = row.get(5).unwrap_or(0);
    let tenant_id: Option<String> = row.get(6).ok();
    let tokens = get_tokens_conn(conn, id)?;
    Ok(Some(ProcessInstance {
        id: id_val,
        process_def_id,
        tenant_id,
        tokens,
        variables,
        state,
        version,
    }))
}

fn insert_token_conn(conn: &Connection, instance_id: &str, token: &Token) -> Result<()> {
    let id = if token.id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        token.id.clone()
    };
    let pg = token.parallel_group_id.as_deref().unwrap_or("");
    let updated_at = token.updated_at.as_deref().unwrap_or("");
    conn.execute(
        "INSERT INTO tokens (id, instance_id, node_id, waiting, status, mode, version, attempt, parallel_group_id, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            instance_id,
            token.node_id,
            if token.waiting() { 1 } else { 0 },
            token_status_to_str(token.status),
            token_mode_to_str(token.mode),
            token.version,
            token.attempt,
            pg,
            updated_at,
        ],
    )?;
    Ok(())
}

fn update_token_cas_conn(conn: &Connection, instance_id: &str, token: &Token) -> bool {
    let updated_at = token.updated_at.clone().unwrap_or_else(|| chrono_utc_now());
    let n = conn
        .execute(
            "UPDATE tokens SET status = ?1, mode = ?2, attempt = ?3, version = version + 1, updated_at = ?4 WHERE instance_id = ?5 AND id = ?6 AND version = ?7",
            params![
                token_status_to_str(token.status),
                token_mode_to_str(token.mode),
                token.attempt,
                updated_at,
                instance_id,
                token.id,
                token.version,
            ],
        )
        .unwrap_or(0);
    n == 1
}

fn update_instance_conn(conn: &Connection, instance: &ProcessInstance) -> Result<()> {
    let variables_json =
        serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
    let tenant_id: Option<&str> = instance.tenant_id.as_deref();
    conn.execute(
        "UPDATE process_instances SET variables = ?1, completed = ?2, state = ?3, version = ?4, tenant_id = ?5 WHERE id = ?6",
        params![
            variables_json,
            if instance.completed() { 1 } else { 0 },
            state_to_str(instance.state),
            instance.version,
            tenant_id,
            instance.id,
        ],
    )?;
    let current = get_tokens_conn(conn, &instance.id)?;
    let current_ids: std::collections::HashSet<_> = current.iter().map(|t| t.id.as_str()).collect();
    let instance_ids: std::collections::HashSet<_> = instance.tokens.iter().map(|t| t.id.as_str()).collect();
    for token in &instance.tokens {
        if current_ids.contains(token.id.as_str()) {
            if !update_token_cas_conn(conn, &instance.id, token) {
                return Err(rusqlite::Error::ExecuteReturnedResults);
            }
        } else {
            insert_token_conn(conn, &instance.id, token)?;
        }
    }
    for id in current_ids.difference(&instance_ids) {
        conn.execute("DELETE FROM tokens WHERE instance_id = ?1 AND id = ?2", params![instance.id, id])?;
    }
    Ok(())
}

fn save_tokens_impl_conn(conn: &Connection, instance_id: &str, tokens: &[Token]) {
    let _ = conn.execute("DELETE FROM tokens WHERE instance_id = ?1", params![instance_id]);
    for token in tokens {
        let _ = insert_token_conn(conn, instance_id, token);
    }
}

fn list_running_conn(conn: &Connection, tenant_id: Option<&str>) -> Vec<String> {
    let rows: Vec<String> = match tenant_id {
        Some(t) => {
            let mut stmt = conn
                .prepare("SELECT id FROM process_instances WHERE state = 'Running' AND (tenant_id = ?1 OR (tenant_id IS NULL AND ?1 = ''))")
                .unwrap();
            stmt.query_map([t], |row| row.get::<_, String>(0)).unwrap().filter_map(Result::ok).collect()
        }
        None => {
            let mut stmt = conn
                .prepare("SELECT id FROM process_instances WHERE state = 'Running'")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0)).unwrap().filter_map(Result::ok).collect()
        }
    };
    rows
}

fn claim_token_conn(conn: &Connection, instance_id: &str, token_id: &str, version: u32) -> bool {
    let updated_at = chrono_utc_now();
    let n = conn
        .execute(
            "UPDATE tokens SET status = 'Executing', version = version + 1, updated_at = ?1 WHERE instance_id = ?2 AND id = ?3 AND status = 'Ready' AND version = ?4",
            params![updated_at, instance_id, token_id, version],
        )
        .unwrap_or(0);
    n == 1
}

/// Transactional repo wrapper: uses the same Connection for one transaction (BEGIN/COMMIT/ROLLBACK).
struct InstanceRepoTx<'a> {
    conn: &'a Connection,
}

impl ProcessInstanceRepo for InstanceRepoTx<'_> {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        get_instance_conn(self.conn, id).ok().flatten()
    }

    fn save(&self, instance: &ProcessInstance) {
        if get_instance_conn(self.conn, &instance.id).ok().flatten().is_some() {
            let _ = update_instance_conn(self.conn, instance);
        } else {
            let variables_json =
                serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
            let created_at = chrono_utc_now();
            let tenant_id: Option<&str> = instance.tenant_id.as_deref();
            let _ = self.conn.execute(
                "INSERT INTO process_instances (id, process_def_id, tenant_id, variables, completed, state, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    instance.id,
                    &instance.process_def_id,
                    tenant_id,
                    variables_json,
                    if instance.completed() { 1 } else { 0 },
                    state_to_str(instance.state),
                    instance.version,
                    created_at,
                ],
            );
            for token in &instance.tokens {
                let _ = insert_token_conn(self.conn, &instance.id, token);
            }
        }
    }

    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        list_running_conn(self.conn, tenant_id)
    }
}

impl TokenRepo for InstanceRepoTx<'_> {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        get_tokens_conn(self.conn, instance_id).unwrap_or_default()
    }

    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        save_tokens_impl_conn(self.conn, instance_id, tokens);
    }

    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        update_token_cas_conn(self.conn, instance_id, token)
    }

    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        claim_token_conn(self.conn, instance_id, token_id, version)
    }
}

impl ProcessInstanceRepo for Arc<InstanceRepoTx<'_>> {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        get_instance_conn(self.conn, id).ok().flatten()
    }

    fn save(&self, instance: &ProcessInstance) {
        if get_instance_conn(self.conn, &instance.id).ok().flatten().is_some() {
            let _ = update_instance_conn(self.conn, instance);
        } else {
            let variables_json =
                serde_json::to_string(&instance.variables).unwrap_or_else(|_| "{}".to_string());
            let created_at = chrono_utc_now();
            let tenant_id: Option<&str> = instance.tenant_id.as_deref();
            let _ = self.conn.execute(
                "INSERT INTO process_instances (id, process_def_id, tenant_id, variables, completed, state, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    instance.id,
                    &instance.process_def_id,
                    tenant_id,
                    variables_json,
                    if instance.completed() { 1 } else { 0 },
                    state_to_str(instance.state),
                    instance.version,
                    created_at,
                ],
            );
            for token in &instance.tokens {
                let _ = insert_token_conn(self.conn, &instance.id, token);
            }
        }
    }

    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        list_running_conn(self.conn, tenant_id)
    }
}

impl TokenRepo for Arc<InstanceRepoTx<'_>> {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        get_tokens_conn(self.conn, instance_id).unwrap_or_default()
    }

    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        save_tokens_impl_conn(self.conn, instance_id, tokens);
    }

    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        update_token_cas_conn(self.conn, instance_id, token)
    }

    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        claim_token_conn(self.conn, instance_id, token_id, version)
    }
}

impl ProcessInstanceRepo for Arc<InstanceRepo> {
    fn load(&self, id: &str) -> Option<ProcessInstance> {
        self.as_ref().get_instance(id).ok().flatten()
    }

    fn save(&self, instance: &ProcessInstance) {
        if self.as_ref().get_instance(&instance.id).ok().flatten().is_some() {
            let _ = self.as_ref().update_instance(instance);
        } else {
            let _ = self.as_ref().insert_instance(instance, &instance.process_def_id);
        }
    }

    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String> {
        self.as_ref().list_running(tenant_id)
    }
}

impl TokenRepo for Arc<InstanceRepo> {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token> {
        self.as_ref()
            .get_instance(instance_id)
            .ok()
            .flatten()
            .map(|i| i.tokens)
            .unwrap_or_default()
    }

    fn save_tokens(&self, instance_id: &str, tokens: &[Token]) {
        self.as_ref().save_tokens_impl(instance_id, tokens);
    }

    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool {
        self.as_ref().update_token_cas(instance_id, token)
    }

    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool {
        self.as_ref().claim_token(instance_id, token_id, version)
    }
}

impl TransactionScope for Arc<InstanceRepo> {
    /// Whitepaper §11.5: one Handler = one transaction. BEGIN IMMEDIATE; run f with transactional repos; COMMIT or ROLLBACK on panic.
    fn with_tx<'r, F, R>(&'r self, f: F) -> std::result::Result<R, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce(Box<dyn ProcessInstanceRepo + 'r>, Box<dyn TokenRepo + 'r>) -> R,
    {
        let conn = &self.as_ref().conn;
        conn.execute("BEGIN IMMEDIATE", [])?;
        let tx = InstanceRepoTx { conn };
        let arc = Arc::new(tx);
        let process_repo: Box<dyn ProcessInstanceRepo + 'r> = Box::new(Arc::clone(&arc));
        let token_repo: Box<dyn TokenRepo + 'r> = Box::new(arc);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(process_repo, token_repo)));
        match result {
            Ok(r) => {
                conn.execute("COMMIT", [])?;
                Ok(r)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                std::panic::resume_unwind(e)
            }
        }
    }
}

// --- Whitepaper §11.7: parallel join ---

impl ParallelJoinRepo for InstanceRepo {
    fn ensure_group(&self, group_id: &str, expected: u32) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.conn.execute(
            "INSERT OR IGNORE INTO parallel_join (group_id, expected, arrived_count, joined) VALUES (?1, ?2, 0, 0)",
            params![group_id, expected],
        )?;
        Ok(())
    }

    fn try_join(&self, group_id: &str) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        self.conn.execute(
            "UPDATE parallel_join SET arrived_count = arrived_count + 1 WHERE group_id = ?1",
            params![group_id],
        )?;
        let n = self.conn.execute(
            "UPDATE parallel_join SET joined = 1 WHERE group_id = ?1 AND joined = 0 AND arrived_count >= expected",
            params![group_id],
        )?;
        Ok(n == 1)
    }
}

impl ParallelJoinRepo for Arc<InstanceRepo> {
    fn ensure_group(&self, group_id: &str, expected: u32) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().ensure_group(group_id, expected)
    }

    fn try_join(&self, group_id: &str) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().try_join(group_id)
    }
}

// --- Whitepaper §11.6: Event Outbox. v3: tenant_id, claim_pending, release_claimed ---

fn outbox_event_from_row(row: &rusqlite::Row) -> rusqlite::Result<OutboxEvent> {
    Ok(OutboxEvent {
        id: row.get(0)?,
        event_type: row.get(1).unwrap_or_else(|_| String::new()),
        payload: row.get(2)?,
        status: row.get(3)?,
        tenant_id: row.get(4).ok(),
        created_at: row.get(5).ok(),
    })
}

impl OutboxRepo for InstanceRepo {
    fn insert_pending(&self, tenant_id: Option<&str>, event_type: &str, payload: &str) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono_utc_now();
        self.conn.execute(
            "INSERT INTO outbox_event (id, event_type, payload, status, tenant_id, created_at) VALUES (?1, ?2, ?3, 'Pending', ?4, ?5)",
            params![id, event_type, payload, tenant_id, created_at],
        )?;
        Ok(id)
    }

    fn list_pending(&self, tenant_id: Option<&str>) -> std::result::Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let mut out = vec![];
        match tenant_id {
            Some(t) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, event_type, payload, status, tenant_id, created_at FROM outbox_event WHERE status = 'Pending' AND (tenant_id = ?1 OR (tenant_id IS NULL AND ?1 = '')) ORDER BY created_at",
                )?;
                let rows = stmt.query_map(params![t], outbox_event_from_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, event_type, payload, status, tenant_id, created_at FROM outbox_event WHERE status = 'Pending' ORDER BY created_at",
                )?;
                let rows = stmt.query_map([], outbox_event_from_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    fn mark_published(&self, id: &str) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.conn.execute("UPDATE outbox_event SET status = 'Published' WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn claim_pending(&self, worker_id: &str, tenant_id: Option<&str>, limit: u32) -> std::result::Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let claimed_at = chrono_utc_now();
        let limit = limit.min(100) as i32;
        for _ in 0..limit {
            let updated = match tenant_id {
                Some(t) => self.conn.execute(
                    "UPDATE outbox_event SET status = 'Dispatched', worker_id = ?1, claimed_at = ?2 WHERE id = (SELECT id FROM outbox_event WHERE status = 'Pending' AND (tenant_id = ?3 OR (tenant_id IS NULL AND ?3 = '')) ORDER BY created_at LIMIT 1)",
                    params![worker_id, claimed_at, t],
                )?,
                None => self.conn.execute(
                    "UPDATE outbox_event SET status = 'Dispatched', worker_id = ?1, claimed_at = ?2 WHERE id = (SELECT id FROM outbox_event WHERE status = 'Pending' ORDER BY created_at LIMIT 1)",
                    params![worker_id, claimed_at],
                )?,
            };
            if updated == 0 {
                break;
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, payload, status, tenant_id, created_at FROM outbox_event WHERE worker_id = ?1 AND status = 'Dispatched' AND claimed_at = ?2 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![worker_id, claimed_at], outbox_event_from_row)?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn release_claimed(&self, id: &str) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.conn.execute(
            "UPDATE outbox_event SET status = 'Pending', worker_id = NULL, claimed_at = NULL WHERE id = ?1 AND status = 'Dispatched'",
            params![id],
        )?;
        Ok(())
    }
}

// --- v3: Leader lease (LeaderLeaseRepo) ---

impl LeaderLeaseRepo for InstanceRepo {
    fn try_acquire(&self, role: &str, worker_id: &str, ttl_secs: u64) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let now_str = chrono_utc_now();
        let now_u: u64 = now_str.parse().unwrap_or(0);
        let expires = (now_u + ttl_secs).to_string();
        let n = self.conn.execute(
            "UPDATE leader_lease SET worker_id = ?1, expires_at = ?2 WHERE role = ?3 AND (expires_at < ?4 OR worker_id = ?5)",
            params![worker_id, expires, role, now_str, worker_id],
        )?;
        if n == 1 {
            return Ok(true);
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO leader_lease (role, worker_id, expires_at) VALUES (?1, ?2, ?3)",
            params![role, worker_id, expires],
        )?;
        Ok(self.conn.changes() == 1)
    }

    fn renew(&self, role: &str, worker_id: &str, ttl_secs: u64) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let now_str = chrono_utc_now();
        let now_u: u64 = now_str.parse().unwrap_or(0);
        let expires = (now_u + ttl_secs).to_string();
        let n = self.conn.execute(
            "UPDATE leader_lease SET expires_at = ?1 WHERE role = ?2 AND worker_id = ?3",
            params![expires, role, worker_id],
        )?;
        Ok(n == 1)
    }
}

impl LeaderLeaseRepo for Arc<InstanceRepo> {
    fn try_acquire(&self, role: &str, worker_id: &str, ttl_secs: u64) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().try_acquire(role, worker_id, ttl_secs)
    }
    fn renew(&self, role: &str, worker_id: &str, ttl_secs: u64) -> std::result::Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().renew(role, worker_id, ttl_secs)
    }
}

impl OutboxRepo for Arc<InstanceRepo> {
    fn insert_pending(&self, tenant_id: Option<&str>, event_type: &str, payload: &str) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().insert_pending(tenant_id, event_type, payload)
    }

    fn list_pending(&self, tenant_id: Option<&str>) -> std::result::Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().list_pending(tenant_id)
    }

    fn mark_published(&self, id: &str) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().mark_published(id)
    }

    fn claim_pending(&self, worker_id: &str, tenant_id: Option<&str>, limit: u32) -> std::result::Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().claim_pending(worker_id, tenant_id, limit)
    }

    fn release_claimed(&self, id: &str) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().release_claimed(id)
    }
}

// --- TimerRepo (docs_database_schema §6) ---

impl InstanceRepo {
    fn get_timer_by_id(&self, id: &str) -> Result<Option<TimerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, token_id, instance_id, due_at, status, created_at FROM timer WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(TimerRecord {
            id: row.get(0)?,
            token_id: row.get(1)?,
            instance_id: row.get(2)?,
            due_at: row.get(3)?,
            status: row.get(4)?,
            created_at: row.get(5)?,
        }))
    }

    fn mark_timer_fired(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE timer SET status = 'Fired' WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn insert_timer(&self, record: &TimerRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO timer (id, token_id, instance_id, due_at, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id,
                record.token_id,
                record.instance_id,
                record.due_at,
                record.status,
                record.created_at,
            ],
        )?;
        Ok(())
    }
}

impl TimerRepo for InstanceRepo {
    fn get_by_id(&self, id: &str) -> Option<TimerRecord> {
        self.get_timer_by_id(id).ok().flatten()
    }

    fn mark_fired(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.mark_timer_fired(id).map_err(Into::into)
    }

    fn insert(&self, record: &TimerRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.insert_timer(record).map_err(Into::into)
    }

    fn list_due(&self, now_iso: &str, limit: u32) -> Result<Vec<TimerRecord>, Box<dyn std::error::Error + Send + Sync>> {
        let limit = limit.min(100) as i32;
        let mut stmt = self.conn.prepare(
            "SELECT id, token_id, instance_id, due_at, status, created_at FROM timer WHERE status = 'Scheduled' AND due_at <= ?1 ORDER BY due_at LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now_iso, limit], |row| {
            Ok(TimerRecord {
                id: row.get(0)?,
                token_id: row.get(1)?,
                instance_id: row.get(2)?,
                due_at: row.get(3)?,
                status: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

impl TimerRepo for Arc<InstanceRepo> {
    fn get_by_id(&self, id: &str) -> Option<TimerRecord> {
        self.as_ref().get_by_id(id)
    }

    fn mark_fired(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().mark_fired(id)
    }

    fn insert(&self, record: &TimerRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().insert(record)
    }

    fn list_due(&self, now_iso: &str, limit: u32) -> Result<Vec<TimerRecord>, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().list_due(now_iso, limit)
    }
}

// --- CompensationRecordRepo (docs_database_schema §7) ---

impl InstanceRepo {
    fn get_compensation_records_by_instance(&self, instance_id: &str) -> Result<Vec<CompensationRecordRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, instance_id, node_id, handler_ref, sort_order, status, created_at FROM compensation_record WHERE instance_id = ?1 ORDER BY sort_order ASC",
        )?;
        let rows = stmt.query_map(params![instance_id], |row| {
            Ok(CompensationRecordRow {
                id: row.get(0)?,
                instance_id: row.get(1)?,
                node_id: row.get(2)?,
                handler_ref: row.get(3).unwrap_or_default(),
                order: row.get::<_, i64>(4)? as u32,
                status: row.get(5).unwrap_or_else(|_| "Pending".to_string()),
                created_at: row.get(6)?,
            })
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn insert_compensation_record(&self, record: &CompensationRecordRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO compensation_record (id, instance_id, node_id, handler_ref, sort_order, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id,
                record.instance_id,
                record.node_id,
                record.handler_ref,
                record.order,
                record.status,
                record.created_at,
            ],
        )?;
        Ok(())
    }

    fn update_compensation_record_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn.execute("UPDATE compensation_record SET status = ?1 WHERE id = ?2", params![status, id])?;
        Ok(())
    }
}

impl CompensationRecordRepo for InstanceRepo {
    fn add(&self, record: &CompensationRecordRow) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.insert_compensation_record(record).map_err(Into::into)
    }

    fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        self.get_compensation_records_by_instance(instance_id).unwrap_or_default()
    }
}

impl CompensationRecordRepo for Arc<InstanceRepo> {
    fn add(&self, record: &CompensationRecordRow) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().add(record)
    }

    fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        self.as_ref().list_by_instance(instance_id)
    }
}
