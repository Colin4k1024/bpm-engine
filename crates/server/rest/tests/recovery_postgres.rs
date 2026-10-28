//! Crash-recovery verification against real PostgreSQL (issue E1-4).
//!
//! Requires Docker (testcontainers) locally, or `BPM_TEST_DATABASE_URL`
//! pointing at a reachable PostgreSQL (CI service container). Run:
//!
//! ```text
//! cargo test -p bpm-engine-server-rest --test recovery_postgres -- --ignored
//! ```
//!
//! Unlike the in-process suites, these tests kill the **real `api_server`
//! binary** (SIGKILL, no graceful shutdown) mid-flight and start a fresh
//! child against the same database, proving crash-safe continuation at the
//! process level:
//!
//! - a hard kill while an external task is locked leaves the instance
//!   Running; the lease survives the crash, the task is redelivered to a new
//!   worker after the lease expires, and completing it finishes the instance
//! - a timer catch scheduled before the kill fires after the restart (the
//!   timer record is persisted; the restarted scheduler re-arms it)
//! - an instance that already completed before the kill stays Completed
//!   after the restart — history intact, no resurrection
//!
//! # Failure artifacts (event trail)
//!
//! On any assertion failure the suite dumps the instance state, the token
//! set and the full event history to stdout AND to
//! `recovery-<test>-trail.log` under `CARGO_TARGET_TMPDIR`. Each killed
//! server generation keeps its stdout/stderr in
//! `recovery-<test>-server<N>.{out,err}.log` in the same directory, so a
//! failure preserves the full event trail (CI uploads every `recovery-*.log`
//! on failure).
//!
//! # Dual mode
//!
//! When `BPM_TEST_DATABASE_URL` is set (CI service container), tests connect
//! there instead of spinning up testcontainers; each test then creates an
//! **isolated database** (`bpm_rec_test_<name>`) from the URL's server so
//! tests never interfere with each other (dropped again on teardown,
//! best-effort). In local mode each test gets its own throwaway container,
//! which is naturally isolated.

#![cfg(feature = "postgres")]

use bpm_engine_worker_sdk::EngineClient;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::Duration;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;
use tokio::process::{Child, Command};

// ---------------------------------------------------------------------------
// Panic-hook trail dump: a last-resort safety net so ANY panic in this suite
// (bare `.expect()`, the SIGKILL assertion, helper internals — paths that do
// not flow through `check`/`wait_for_state`) still leaves an event trail.
// The hook is one-shot per test: it takes the registered context, writes the
// header synchronously, and fetches instance + history on a detached thread
// with a short deadline so it cannot hang the test runner.
// ---------------------------------------------------------------------------

/// Context snapshot the panic hook needs to dump a trail.
#[derive(Clone)]
struct TrailContext {
    test_name: String,
    base_url: String,
    trail_path: PathBuf,
    out_log: PathBuf,
    err_log: PathBuf,
    generation: u32,
    port: u16,
    db_url: String,
    /// Last instance id the test noted (set via `Harness::note_instance`).
    instance_hint: Option<String>,
}

static ACTIVE_TRAIL: Mutex<Option<TrailContext>> = Mutex::new(None);

fn install_trail_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // One-shot: take the context so a panic inside the dump itself
            // does not recurse.
            let ctx = ACTIVE_TRAIL.lock().ok().and_then(|mut g| g.take());
            if let Some(ctx) = ctx {
                dump_trail_on_panic(&ctx, &info.to_string());
            }
            default_hook(info);
        }));
    });
}

/// Synchronous header + detached best-effort HTTP fetch, 3s deadline.
fn dump_trail_on_panic(ctx: &TrailContext, panic_msg: &str) {
    let out = format!(
        "=== recovery event trail [panic hook, {}] ===\npanic: {}\ngeneration: {} (port {}, db {})\ninstance hint: {}\nserver logs: {} / {}\n",
        ctx.test_name,
        panic_msg.lines().next().unwrap_or(""),
        ctx.generation,
        ctx.port,
        ctx.db_url,
        ctx.instance_hint.as_deref().unwrap_or("<none>"),
        ctx.out_log.display(),
        ctx.err_log.display(),
    );
    println!("{out}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ctx.trail_path)
    {
        use std::io::Write;
        let _ = file.write_all(out.as_bytes());
    }

    // Fetch instance + history on a detached thread (fresh runtime: the
    // panicking thread may sit inside a tokio worker).
    let ctx = ctx.clone();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut fetched = String::new();
        if let Some(id) = &ctx.instance_hint {
            for (label, url) in [
                (
                    "instance",
                    format!("{}/api/v1/process-instances/{id}", ctx.base_url),
                ),
                (
                    "history",
                    format!("{}/api/v1/process-instances/{id}/history", ctx.base_url),
                ),
            ] {
                match rt.block_on(client.get(url).send()) {
                    Ok(resp) => match rt.block_on(resp.json::<serde_json::Value>()) {
                        Ok(body) => fetched.push_str(&format!("{label}: {body:#}\n")),
                        Err(e) => fetched.push_str(&format!("{label}: <bad json: {e}>\n")),
                    },
                    Err(e) => fetched.push_str(&format!("{label}: <server unreachable: {e}>\n")),
                }
            }
        }
        let _ = tx.send(fetched);
    });
    if let Ok(fetched) = rx.recv_timeout(Duration::from_secs(3)) {
        if !fetched.is_empty() {
            println!("{fetched}");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&ctx.trail_path)
            {
                use std::io::Write;
                let _ = file.write_all(fetched.as_bytes());
            }
        }
    }
}

