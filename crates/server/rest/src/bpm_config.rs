//! Centralized configuration management.
//!
//! Priority: environment variables > config file > defaults.
//!
//! Config file path is determined by `BPM_CONFIG_PATH` (default `./bpm.toml`).
//! Missing config file is not an error — defaults are used.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tracing::info;

/// Top-level BPM engine configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BpmConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub engine: EngineConfig,
    pub log: LogConfig,
    pub rate_limit: RateLimitConfig,
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// Storage backend driver: `"memory"` (default) or `"postgres"`.
    pub driver: String,
    /// Connection URL for the postgres driver (ignored by the memory driver).
    pub url: String,
    /// Maximum number of postgres pool connections.
    pub max_connections: u32,
    /// Run pending SQL migrations automatically on startup (postgres driver).
    ///
    /// When `false`, the server validates the schema instead: startup fails
    /// if any migration is pending, so an operator must apply migrations out
    /// of band (or re-enable auto-migration). Deployment contract for
    /// environments where DDL is change-managed.
    pub migrate_on_start: bool,
}

/// Authentication configuration (E5-1).
///
/// The two credentials define three runtime modes (see `docs/rbac.md`):
///
/// - **JWT mode** — `jwt_secret` set (env `BPM_AUTH__JWT_SECRET`, synced to
///   `BPM_JWT_SECRET`): `/api/v1` requests must carry
///   `Authorization: Bearer <jwt>` (HS256, claims `{sub, tenant, role, exp}`);
///   identity, tenant scope, and role-based permissions are derived from the
///   VERIFIED claims. The client `X-Tenant-Id` header is deprecated in this
///   mode (a header conflicting with the claim is rejected with 403).
/// - **API-key mode** — `api_key` set (env `BPM_AUTH__API_KEY`, synced to
///   `BPM_API_KEY`): the legacy admin/bootstrap credential with FULL access
///   (no role checks); `X-Tenant-Id` is honored as untrusted input. When
///   BOTH are configured, a presented JWT takes precedence; without one,
///   the API key is the fallback.
/// - **Open mode** — neither set: every request is accepted (PoC) and
///   `X-Tenant-Id` remains a dev-mode affordance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// HS256 secret for JWT signing/verification (E5-1). Empty = JWT auth
    /// disabled.
    pub jwt_secret: String,
    /// Legacy admin API key (`X-API-Key`). Empty = the API-key layer is
    /// disabled.
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub timer_poll_interval_ms: u64,
    pub timer_batch_size: u32,
    /// Maximum byte length of a stored variable value (the final stored
    /// string, envelope included). Enforced at the REST write boundaries:
    /// start-instance, user-task complete, external-task complete (E2-3).
    pub variable_max_bytes: usize,
    /// Maximum byte length of a variable value recorded verbatim in event
    /// history; longer values are replaced with a `__truncated:` marker.
    pub history_variable_max_bytes: usize,
    /// Task types whose external tasks are approval-required (E5-2,
    /// ADR-008): the engine persists an approval record when a token
    /// arrives and creates the fetchable task only after an operator
    /// approves. Accepts a TOML array
    /// (`approval_required_task_types = ["payment_charge"]`) or a
    /// comma-separated string (env
    /// `BPM_ENGINE__APPROVAL_REQUIRED_TASK_TYPES=payment_charge,audit`).
    /// Default: empty (no approval gate — pre-E5-2 behavior).
    #[serde(deserialize_with = "comma_list_or_vec")]
    pub approval_required_task_types: Vec<String>,
}

