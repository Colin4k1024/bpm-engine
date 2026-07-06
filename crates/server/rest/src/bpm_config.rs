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
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub timer_poll_interval_ms: u64,
    pub timer_batch_size: u32,
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
            url: String::new(),
            max_connections: 10,
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            timer_poll_interval_ms: 1000,
            timer_batch_size: 100,
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
            database.url = %mask_secret(&self.database.url),
            database.max_connections = self.database.max_connections,
            auth.jwt_secret = %mask_secret(&self.auth.jwt_secret),
            auth.api_key = %mask_secret(&self.auth.api_key),
            engine.timer_poll_interval_ms = self.engine.timer_poll_interval_ms,
            engine.timer_batch_size = self.engine.timer_batch_size,
            log.level = %self.log.level,
            log.format = %self.log.format,
            rate_limit.requests_per_minute = self.rate_limit.requests_per_minute,
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
        assert_eq!(cfg.engine.timer_poll_interval_ms, 1000);
        assert_eq!(cfg.engine.timer_batch_size, 100);
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.log.format, "json");
        assert_eq!(cfg.rate_limit.requests_per_minute, 100);
        assert!(cfg.auth.jwt_secret.is_empty());
        assert!(cfg.auth.api_key.is_empty());
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
    }
}