/// Process with one ExternalTask (task_type "crash-work") that blocks until
/// a worker completes it.
const EXTERNAL_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="crash-work-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="crash-work" name="Crash Work">
      <extensionElements>
        <taskDefinition type="crash-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="crash-work"/>
    <sequenceFlow id="f2" sourceRef="crash-work" targetRef="end"/>
  </process>
</definitions>"#;

/// Process with a PT2S timer intermediate catch event.
const TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-restart-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateCatchEvent id="wait-2s" name="Wait 2 Seconds">
      <timerEventDefinition>
        <timeDuration>PT2S</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="wait-2s"/>
    <sequenceFlow id="f2" sourceRef="wait-2s" targetRef="end"/>
  </process>
</definitions>"#;

/// Lease used by worker A before the hard kill (per the issue: 30 s).
const WORKER_LEASE_MS: u64 = 30_000;

/// Serializes child-server startup across the parallel tests in this binary.
/// A fixed port is briefly free between the kill and the re-bind; the lock
/// guarantees no other test picks that port in that window (and that a test
/// picking a port binds it — via a healthy child — before releasing).
static STARTUP_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Per-test harness: isolated PostgreSQL, a config file driving the real
/// `api_server` binary, child-process lifecycle (spawn / SIGKILL / restart),
/// REST helpers that dump the event trail on failure, and teardown.
struct Harness {
    test_name: String,
    /// URL of the isolated test database the child servers connect to.
    db_url: String,
    /// Maintenance URL used to create/drop the isolated database (env mode).
    admin_url: String,
    /// Isolated database name (env mode only; `None` in testcontainer mode).
    isolated_db: Option<String>,
    /// Throwaway container (local mode). Dropped on teardown.
    _container: Option<testcontainers::Container<'static, Postgres>>,
    /// Config file the child processes read via `BPM_CONFIG_PATH`.
    config_path: PathBuf,
    /// Fixed port the child binds (the real binary does not support port 0).
    port: u16,
    base_url: String,
    client: reqwest::Client,
    /// Event-trail file, written on assertion failures.
    trail_path: PathBuf,
    /// Log name prefix shared by trail and child logs (`recovery-<test>`).
    log_prefix: String,
    generation: u32,
    child: Option<Child>,
    /// Exit status of the last killed child (evidence of the hard kill).
    child_exit: Option<std::process::ExitStatus>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Clear this test's panic-hook context so a later panic in another
        // test cannot dump a stale trail.
        if let Ok(mut guard) = ACTIVE_TRAIL.lock() {
            if let Some(ctx) = guard.as_ref() {
                if ctx.test_name == self.test_name {
                    *guard = None;
                }
            }
        }
    }
}

impl Harness {
    /// Prepare the database (dual mode) and the artifact paths. The config
    /// file and the server itself are created by [`Harness::start_server`].
    async fn new(test_name: &str) -> Self {
        install_trail_panic_hook();
        let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(&tmp).expect("create CARGO_TARGET_TMPDIR");
        let log_prefix = format!("recovery-{test_name}");
        let trail_path = tmp.join(format!("{log_prefix}-trail.log"));

        let (db_url, admin_url, isolated_db, container) =
            setup_database(test_name, &log_prefix).await;

        Harness {
            test_name: test_name.to_string(),
            db_url,
            admin_url,
            isolated_db,
            _container: container,
            config_path: tmp.join(format!("{log_prefix}-bpm.toml")),
            port: 0,
            base_url: String::new(),
            client: reqwest::Client::new(),
            trail_path,
            log_prefix,
            generation: 0,
            child: None,
            child_exit: None,
        }
    }

    fn out_log_path(&self) -> PathBuf {
        self.trail_path.with_file_name(format!(
            "{}-server{}.out.log",
            self.log_prefix, self.generation
        ))
    }

    fn err_log_path(&self) -> PathBuf {
        self.trail_path.with_file_name(format!(
            "{}-server{}.err.log",
            self.log_prefix, self.generation
        ))
    }