/// Deserialize a `Vec<String>` that may arrive as EITHER a sequence (TOML
/// array) or a single comma-separated string (env var override): empty
/// segments are dropped, whitespace is trimmed.
fn comma_list_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list of task types or a comma-separated string")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect())
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while let Some(item) = seq.next_element::<String>()? {
                let item = item.trim().to_string();
                if !item.is_empty() {
                    out.push(item);
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_any(V)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub level: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub requests_per_minute: u64,
}

/// Audit & data-retention configuration (E5-3, issue #25).
///
/// Retention covers the **append-only audit records** — history events and
/// dead-letter entries — only. Instances, tokens, and process definitions
/// are OPERATIONAL state and are never deleted by the sweep (see
/// `docs/audit-and-retention.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuditConfig {
    /// Days to retain history events and dead-letter entries. `0` (the
    /// DEFAULT) disables retention entirely: nothing is ever deleted —
    /// the safe default, since deletion is irreversible and can break
    /// replay/crash-recovery for old instances.
    ///
    /// When set, a sweep runs at startup and then periodically: rows
    /// strictly older than `now - retention_days` are deleted (history by
    /// `occurred_at`, dead letters by `created_at`).
    pub retention_days: u64,
    /// Seconds between periodic retention sweeps (default 3600). Only
    /// meaningful when `retention_days > 0`. The startup sweep always
    /// runs once.
    pub sweep_interval_secs: u64,
}

// --- Defaults ---

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: "memory".to_string(),
            url: String::new(),
            max_connections: 10,
            migrate_on_start: true,
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            timer_poll_interval_ms: 1000,
            timer_batch_size: 100,
            variable_max_bytes: 65_536,
            history_variable_max_bytes: 1024,
            approval_required_task_types: Vec::new(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "json".to_string(),
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 100,
        }
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            retention_days: 0,
            sweep_interval_secs: 3600,
        }
    }
}

impl BpmConfig {
    /// Build the effective configuration.
    ///
    /// 1. Start from defaults.
    /// 2. Layer on values from the TOML config file (if it exists).
    /// 3. Layer on environment variable overrides.
    pub fn load() -> Self {
        let config_path =
            std::env::var("BPM_CONFIG_PATH").unwrap_or_else(|_| "./bpm.toml".to_string());

        // Build from file + env using the `config` crate.
        let mut builder = config::Config::builder()
            // Set defaults via a serialized source
            .add_source(config::Config::try_from(&BpmConfig::default()).unwrap());

        // Add TOML file if it exists (not an error if missing)
        if std::path::Path::new(&config_path).exists() {
            info!(path = %config_path, "loading config file");
            builder = builder.add_source(config::File::new(&config_path, config::FileFormat::Toml));
        } else {
            info!(path = %config_path, "config file not found, using defaults");
        }

        // Environment variable overrides: BPM_SERVER__PORT, BPM_LOG__LEVEL, etc.
        builder = builder.add_source(
            config::Environment::with_prefix("BPM")
                .separator("__")
                .try_parsing(true),
        );

        let cfg = builder.build().expect("failed to build configuration");

        cfg.try_deserialize::<BpmConfig>()
            .expect("failed to deserialize configuration")
    }

    /// Return a socket address for the server listener.
    pub fn server_addr(&self) -> SocketAddr {
        use std::net::IpAddr;
        let ip: IpAddr = self.server.host.parse().unwrap_or([0, 0, 0, 0].into());
        SocketAddr::new(ip, self.server.port)
    }

    /// Return a [`TimerSchedulerConfig`](bpm_engine_runtime::TimerSchedulerConfig)
    /// derived from engine settings.
    pub fn timer_scheduler_config(&self) -> bpm_engine_runtime::TimerSchedulerConfig {
        bpm_engine_runtime::TimerSchedulerConfig {
            poll_interval: std::time::Duration::from_millis(self.engine.timer_poll_interval_ms),
            batch_size: self.engine.timer_batch_size,
        }
    }

