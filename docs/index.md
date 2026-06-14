---
layout: default
title: bpm-engine
description: A correctness-first, token-driven workflow execution kernel in Rust
lang: en
---

<!-- Hero Section -->
<section class="hero">
  <div class="hero-content">
    <div class="hero-badge">
      <span class="hero-badge-dot"></span>
      <span>v0.2.0 · Rust · Apache-2.0</span>
    </div>
    <h1 class="hero-title">
      <span class="hero-title-gradient">bpm-engine</span>
    </h1>
    <p class="hero-subtitle">
      A correctness-first, token-driven workflow execution kernel in Rust.<br>
      Designed for deterministic replay and crash-safe long-running processes.
    </p>
    <div class="hero-actions">
      <a href="#getting-started" class="btn btn-primary">
        <svg class="btn-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polygon points="5 3 19 12 5 21 5 3"></polygon>
        </svg>
        Quick Start
      </a>
      <a href="https://github.com/fanjia1024/bpm-engine" class="btn btn-secondary" target="_blank">
        <svg class="btn-icon" viewBox="0 0 16 16" fill="currentColor">
          <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
        </svg>
        GitHub
      </a>
    </div>
  </div>
</section>

<!-- Why bpm-engine -->
<section class="section" id="why">
  <div class="section-header">
    <span class="section-tag">Why bpm-engine?</span>
    <h2 class="section-title">Correctness Over Features</h2>
    <p class="section-desc">
      Most BPM engines optimize for <strong>features and modeling UX</strong>. This engine optimizes for <strong>correctness</strong>.
      Every execution step is driven by database state. Every state transition is recorded as history.
    </p>
  </div>
</section>

<!-- Core Features -->
<section class="section" id="features">
  <div class="section-header">
    <span class="section-tag">Core Guarantees</span>
    <h2 class="section-title">Built for Reliability</h2>
    <p class="section-desc">
      Six fundamental guarantees that make bpm-engine suitable for mission-critical workflows.
    </p>
  </div>
  
  <div class="features-grid">
    <div class="feature-card">
      <div class="feature-icon">⚡</div>
      <h3>Exactly-Once Completion</h3>
      <p>Tokens reach final states exactly once. No duplicate executions, no lost state. The engine guarantees atomic completion.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔄</div>
      <h3>Crash-Safe by Design</h3>
      <p>Engine recovers deterministically by replaying events. Kill the process, restart, and continue exactly where you left off.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🔒</div>
      <h3>Lease-Based External Tasks</h3>
      <p>External workers fetch tasks by topic. Leases guarantee exactly one owner at a time with automatic timeout handling.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">⏱️</div>
      <h3>Persistent Timers</h3>
      <p>No in-memory timers. All timers are persisted and survive restarts. Naturally scalable across multiple instances.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">📜</div>
      <h3>Full Audit History</h3>
      <p>Every state change emits an event. History is append-only and globally ordered. Perfect for compliance and debugging.</p>
    </div>

    <div class="feature-card">
      <div class="feature-icon">🛡️</div>
      <h3>Formal Invariants</h3>
      <p>Join nodes wait for all branches. Retries are monotonic. Core behavior is mathematically proven through invariants.</p>
    </div>
  </div>
</section>

<!-- Getting Started -->
<section class="section" id="getting-started">
  <div class="section-header">
    <span class="section-tag">Quick Start</span>
    <h2 class="section-title">Up and Running in 5 Minutes</h2>
    <p class="section-desc">
      Get started with bpm-engine. No Docker required for the default in-memory backend.
    </p>
  </div>
  
  <div class="code-section">
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
  Built-in process definitions: <code>minimal</code> (Start → End), <code>payment-flow</code> (Start → ExternalTask → End)
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
use bpm_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, 
    TaskHandler, TaskResult, Worker, WorkerConfig
};

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
</section>

<!-- Architecture -->
<section class="section" id="architecture">
  <div class="section-header">
    <span class="section-tag">Architecture</span>
    <h2 class="section-title">Simple Yet Powerful</h2>
    <p class="section-desc">
      A clean, layered architecture that separates concerns and enables flexibility.
    </p>
  </div>

  <div class="arch-diagram">
    <pre>