    /// Write the TOML config the child reads (`BPM_CONFIG_PATH`). The real
    /// binary binds `server.port` literally, so the harness picks a free port
    /// and pins it for all generations.
    fn write_config(&self) {
        let toml = format!(
            "[server]\nhost = \"127.0.0.1\"\nport = {}\n\
             \n[database]\ndriver = \"postgres\"\nurl = {}\nmax_connections = 5\nmigrate_on_start = true\n\
             \n[engine]\ntimer_poll_interval_ms = 200\ntimer_batch_size = 100\n\
             \n[rate_limit]\nrequests_per_minute = 1000000\n\
             \n[log]\nlevel = \"info\"\nformat = \"json\"\n",
            self.port,
            toml_string(&self.db_url),
        );
        std::fs::write(&self.config_path, toml).expect("write child config TOML");
    }

    /// Spawn the `api_server` binary and wait until it is healthy.
    /// `STARTUP_LOCK` must be held by the caller.
    async fn spawn_and_wait_healthy(&mut self) {
        self.generation += 1;
        let out = std::fs::File::create(self.out_log_path()).expect("create child stdout log");
        let err = std::fs::File::create(self.err_log_path()).expect("create child stderr log");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_api_server"));
        // Scrub BPM_* env vars inherited from the test harness (e.g.
        // BPM_TEST_DATABASE_URL, BPM_CONFIG_PATH) so the child is driven ONLY
        // by the generated config file.
        for (key, _) in std::env::vars() {
            if key.starts_with("BPM_") {
                cmd.env_remove(&key);
            }
        }
        let child = cmd
            .env("BPM_CONFIG_PATH", &self.config_path)
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            // Safety net: a panicking test must not leak server processes.
            .kill_on_drop(true)
            .spawn()
            .expect("spawn api_server child process");

        let mut child = child;
        self.wait_healthy(&mut child, Duration::from_secs(30)).await;
        self.child = Some(child);
        // Readiness (not just liveness): the new process must have connected
        // to PostgreSQL with an up-to-date schema.
        let resp = self
            .client
            .get(format!("{}/ready", self.base_url))
            .send()
            .await
            .expect("/ready request");
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.expect("/ready json");
        self.check(
            None,
            status == 200 && body["checks"]["db_pool"] == "ok",
            &format!("restarted server /ready must be ok, got HTTP {status}: {body}"),
        )
        .await;
    }

    /// Pick a free port, write the config, and start the first server
    /// generation. Holds `STARTUP_LOCK` until the child is bound (healthy).
    async fn start_server(&mut self) {
        let _guard = STARTUP_LOCK.lock().await;
        if self.port == 0 {
            self.port = pick_free_port();
        }
        if self.base_url.is_empty() {
            self.base_url = format!("http://127.0.0.1:{}", self.port);
        }
        self.write_config();
        self.spawn_and_wait_healthy().await;
        self.arm_trail_hook();
    }

    /// Hard-kill (SIGKILL) the running child and start a fresh one with the
    /// same config (same port, same database): a crash + operator restart.
    async fn kill_and_restart(&mut self) {
        let _guard = STARTUP_LOCK.lock().await;
        self.kill_child().await;
        self.write_config();
        self.spawn_and_wait_healthy().await;
        self.arm_trail_hook();
    }

    /// SIGKILL the running child and reap it. Panics unless the process
    /// actually died by signal 9 (proof of a true hard kill, not a graceful
    /// shutdown). `STARTUP_LOCK` must be held by the caller.
    async fn kill_child(&mut self) {
        let mut child = self
            .child
            .take()
            .expect("kill_child called with no running server");
        // SIGKILL is immediate and cannot be caught: the process gets no
        // chance to run graceful shutdown, close pools, or flush state.
        if child.kill().await.is_err() {
            eprintln!("warning: api_server exited before the SIGKILL landed");
        }
        let status = child.wait().await.expect("reap killed api_server child");
        #[cfg(unix)]
        {
            let signal = std::os::unix::process::ExitStatusExt::signal(&status);
            assert_eq!(
                signal,
                Some(9),
                "api_server must die by SIGKILL, got exit status {status}"
            );
        }
        self.child_exit = Some(status);
    }

