---
layout: default
---

# Roadmap

Short-term and long-term direction for the BPM engine, aligned with [opti.md](opti.md) priorities.

---

## Stability window (current phase)

Until we have real usage feedback, we **do not** add new BPMN features, performance optimizations, UI, or multi-language SDKs. New ideas belong in **issues**; priority is to observe where users get stuck. This phase is **convergence, not expansion**.

---

## Current (v0.1)

* Core execution semantics: token state machine, event-driven handlers, CAS claim
* In-memory storage adapter (MemoryRepo, ProcessDefStore)
* REST API: deploy BPMN, start instances, external task fetch-and-lock/complete/fail
* Rust Worker SDK (EngineClient, Worker, TaskHandler)
* BPMN 2.0 XML parser and compiler (supported subset)
* Formal invariants and testing strategy
* Documentation: architecture, execution model, database schema, recovery, FAQ, cheat sheet, SDK docs
* CI: fmt, clippy, tests (GitHub Actions)

---

## Next (short-term)

* **Postgres adapter**: Implement ProcessInstanceStore, TokenStore, TimerStore, ExternalTaskStore (and related repos) for PostgreSQL; schema versioning
* **BPMN test set**: Automated validation against a BPMN spec test set; document supported vs unsupported elements
* **Observability**: Wire existing `observability` feature to Prometheus/OpenTelemetry (token claim rate, task lock duration, scheduler latency); optional Grafana dashboard example
* **More tests**: External task lock timeout/reclaim, scheduler boundary (timer + retry), additional invariant tests

---

## Later (medium / long-term)

* **Auth & multi-tenant**: Optional API key / JWT; tenant isolation; rate limiting; RBAC for management and external-task APIs
* **Dashboard / visualization**: Read-only process instance view (diagram + token highlight, task list, history timeline); e.g. BPMN.io or Mermaid integration
* **Invariants tooling**: Invariant violation detection API; replay trace comparison report
* **Python Worker SDK**: Client and poll loop for external tasks (see [sdk-python.md](sdk-python.md))

---

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md). Good first steps: add an invariant test, improve docs or examples, or implement a small feature from this roadmap.