    /// Print effective configuration to the log, with secrets masked.
    pub fn log_effective(&self) {
        info!(
            server.host = %self.server.host,
            server.port = self.server.port,
            database.driver = %self.database.driver,
            database.url = %mask_secret(&self.database.url),
            database.max_connections = self.database.max_connections,
            database.migrate_on_start = self.database.migrate_on_start,
            auth.jwt_secret = %mask_secret(&self.auth.jwt_secret),
            auth.api_key = %mask_secret(&self.auth.api_key),
            engine.timer_poll_interval_ms = self.engine.timer_poll_interval_ms,
            engine.timer_batch_size = self.engine.timer_batch_size,
            engine.variable_max_bytes = self.engine.variable_max_bytes,
            engine.history_variable_max_bytes = self.engine.history_variable_max_bytes,
            engine.approval_required_task_types = ?self.engine.approval_required_task_types,
            log.level = %self.log.level,
            log.format = %self.log.format,
            rate_limit.requests_per_minute = self.rate_limit.requests_per_minute,
            audit.retention_days = self.audit.retention_days,
            audit.sweep_interval_secs = self.audit.sweep_interval_secs,
            "effective configuration"
        );
    }
}

/// Mask a secret value: show first 4 chars + `***` if non-empty, otherwise `<not set>`.
fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        "<not set>".to_string()
    } else if value.len() <= 4 {
        "***".to_string()
    } else {
        format!("{}***", &value[..4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = BpmConfig::default();
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.port, 3000);
        assert_eq!(cfg.database.driver, "memory");
        assert_eq!(cfg.database.max_connections, 10);
        assert!(cfg.database.migrate_on_start, "auto-migration defaults on");
        assert_eq!(cfg.engine.timer_poll_interval_ms, 1000);
        assert_eq!(cfg.engine.timer_batch_size, 100);
        assert_eq!(cfg.engine.variable_max_bytes, 65_536);
        assert_eq!(cfg.engine.history_variable_max_bytes, 1024);
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.log.format, "json");
        assert_eq!(cfg.rate_limit.requests_per_minute, 100);
        assert_eq!(cfg.auth.jwt_secret, String::new());
        assert_eq!(cfg.auth.api_key, String::new());
        assert!(
            cfg.engine.approval_required_task_types.is_empty(),
            "no task types require approval by default (pre-E5-2 behavior)"
        );
        // E5-3: retention is OFF by default (0 = keep everything).
        assert_eq!(cfg.audit.retention_days, 0);
        assert_eq!(cfg.audit.sweep_interval_secs, 3600);
    }

    #[test]
    fn audit_retention_toml_override_and_default_off() {
        // Defaults: retention disabled, hourly sweep.
        let cfg: BpmConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.audit.retention_days, 0);
        assert_eq!(cfg.audit.sweep_interval_secs, 3600);

        // TOML [audit] override.
        let toml_str = r#"
[audit]
retention_days = 30
sweep_interval_secs = 600
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.audit.retention_days, 30);
        assert_eq!(cfg.audit.sweep_interval_secs, 600);
        // Partial override keeps the other default.
        let cfg: BpmConfig = toml::from_str("[audit]\nretention_days = 7\n").unwrap();
        assert_eq!(cfg.audit.retention_days, 7);
        assert_eq!(cfg.audit.sweep_interval_secs, 3600);
        // Unset sections keep defaults.
        assert_eq!(cfg.rate_limit.requests_per_minute, 100);
    }

    #[test]
    fn engine_variable_caps_default_and_toml_override() {
        // Defaults
        let cfg: BpmConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.engine.variable_max_bytes, 65_536);
        assert_eq!(cfg.engine.history_variable_max_bytes, 1024);

        // TOML [engine] override
        let toml_str = r#"