    /// Poll `/health` until the child answers 200, or panic with the child
    /// log tail (fast path: detect an early exit via `try_wait`).
    async fn wait_healthy(&self, child: &mut Child, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let note = format!("api_server exited early with {status}");
                self.dump_trail(None, &note).await;
                panic!(
                    "{note}\n--- stdout tail ---\n{}\n--- stderr tail ---\n{}",
                    read_log_tail(&self.out_log_path(), 4000),
                    read_log_tail(&self.err_log_path(), 4000),
                );
            }
            if let Ok(resp) = self
                .client
                .get(format!("{}/health", self.base_url))
                .send()
                .await
            {
                if resp.status().as_u16() == 200 {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let note = format!(
                    "api_server did not become healthy within {timeout:?} (port {})",
                    self.port
                );
                self.dump_trail(None, &note).await;
                panic!(
                    "{note}\n--- stdout tail ---\n{}\n--- stderr tail ---\n{}",
                    read_log_tail(&self.out_log_path(), 4000),
                    read_log_tail(&self.err_log_path(), 4000),
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Assert `cond`, dumping the event trail first when it fails.
    async fn check(&self, instance_id: Option<&str>, cond: bool, msg: &str) {
        if !cond {
            self.dump_trail(instance_id, msg).await;
            panic!("{msg}");
        }
    }

    /// Dump the event trail for `instance_id` (instance state, tokens, full
    /// history) to stdout and to the trail file. Best-effort by design: it
    /// runs on failure paths where the server may be down.
    async fn dump_trail(&self, instance_id: Option<&str>, note: &str) {
        let mut out = format!(
            "=== recovery event trail [{}] ===\nnote: {note}\ngeneration: {} (port {}, db {})\nlast kill exit: {}\n",
            self.test_name,
            self.generation,
            self.port,
            self.db_url,
            self.child_exit
                .map(|s| s.to_string())
                .unwrap_or_else(|| "<none>".to_string()),
        );
        if let Some(id) = instance_id {
            match self
                .client
                .get(format!("{}/api/v1/process-instances/{id}", self.base_url))
                .send()
                .await
            {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(body) => out.push_str(&format!("instance: {body:#}\n")),
                    Err(e) => out.push_str(&format!("instance: <bad json: {e}>\n")),
                },
                Err(e) => out.push_str(&format!("instance: <server unreachable: {e}>\n")),
            }
            match self
                .client
                .get(format!(
                    "{}/api/v1/process-instances/{id}/history",
                    self.base_url
                ))
                .send()
                .await
            {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        if let Some(events) = body["events"].as_array() {
                            out.push_str(&format!("history ({} events):\n", events.len()));
                            for (i, e) in events.iter().enumerate() {
                                out.push_str(&format!(
                                    "  #{i} {} node={}\n",
                                    e["event_type"], e["payload"]["node_id"]
                                ));
                            }
                        } else {
                            out.push_str(&format!("history: <no events array: {body}>\n"));
                        }
                    }
                    Err(e) => out.push_str(&format!("history: <bad json: {e}>\n")),
                },
                Err(e) => out.push_str(&format!("history: <server unreachable: {e}>\n")),
            }
        }
        out.push_str(&format!(
            "server logs: {} / {}\n",
            self.out_log_path().display(),
            self.err_log_path().display()
        ));
        println!("{out}");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.trail_path)
        {
            use std::io::Write;
            let _ = file.write_all(out.as_bytes());
            let _ = file.write_all(b"\n");
        }
    }

    /// Kill any running child, drop the isolated database (env mode) and the
    /// container (local mode).
    async fn teardown(mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(db) = &self.isolated_db {
            drop_database(&self.admin_url, db).await;
        }
    }

    // --- REST helpers (dump the trail on failure) -------------------------

    async fn deploy(&self, bpmn: &str) {
        let resp = self
            .client
            .post(format!(
                "{}/api/v1/process-definitions/deploy",
                self.base_url
            ))
            .header("content-type", "application/xml")
            .body(bpmn.to_string())
            .send()
            .await
            .expect("deploy request");
        let status = resp.status().as_u16();
        self.check(
            None,
            status == 201,
            &format!("deploy should return 201, got {status}"),
        )
        .await;
    }

    async fn start_instance(&self, def_id: &str) -> String {
        let resp = self
            .client
            .post(format!("{}/api/v1/process-instances", self.base_url))
            .json(&serde_json::json!({ "process_def_id": def_id }))
            .send()
            .await
            .expect("start request");
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.expect("start json");
        self.check(
            None,
            status == 201 && body["instance_id"].is_string(),
            &format!("start should return 201 with instance_id, got {status}: {body}"),
        )
        .await;
        let id = body["instance_id"]
            .as_str()
            .expect("instance_id")
            .to_string();
        // Keep the panic-hook trail context pointed at this instance so a
        // later bare `.expect()` panic still dumps its trail.
        self.note_instance(&id);
        id
    }

    /// Update the registered panic-hook context with the instance to dump.
    fn note_instance(&self, id: &str) {
        if let Ok(mut guard) = ACTIVE_TRAIL.lock() {
            if let Some(ctx) = guard.as_mut() {
                if ctx.test_name == self.test_name {
                    ctx.instance_hint = Some(id.to_string());
                }
            }
        }
    }

    /// Snapshot the harness state into the panic-hook registry.
    fn arm_trail_hook(&self) {
        let ctx = TrailContext {
            test_name: self.test_name.clone(),
            base_url: self.base_url.clone(),
            trail_path: self.trail_path.clone(),
            out_log: self.out_log_path(),
            err_log: self.err_log_path(),
            generation: self.generation,
            port: self.port,
            db_url: self.db_url.clone(),
            instance_hint: None,
        };
        if let Ok(mut guard) = ACTIVE_TRAIL.lock() {
            // Preserve an existing instance hint across re-arms (restarts).
            let hint = guard
                .as_ref()
                .filter(|c| c.test_name == self.test_name)
                .and_then(|c| c.instance_hint.clone());
            let mut ctx = ctx;
            ctx.instance_hint = hint;
            *guard = Some(ctx);
        }
    }

    async fn get_instance(&self, instance_id: &str) -> serde_json::Value {
        let resp = self
            .client
            .get(format!(
                "{}/api/v1/process-instances/{}",
                self.base_url, instance_id
            ))
            .send()
            .await
            .expect("get instance request");
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.expect("instance json");
        self.check(
            Some(instance_id),
            status == 200,
            &format!("get instance should return 200, got {status}: {body}"),
        )
        .await;
        body
    }

    /// Poll GET instance until `status` equals `expected`; on timeout dump
    /// the trail and panic.
    async fn wait_for_state(
        &self,
        instance_id: &str,
        expected: &str,
        timeout: Duration,
    ) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let body = self.get_instance(instance_id).await;
            if body["status"] == expected {
                return body;
            }
            if tokio::time::Instant::now() >= deadline {
                self.dump_trail(
                    Some(instance_id),
                    &format!(
                        "instance did not reach {expected} within {timeout:?} (last: {})",
                        body["status"]
                    ),
                )
                .await;
                panic!(
                    "instance {} did not reach {} within {:?}; last state: {}",
                    instance_id, expected, timeout, body["status"]
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn history_events(&self, instance_id: &str) -> Vec<serde_json::Value> {
        let resp = self
            .client
            .get(format!(
                "{}/api/v1/process-instances/{}/history",
                self.base_url, instance_id
            ))
            .send()
            .await
            .expect("history request");
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.expect("history json");
        self.check(
            Some(instance_id),
            status == 200 && body["events"].is_array(),
            &format!("history should return 200 with events, got {status}: {body}"),
        )
        .await;
        body["events"].as_array().cloned().expect("events array")
    }
}

// ---------------------------------------------------------------------------
// Database setup (dual mode: env URL or testcontainers)
// ---------------------------------------------------------------------------

/// Pick the test database URL for `test_name`.
///
/// `BPM_TEST_DATABASE_URL` (CI): create an isolated database from the URL's
/// server. Otherwise: start a throwaway Postgres container (naturally
/// isolated). Also probes until PostgreSQL actually accepts queries —
/// `build_app` runs migrations on an eager pool connection, and postgres
/// accepts TCP before finishing startup, so an immediate connect can race.
async fn setup_database(
    test_name: &str,
    log_prefix: &str,
) -> (
    String,
    String,
    Option<String>,
    Option<testcontainers::Container<'static, Postgres>>,
) {
    let (base_url, container) = match std::env::var("BPM_TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            println!("{log_prefix}: using BPM_TEST_DATABASE_URL for the isolated test database");
            (url.trim().to_string(), None)
        }
        _ => {
            let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
            let container = docker.run(Postgres::default());
            let port = container.get_host_port_ipv4(5432);
            (
                format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
                Some(container),
            )
        }
    };

    let admin_url = maintenance_url(&base_url);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match probe_postgres(&admin_url).await {
            Ok(()) => break,
            Err(e) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = e;
            }
            Err(e) => panic!("{log_prefix}: postgres not ready within 30s: {e}"),
        }
    }

    match container {
        Some(container) => (base_url, admin_url, None, Some(container)),
        None => {
            let db_name = isolated_db_name(test_name);
            // Clean up any leftover database from a previous (failed) run.
            drop_database(&admin_url, &db_name).await;
            let pool = bpm_engine_adapter_postgres::create_pool(&admin_url).expect("admin pool");
            let client = pool.get().await.expect("admin client");
            // PostgreSQL identifiers cannot be parameter-bound; `db_name` is
            // generated locally from an allow-listed character set (see
            // `isolated_db_name`), so interpolation here is safe.
            client
                .execute(&format!("CREATE DATABASE {db_name}"), &[])
                .await
                .expect("create isolated test database");
            pool.close();
            let test_url = url_with_database(&base_url, &db_name);
            (test_url, admin_url, Some(db_name), None)
        }
    }
}

