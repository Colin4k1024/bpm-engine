# Python Worker SDK (planned)

A **Python Worker SDK** for external tasks is **planned** but not yet implemented.

---

## Current status

- **Rust Worker SDK** is available and documented: see [sdk-rust.md](sdk-rust.md) and [crates/worker-sdk](crates/worker-sdk).
- Python workers can integrate with the engine today by calling the **REST API**:
  - `POST /api/v1/external-tasks/fetch-and-lock` — fetch and lock tasks
  - `POST /api/v1/external-tasks/:task_id/complete` — complete task
  - `POST /api/v1/external-tasks/:task_id/fail` — fail task

See [api-spec.md](api-spec.md) for the API contract.

---

## Roadmap

- A dedicated Python SDK (client + poll loop + handler interface) may be added in a future release.
- Until then, use the REST API from Python (e.g. `requests` or `httpx`) with the same payloads and semantics as the Rust SDK.