[engine]
variable_max_bytes = 4096
history_variable_max_bytes = 256
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.engine.variable_max_bytes, 4096);
        assert_eq!(cfg.engine.history_variable_max_bytes, 256);
        // Unset engine fields keep defaults
        assert_eq!(cfg.engine.timer_poll_interval_ms, 1000);
    }

    #[test]
    fn engine_variable_caps_partial_toml_override() {
        let toml_str = r#"
[engine]
variable_max_bytes = 131072
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.engine.variable_max_bytes, 131_072);
        assert_eq!(cfg.engine.history_variable_max_bytes, 1024);
    }

    #[test]
    fn approval_required_task_types_toml_array_and_comma_string() {
        // TOML array form.
        let toml_str = r#"
[engine]
approval_required_task_types = ["payment_charge", "audit"]
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            cfg.engine.approval_required_task_types,
            vec!["payment_charge".to_string(), "audit".to_string()]
        );
        // Unset fields keep defaults.
        assert_eq!(cfg.engine.variable_max_bytes, 65_536);
    }

    #[test]
    fn approval_required_task_types_comma_string_form() {
        // Comma-separated string form (the env-var override shape),
        // including whitespace trimming and empty-segment dropping.
        let toml_str = r#"
[engine]
approval_required_task_types = "payment_charge, audit , ,payout"
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            cfg.engine.approval_required_task_types,
            vec![
                "payment_charge".to_string(),
                "audit".to_string(),
                "payout".to_string()
            ]
        );
    }

    #[test]
    fn approval_required_task_types_empty_forms() {
        // Empty string and empty array both yield no gate.
        let cfg: BpmConfig =
            toml::from_str("[engine]\napproval_required_task_types = \"\"\n").unwrap();
        assert!(cfg.engine.approval_required_task_types.is_empty());
        let cfg: BpmConfig =
            toml::from_str("[engine]\napproval_required_task_types = []\n").unwrap();
        assert!(cfg.engine.approval_required_task_types.is_empty());
    }

    #[test]
    fn mask_secret_masks_long_values() {
        assert_eq!(mask_secret("super-secret-key"), "supe***");
    }

    #[test]
    fn mask_secret_short_value() {
        assert_eq!(mask_secret("ab"), "***");
    }

    #[test]
    fn mask_secret_empty() {
        assert_eq!(mask_secret(""), "<not set>");
    }

    #[test]
    fn server_addr_parses_default() {
        let cfg = BpmConfig::default();
        let addr = cfg.server_addr();
        assert_eq!(addr.port(), 3000);
    }

    #[test]
    fn timer_scheduler_config_from_engine_settings() {
        let mut cfg = BpmConfig::default();
        cfg.engine.timer_poll_interval_ms = 500;
        cfg.engine.timer_batch_size = 50;
        let tsc = cfg.timer_scheduler_config();
        assert_eq!(tsc.poll_interval, std::time::Duration::from_millis(500));
        assert_eq!(tsc.batch_size, 50);
    }

    #[test]
    fn load_from_toml_string() {
        let toml_str = r#"
[server]
port = 8080

[log]
level = "debug"
format = "pretty"

[engine]
timer_poll_interval_ms = 2000
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.log.level, "debug");
        assert_eq!(cfg.log.format, "pretty");
        assert_eq!(cfg.engine.timer_poll_interval_ms, 2000);
        // Unset fields keep defaults
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.rate_limit.requests_per_minute, 100);
        // Database driver defaults to memory
        assert_eq!(cfg.database.driver, "memory");
    }

    #[test]
    fn load_from_toml_string_with_postgres_driver() {
        let toml_str = r#"
[database]
driver = "postgres"
url = "postgres://user:secret@localhost:5432/bpm"
max_connections = 5
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.database.driver, "postgres");
        assert_eq!(
            cfg.database.url,
            "postgres://user:secret@localhost:5432/bpm"
        );
        assert_eq!(cfg.database.max_connections, 5);
        // Unset migrate_on_start keeps its default (true)
        assert!(cfg.database.migrate_on_start);
        // Other sections keep defaults
        assert_eq!(cfg.server.port, 3000);
    }

    #[test]
    fn load_from_toml_string_can_disable_migrate_on_start() {
        let toml_str = r#"
[database]
driver = "postgres"
url = "postgres://user:secret@localhost:5432/bpm"
migrate_on_start = false
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.database.driver, "postgres");
        assert!(
            !cfg.database.migrate_on_start,
            "TOML [database] migrate_on_start = false must override the default"
        );
        // Unset fields keep defaults
        assert_eq!(cfg.database.max_connections, 10);
    }

    #[test]
    fn load_from_toml_string_migrate_on_start_true() {
        let toml_str = r#"
[database]
migrate_on_start = true
"#;
        let cfg: BpmConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.database.migrate_on_start);
    }
}