/// Open a connection and run a trivial query to verify postgres is serving.
async fn probe_postgres(url: &str) -> anyhow::Result<()> {
    let pool = bpm_engine_adapter_postgres::create_pool(url)?;
    let client = pool.get().await?;
    client.execute("SELECT 1", &[]).await?;
    pool.close();
    Ok(())
}

/// Terminate all connections to `db_name` and drop it (best-effort).
async fn drop_database(admin_url: &str, db_name: &str) {
    let pool = match bpm_engine_adapter_postgres::create_pool(admin_url) {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("teardown: could not connect for drop: {err}");
            return;
        }
    };
    let client = match pool.get().await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("teardown: could not get admin client: {err}");
            return;
        }
    };
    // Connection pids are filtered by the datname parameter ($1); the
    // identifier interpolation below is over an allow-listed name.
    let _ = client
        .execute(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[&db_name],
        )
        .await;
    let _ = client
        .execute(&format!("DROP DATABASE IF EXISTS {db_name}"), &[])
        .await
        .inspect_err(|err| eprintln!("teardown: drop database failed: {err}"));
    pool.close();
}

/// Timer row (status, node_id) persisted for `instance_id`, if any.
async fn timer_row(db_url: &str, instance_id: &str) -> Option<(String, String)> {
    let pool = bpm_engine_adapter_postgres::create_pool(db_url).expect("timer query pool");
    let client = pool.get().await.expect("timer query client");
    let row = client
        .query_opt(
            "SELECT status, node_id FROM timer WHERE instance_id = $1",
            &[&instance_id],
        )
        .await
        .expect("timer row query");
    let result = row.map(|r| (r.get::<_, String>("status"), r.get::<_, String>("node_id")));
    pool.close();
    result
}

