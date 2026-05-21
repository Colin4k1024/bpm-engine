---
layout: default
title: bpm-engine
description: A correctness-first, token-driven workflow execution kernel in Rust
---

<!-- Hero Section -->
<div class="hero">
  <div class="hero-badge">
    <span class="badge">v0.2.0</span>
    <span class="badge">Rust</span>
    <span class="badge">Apache-2.0</span>
  </div>
  <h1 class="hero-title">bpm-engine</h1>
  <p class="hero-subtitle">
    A correctness-first, token-driven workflow execution kernel in Rust.<br>
    Designed for deterministic replay and crash-safe long-running processes.
  </p>
  <div class="hero-actions">
    <a href="#getting-started" class="btn btn-primary">Quick Start</a>
    <a href="https://github.com/fanjia1024/bpm-engine" class="btn btn-secondary">GitHub</a>
  </div>
</div>

<!-- Core Value Proposition -->
<div class="section">
  <h2 class="section-title">Why bpm-engine?</h2>
  <p class="section-desc">
    Most BPM engines optimize for <strong>features and modeling UX</strong>. This engine optimizes for <strong>correctness</strong>.
    Every execution step is driven by database state. Every state transition is recorded as history.
    Every execution can be replayed and verified.
  </p>
</div>

<!-- Key Features -->
<div class="section">
  <h2 class="section-title">Core Guarantees</h2>
  <div class="features-grid">

    <div class="feature-card">
      <div class="feature-icon">⚡</div>
      <h3>Exactly-Once Completion</h3>
      <p>Tokens reach final states exactly once. No duplicate executions, no lost state.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔄</div>
      <h3>Crash-Safe by Design</h3>
      <p>Engine recovers deterministically by replaying events. Kill the process, restart, continue.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔒</div>
      <h3>Lease-Based External Tasks</h3>
      <p>External workers fetch tasks by topic. Leases guarantee exactly one owner at a time.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">⏱️</div>
      <h3>Persistent Timers</h3>
      <p>No in-memory timers. Timers survive restarts and are naturally scalable.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">📜</div>
      <h3>Full Audit History</h3>
      <p>Every state change emits an event. History is append-only and globally ordered.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🛡️</div>
      <h3>Formal Invariants</h3>
      <p>Join nodes wait for all branches. Retries are monotonic. Core behavior is proven.</p>
    </div>

  </div>
</div>

<!-- Quick Start -->
<div class="section" id="getting-started">
  <h2 class="section-title">Getting Started</h2>

  <div class="code-tabs">
    <button class="tab-btn active" data-tab="install">Install</button>
    <button class="tab-btn" data-tab="start">Start Engine</button>
    <button class="tab-btn" data-tab="run">Run Process</button>
    <button class="tab-btn" data-tab="worker">External Worker</button>
  </div>

  <div class="tab-content active" id="install">
{% highlight bash %}
# Clone the repository
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine

# Build the project
cargo build
{% endhighlight %}
  </div>

  <div class="tab-content" id="start">
{% highlight bash %}
# Start the REST server (http://127.0.0.1:3000)
cargo run -p bpm-server-rest
{% endhighlight %}
<p class="code-note">
  Built-in process definitions: <code>minimal</code> (Start → End), <code>payment-flow</code> (Start → ExternalTask `payment` → End)
</p>
  </div>

  <div class="tab-content" id="run">
{% highlight bash %}
# Run a minimal process (Start → End)
cargo run --example simple_process

# Or with curl:
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'

# Check instance status
curl http://127.0.0.1:3000/api/v1/process-instances/:id

# Get execution history
curl http://127.0.0.1:3000/api/v1/process-instances/:id/history

# Get aggregated trace
curl http://127.0.0.1:3000/api/v1/process-instances/:id/trace
{% endhighlight %}
  </div>

  <div class="tab-content" id="worker">
{% highlight rust %}
use bpm_worker_sdk::{EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig};

struct PaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str { "payment" }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // Business logic here
        let mut variables = std::collections::HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}

let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(PaymentHandler)
    .config(WorkerConfig::new("worker-1")
        .poll_interval(std::time::Duration::from_secs(1)))
    .build();

worker.start().await;
{% endhighlight %}
  </div>
</div>

<!-- Architecture -->
<div class="section">
  <h2 class="section-title">Architecture</h2>

  <div class="arch-diagram">
    <pre>┌─────────────────────────────────────────────────────────────┐
│                     bpm-engine                              │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐   │
│  │  Scheduler  │  │   Token     │  │    Invariants   │   │
│  │             │  │   Executor  │  │                 │   │
│  └─────────────┘  └─────────────┘  └─────────────────┘   │
│                          │                                  │
│                          ▼                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Persistence Layer                       │   │
│  │  (in-memory / PostgreSQL)                           │   │
│  │  Runtime Tables │ History │ Timers                   │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
│  External Workers (fetch / lock / complete via REST API)     │
└─────────────────────────────────────────────────────────────┘</pre>
  </div>

  <div class="arch-links">
    <a href="docs/architecture.html">Architecture Overview</a>
    <a href="docs/execution-model.html">Execution Model</a>
    <a href="docs/invariants.html">Invariants</a>
    <a href="docs/recovery.html">Persistence & Recovery</a>
  </div>
</div>