┌─────────────────────────────────────────────────────────────────┐
│                        bpm-engine                               │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │   Scheduler  │  │    Token     │  │     Invariants       │  │
│  │              │  │   Executor   │  │                      │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│                            │                                    │
│                            ▼                                    │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   Persistence Layer                       │  │
│  │            (in-memory / PostgreSQL)                       │  │
│  │    Runtime Tables │ History │ Timers │ External Tasks     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│    External Workers (fetch / lock / complete via REST API)      │
└─────────────────────────────────────────────────────────────────┘
    </pre>
  </div>

  <div class="arch-links">
    <a href="architecture.html">📖 Architecture Overview</a>
    <a href="execution-model.html">⚙️ Execution Model</a>
    <a href="invariants.html">🛡️ Invariants</a>
    <a href="recovery.html">💾 Persistence & Recovery</a>
  </div>
</section>

<!-- Workspace Crates -->
<section class="section" id="crates">
  <div class="section-header">
    <span class="section-tag">Modular Design</span>
    <h2 class="section-title">Workspace Crates</h2>
    <p class="section-desc">
      A well-organized workspace with clear responsibilities for each crate.
    </p>
  </div>
  
  <div class="table-wrapper">
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
</section>

<!-- REST API -->
<section class="section" id="api">
  <div class="section-header">
    <span class="section-tag">API Reference</span>
    <h2 class="section-title">REST API</h2>
    <p class="section-desc">
      Clean, RESTful API for all engine operations. Base path: <code>/api/v1</code>
    </p>
  </div>

  <div class="table-wrapper">
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

  <p class="code-note" style="text-align: center; margin-top: 24px;">
    Optional header: <code>x-tenant-id</code> for tenant isolation
  </p>
</section>

<!-- Documentation Links -->
<section class="section" id="docs">
  <div class="section-header">
    <span class="section-tag">Documentation</span>
    <h2 class="section-title">Learn More</h2>
    <p class="section-desc">
      Comprehensive documentation to help you understand and use bpm-engine effectively.
    </p>
  </div>
  
  <div class="docs-grid">
    <div class="doc-card">
      <div class="doc-card-icon">📖</div>
      <h3>Core Concepts</h3>
      <ul>
        <li><a href="architecture.html">Architecture Overview</a></li>
        <li><a href="execution-model.html">Execution Model</a></li>
        <li><a href="invariants.html">Formal Invariants</a></li>
        <li><a href="why-correctness.html">Why Correctness Matters</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">🔧</div>
      <h3>Development</h3>
      <ul>
        <li><a href="quick-start.html">Quick Start Guide</a></li>
        <li><a href="sdk-rust.html">Rust Worker SDK</a></li>
        <li><a href="bpmn.html">BPMN 2.0 Support</a></li>
        <li><a href="api-spec.html">API Specification</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">🛡️</div>
      <h3>Reliability</h3>
      <ul>
        <li><a href="recovery.html">Persistence & Recovery</a></li>
        <li><a href="accident-scenarios.html">Accident Scenarios</a></li>
        <li><a href="database-schema.html">Database Schema</a></li>
        <li><a href="saga.html">Saga & Compensation</a></li>
      </ul>
    </div>

    <div class="doc-card">
      <div class="doc-card-icon">❓</div>
      <h3>Reference</h3>
      <ul>
        <li><a href="faq.html">FAQ</a></li>
        <li><a href="cheat-sheet.html">Cheat Sheet</a></li>
        <li><a href="roadmap.html">Roadmap</a></li>
        <li><a href="https://github.com/fanjia1024/bpm-engine/blob/master/CONTRIBUTING.md">Contributing Guide</a></li>
      </ul>
    </div>
  </div>
</section>

<!-- Project Status -->
<section class="section">
  <div class="status-section">
    <div class="section-header">
      <span class="section-tag">Project Status</span>
      <h2 class="section-title">Current Status</h2>
    </div>

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
</section>