/// Map a base URL to the database used for maintenance statements. URLs
/// without a database name (empty path) are pointed at the conventional
/// `postgres` maintenance database; URLs with a name are used as-is.
fn maintenance_url(base: &str) -> String {
    url_with_database(base, "postgres")
}

/// Return the base URL rewritten to point at `db_name`.
///
/// The URLs used by this suite are plain `postgres://user:pass@host:port[/db]`
/// shapes (from testcontainers or `BPM_TEST_DATABASE_URL`), so simple string
/// surgery on the path is sufficient.
fn url_with_database(base: &str, db_name: &str) -> String {
    let scheme_end = base.find("://").expect("postgres URL has a scheme") + 3;
    let path_start = base[scheme_end..].find('/').map(|i| scheme_end + i);
    match path_start {
        Some(i) => format!("{}/{}", &base[..i], db_name),
        None => format!("{}/{}", base.trim_end_matches('/'), db_name),
    }
}

/// Isolated database name for a test: `bpm_rec_test_<sanitized test name>`.
///
/// PostgreSQL identifiers cannot be parameter-bound, so this name is built
/// only from an allow-listed character set (ASCII alphanumeric and `_`).
fn isolated_db_name(test_name: &str) -> String {
    let safe: String = test_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("bpm_rec_test_{safe}")
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

/// Bind an ephemeral listener, read the OS-assigned port, and drop it. The
/// port is immediately re-used by the spawned `api_server`; `STARTUP_LOCK`
/// closes the pick-to-bind race between the tests in this binary.
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port to find a free port");
    listener
        .local_addr()
        .expect("ephemeral listener local addr")
        .port()
}

/// Quote a string for the TOML config file.
fn toml_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Last `max_chars` characters of a log file (never panics).
fn read_log_tail(path: &Path, max_chars: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let mut start = s.len().saturating_sub(max_chars);
            while !s.is_char_boundary(start) {
                start += 1;
            }
            s[start..].to_string()
        }
        Err(e) => format!("<unreadable: {e}>"),
    }
}

/// Count history events of `event_type` (optionally with payload node_id).
fn count_events(events: &[serde_json::Value], event_type: &str, node_id: Option<&str>) -> usize {
    events
        .iter()
        .filter(|e| e["event_type"] == event_type)
        .filter(|e| match node_id {
            Some(n) => e["payload"]["node_id"] == n,
            None => true,
        })
        .count()
}