<!-- Workspace Crates -->
<div class="section">
  <h2 class="section-title">Workspace Crates</h2>
  <div class="crates-table">
    <table>
      <thead>
        <tr>
          <th>Crate</th>
          <th>Responsibility</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <td><code>bpm-core</code></td>
          <td>ProcessDefinition, NodeType, Token, EngineEvent, Saga. Pure logic, no I/O.</td>
        </tr>
        <tr>
          <td><code>bpm-storage</code></td>
          <td>Async persistence traits (ProcessInstanceStore, TokenStore, ExternalTaskStore, TimerStore)</td>
        </tr>
        <tr>
          <td><code>bpm-runtime</code></td>
          <td>BpmEngine event loop, EngineContext, event handlers, gateway evaluation</td>
        </tr>
        <tr>
          <td><code>bpm-adapter-memory</code></td>
          <td>In-memory implementations. Default for development/testing</td>
        </tr>
        <tr>
          <td><code>bpm-adapter-postgres</code></td>
          <td>PostgreSQL implementation. Production-ready persistence</td>
        </tr>
        <tr>
          <td><code>bpm-bpmn</code></td>
          <td>BPMN 2.0 XML parser → ProcessDefinition compiler</td>
        </tr>
        <tr>
          <td><code>bpm-server-rest</code></td>
          <td>HTTP API server (axum). Wires EngineContext with storage adapter</td>
        </tr>
        <tr>
          <td><code>bpm-worker-sdk</code></td>
          <td>EngineClient, Worker, TaskHandler. Workers are stateless and horizontally scalable</td>
        </tr>
      </tbody>
    </table>
  </div>
</div>

<!-- REST API -->
<div class="section">
  <h2 class="section-title">REST API</h2>
  <p class="section-desc">Base path: <code>/api/v1</code></p>

  <div class="api-table">
    <table>
      <thead>
        <tr>
          <th>Method</th>
          <th>Path</th>
          <th>Description</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/process-instances</code></td>
          <td>Start instance. Body: <code>{"process_def_id", "variables"?: {}}</code></td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id</code></td>
          <td>Get instance status and current nodes</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id/history</code></td>
          <td>Get execution history (events with sequence and category)</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/process-instances/:id/trace</code></td>
          <td>Get aggregated trace (token timelines and external-task history)</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/process-definitions/deploy</code></td>
          <td>Deploy process from BPMN 2.0 XML (body: raw XML)</td>
        </tr>
        <tr>
          <td><span class="method get">GET</span></td>
          <td><code>/tasks?type=user|external</code></td>
          <td>List waiting tasks</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/tasks/:task_id/complete</code></td>
          <td>Complete user task</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/fetch-and-lock</code></td>
          <td>Worker: fetch and lock tasks</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/:task_id/complete</code></td>
          <td>Worker: complete task</td>
        </tr>
        <tr>
          <td><span class="method post">POST</span></td>
          <td><code>/external-tasks/:task_id/fail</code></td>
          <td>Worker: fail task</td>
        </tr>
      </tbody>
    </table>
  </div>

  <p class="code-note">
    Optional header: <code>x-tenant-id</code> for tenant isolation
  </p>
</div>

<!-- Documentation Links -->
<div class="section">
  <h2 class="section-title">Documentation</h2>
  <div class="docs-grid">

    <div class="doc-card">
      <h3>📖 Core Concepts</h3>
      <ul>
        <li><a href="docs/architecture.html">Architecture Overview</a></li>
        <li><a href="docs/execution-model.html">Execution Model</a></li>
        <li><a href="docs/invariants.html">Formal Invariants</a></li>
        <li><a href="docs/why-correctness.html">Why Correctness Matters</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <h3>🔧 Development</h3>
      <ul>
        <li><a href="docs/quick-start.html">Quick Start Guide</a></li>
        <li><a href="docs/sdk-rust.html">Rust Worker SDK</a></li>
        <li><a href="docs/bpmn.html">BPMN 2.0 Support</a></li>
        <li><a href="docs/api-spec.html">API Specification</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <h3>🛡️ Reliability</h3>
      <ul>
        <li><a href="docs/recovery.html">Persistence & Recovery</a></li>
        <li><a href="docs/accident-scenarios.html">Accident Scenarios</a></li>
        <li><a href="docs/database-schema.html">Database Schema</a></li>
        <li><a href="docs/saga.html">Saga & Compensation</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <h3>❓ Reference</h3>
      <ul>
        <li><a href="docs/faq.html">FAQ</a></li>
        <li><a href="docs/cheat-sheet.html">Cheat Sheet</a></li>
        <li><a href="docs/roadmap.html">Roadmap</a></li>
        <li><a href="https://github.com/fanjia1024/bpm-engine/blob/master/CONTRIBUTING.md">Contributing Guide</a></li>
      </ul>
    </div>

  </div>
</div>

<!-- Project Status -->
<div class="section status-section">
  <h2 class="section-title">Project Status</h2>

  <div class="status-grid">
    <div class="status-item">
      <span class="status-badge status-stable">Stable</span>
      <p>Core execution semantics</p>
    </div>
    <div class="status-item">
      <span class="status-badge status-evolving">Evolving</span>
      <p>APIs may evolve</p>
    </div>
    <div class="status-item">
      <span class="status-badge status-research">Research</span>
      <p>Production use cases</p>
    </div>
  </div>

  <p class="status-note">
    The engine is suitable for: <strong>Research</strong>, <strong>Prototyping</strong>, <strong>Internal Systems</strong>, <strong>Correctness-focused experimentation</strong>
  </p>
</div>