/// Stable summary of a history event list (multiset of `type@node`), used to
/// prove the history is unchanged across a restart.
fn history_summary(events: &[serde_json::Value]) -> Vec<String> {
    let mut summary: Vec<String> = events
        .iter()
        .map(|e| format!("{}@{}", e["event_type"], e["payload"]["node_id"]))
        .collect();
    summary.sort();
    summary
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Hard-kill the server while worker A holds the external-task lease, then
/// restart and prove crash-safe continuation:
///
/// 1. the instance survives the SIGKILL, still Running with its Waiting token
/// 2. the lease survives too — the task is NOT redelivered while it is valid
/// 3. after the lease expires the SAME task is redelivered to a new worker
/// 4. completing it after the restart finishes the instance, with exactly one
///    completion in history
#[tokio::test]
#[ignore = "requires Docker"]
async fn hard_kill_restart_redelivers_locked_external_task() {
    let mut h = Harness::new("hard_kill_restart_redelivers").await;
    h.start_server().await;

    h.deploy(EXTERNAL_TASK_BPMN).await;
    let instance_id = h.start_instance("crash-work-flow").await;

    // Worker A locks the task (30 s lease) and never completes: mid-flight
    // crash state.
    let worker_a = EngineClient::new(&h.base_url);
    let tasks = worker_a
        .fetch_and_lock("worker-a", &["crash-work".to_string()], 1, WORKER_LEASE_MS)
        .await
        .expect("worker-a fetch before the kill");
    h.check(
        Some(&instance_id),
        tasks.len() == 1,
        &format!("worker-a must lock exactly one task, got {}", tasks.len()),
    )
    .await;
    let task_id = tasks[0].task_id.clone();

    let body = h.get_instance(&instance_id).await;
    h.check(
        Some(&instance_id),
        body["status"] == "RUNNING",
        &format!(
            "instance must be Running before the kill, got {}",
            body["status"]
        ),
    )
    .await;

    // --- hard crash: SIGKILL, then a fresh server on the same database ----
    h.kill_and_restart().await;

    // 1. The instance is still queryable and Running, token still Waiting.
    let body = h.get_instance(&instance_id).await;
    h.check(
        Some(&instance_id),
        body["status"] == "RUNNING",
        &format!(
            "instance must survive the hard kill as Running, got {}",
            body["status"]
        ),
    )
    .await;
    let tokens = body["tokens"].as_array().cloned().unwrap_or_default();
    h.check(
        Some(&instance_id),
        tokens
            .iter()
            .any(|t| t["node_id"] == "crash-work" && t["status"] == "Waiting"),
        &format!("token must still wait at 'crash-work' after the restart: {tokens:?}"),
    )
    .await;

    // 2. The persisted lease survived the crash: while it is still valid the
    // task must NOT be redelivered (single-owner invariant).
    let worker_b = EngineClient::new(&h.base_url);
    let early = worker_b
        .fetch_and_lock("worker-b", &["crash-work".to_string()], 1, WORKER_LEASE_MS)
        .await
        .expect("worker-b fetch right after the restart");
    h.check(
        Some(&instance_id),
        early.is_empty(),
        "task must not be redelivered while worker-a's lease is still valid",
    )
    .await;

    // 3. After the lease expires the SAME task is redelivered (at-least-once).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let redelivered = loop {
        let tasks = worker_b
            .fetch_and_lock("worker-b", &["crash-work".to_string()], 1, WORKER_LEASE_MS)
            .await
            .expect("worker-b redelivery fetch");
        if let Some(task) = tasks.first() {
            break task.task_id.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            h.dump_trail(
                Some(&instance_id),
                "task was not redelivered within 45s of lease expiry",
            )
            .await;
            panic!("expired lease was not reclaimed/redelivered within 45s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    h.check(
        Some(&instance_id),
        redelivered == task_id,
        &format!("redelivered task must be the SAME task id ({task_id}), got {redelivered}"),
    )
    .await;

    // 4. Completing it after the restart finishes the instance.
    worker_b
        .complete(&task_id, "worker-b", HashMap::new())
        .await
        .expect("worker-b complete after the restart");
    h.wait_for_state(&instance_id, "COMPLETED", Duration::from_secs(15))
        .await;

    let events = h.history_events(&instance_id).await;
    h.check(
        Some(&instance_id),
        count_events(&events, "ExternalTaskLocked", None) == 2,
        &format!(
            "task must be locked twice (initial + redelivery after reclaim), got {}",
            count_events(&events, "ExternalTaskLocked", None)
        ),
    )
    .await;
    h.check(
        Some(&instance_id),
        count_events(&events, "ExternalTaskCompleted", None) == 1,
        &format!(
            "exactly one completion expected, got {}",
            count_events(&events, "ExternalTaskCompleted", None)
        ),
    )
    .await;
    h.check(
        Some(&instance_id),
        count_events(&events, "ProcessCompleted", None) == 1,
        &format!(
            "exactly one ProcessCompleted expected, got {}",
            count_events(&events, "ProcessCompleted", None)
        ),
    )
    .await;

    h.teardown().await;
}

/// Kill the server before a PT2S timer fires and restart it: the persisted
/// timer record is re-armed by the restarted scheduler and the instance
/// completes through the timer — crash-safe timer continuation.
#[tokio::test]
#[ignore = "requires Docker"]
async fn timer_survives_hard_kill_restart() {
    let mut h = Harness::new("timer_survives_restart").await;
    h.start_server().await;

    h.deploy(TIMER_BPMN).await;
    let instance_id = h.start_instance("timer-restart-flow").await;

    // The instance waits at the timer catch; the timer record is persisted.
    let body = h.get_instance(&instance_id).await;
    let nodes: Vec<String> = body["current_nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    h.check(
        Some(&instance_id),
        nodes.iter().any(|n| n == "wait-2s"),
        &format!("instance should wait at the timer catch, got {nodes:?}"),
    )
    .await;
    let (timer_status, timer_node) = timer_row(&h.db_url, &instance_id)
        .await
        .expect("timer record must be persisted at the catch");
    h.check(
        Some(&instance_id),
        timer_status == "Scheduled" && timer_node == "wait-2s",
        &format!("timer record must be Scheduled at 'wait-2s', got {timer_status} at {timer_node}"),
    )
    .await;

    // --- hard kill BEFORE the timer fires (PT2S; kill at ~500 ms) ---------
    tokio::time::sleep(Duration::from_millis(500)).await;
    let body = h.get_instance(&instance_id).await;
    h.check(
        Some(&instance_id),
        body["status"] == "RUNNING",
        &format!(
            "instance must still be Running when the server is killed (the timer must not have \
             fired yet); got {} — rerun on a less loaded machine if the timer already fired",
            body["status"]
        ),
    )
    .await;
    h.kill_and_restart().await;

    // The restarted scheduler re-arms the persisted timer and the instance
    // completes through it.
    h.wait_for_state(&instance_id, "COMPLETED", Duration::from_secs(20))
        .await;

    let (timer_status, _) = timer_row(&h.db_url, &instance_id)
        .await
        .expect("timer record must exist after the restart");
    h.check(
        Some(&instance_id),
        timer_status == "Fired",
        &format!("timer record must be Fired after the restart, got {timer_status}"),
    )
    .await;

    let events = h.history_events(&instance_id).await;
    h.check(
        Some(&instance_id),
        count_events(&events, "TokenArrived", Some("end")) == 1,
        &format!(
            "timer must advance the token past the catch exactly once, got {}",
            count_events(&events, "TokenArrived", Some("end"))
        ),
    )
    .await;
    h.check(
        Some(&instance_id),
        count_events(&events, "ProcessCompleted", None) == 1,
        &format!(
            "exactly one ProcessCompleted expected, got {}",
            count_events(&events, "ProcessCompleted", None)
        ),
    )
    .await;

    h.teardown().await;
}

/// Complete an instance, hard-kill + restart the server, and verify the
/// completion is durable: still Completed, token set and history unchanged,
/// no task resurrection.
#[tokio::test]
#[ignore = "requires Docker"]
async fn completed_instance_is_not_resurrected_by_restart() {
    let mut h = Harness::new("completed_instance_not_resurrected").await;
    h.start_server().await;

    h.deploy(EXTERNAL_TASK_BPMN).await;
    let instance_id = h.start_instance("crash-work-flow").await;

    // Complete the external task and let the instance finish.
    let worker = EngineClient::new(&h.base_url);
    let tasks = worker
        .fetch_and_lock("worker-a", &["crash-work".to_string()], 1, WORKER_LEASE_MS)
        .await
        .expect("worker fetch");
    h.check(
        Some(&instance_id),
        tasks.len() == 1,
        &format!("worker must lock exactly one task, got {}", tasks.len()),
    )
    .await;
    worker
        .complete(&tasks[0].task_id, "worker-a", HashMap::new())
        .await
        .expect("worker complete");
    let before = h
        .wait_for_state(&instance_id, "COMPLETED", Duration::from_secs(15))
        .await;
    let events_before = h.history_events(&instance_id).await;

    // --- hard kill + restart ----------------------------------------------
    h.kill_and_restart().await;

    // Still Completed: the durable completion is not resurrected.
    let after = h.get_instance(&instance_id).await;
    h.check(
        Some(&instance_id),
        after["status"] == "COMPLETED",
        &format!(
            "instance must stay Completed after the restart, got {}",
            after["status"]
        ),
    )
    .await;
    h.check(
        Some(&instance_id),
        after["tokens"] == before["tokens"],
        &format!(
            "token set must be unchanged by the restart\nbefore: {}\nafter:  {}",
            before["tokens"], after["tokens"]
        ),
    )
    .await;

    // History intact: same event multiset, exactly one completion.
    let events_after = h.history_events(&instance_id).await;
    h.check(
        Some(&instance_id),
        history_summary(&events_after) == history_summary(&events_before),
        "history must be identical across the restart (no resurrection, no new events)",
    )
    .await;
    h.check(
        Some(&instance_id),
        count_events(&events_after, "ProcessCompleted", None) == 1
            && count_events(&events_after, "ExternalTaskCompleted", None) == 1
            && count_events(&events_after, "TokenFailed", None) == 0,
        &format!(
            "expected exactly one ProcessCompleted and ExternalTaskCompleted, no TokenFailed; \
             got {:?}",
            history_summary(&events_after)
        ),
    )
    .await;

    // The completed task is not redelivered to workers after the restart.
    let refetched = worker
        .fetch_and_lock("worker-b", &["crash-work".to_string()], 1, WORKER_LEASE_MS)
        .await
        .expect("fetch after the restart");
    h.check(
        Some(&instance_id),
        refetched.is_empty(),
        "completed task must not be redelivered after the restart",
    )
    .await;

    h.teardown().await;
}
