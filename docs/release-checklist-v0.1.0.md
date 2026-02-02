# v0.1.0 Release Checklist

Use this checklist before tagging v0.1.0. Each item should be verifiable.

---

## 1. API & semantic freeze

- [ ] **docs/api-spec.md** contains the section **API & Semantic Stability** (Section 8) with the stable API list and History/Trace semantics (no breaking changes).
- [ ] **README** or api-spec clearly states the commitment scope from v0.1.0 (e.g. link to api-spec § API & Semantic Stability).

**How to verify:** Open [docs/api-spec.md](api-spec.md), search for "API & Semantic Stability" and "Stable (v0.1.0)"; confirm process-instances, history, trace, and external-task APIs are listed.

---

## 2. Observability and auditability

- [ ] **GET /history** and **GET /trace** behavior matches the spec; History API Semantics (append-only, globally ordered, replay same state, backward-compatible) are documented in api-spec and README.
- [ ] **Invariant violations** in REST 4xx include `X-Invariant-Violation` header and (optionally) response body field `invariant_violation`; logs record invariant kind when present.

**How to verify:** Call external-task complete with wrong worker_id; check response for `X-Invariant-Violation` and body `invariant_violation`; check logs for "invariant violation".

---

## 3. Worker responsibility boundary

- [ ] **docs/sdk-rust.md** includes the **Worker Responsibility Contract** section (redelivery, idempotent handler, no non-rollbackable side effects without own deduplication/compensation).
- [ ] **README** External Task and payment example comment stress idempotency and worker responsibility.

**How to verify:** Open [docs/sdk-rust.md](../sdk-rust.md), search for "Worker Responsibility Contract"; open [crates/worker-sdk/examples/payment.rs](../crates/worker-sdk/examples/payment.rs) and confirm idempotency comment.

---

## 4. Deploy and verification

- [ ] **deploy/README.md** contains the 30-second verification checklist (steps 1–7).
- [ ] **deploy/verify-recovery.sh** exists, is executable, and performs kill → restart → GET instance and history (or **deploy/verify-recovery.md** with copy-paste commands). Exit code 0 on success, non-zero on failure.

**How to verify:** Run `./deploy/verify-recovery.sh` from repo root (engine must be buildable); or follow deploy/README.md checklist manually.

---

## 5. Tests and CI

- [ ] **CI** (format, clippy, tests, invariant tests) is green.
- [ ] **No known blocking bugs** (optional: list or link to issues; otherwise state "none").

**How to verify:** Run `./scripts/ci.sh` (or equivalent); run `cargo test` and invariant tests.

---

## 6. Documentation and entry points

- [ ] **README** has **Where to start reading the code** and **Observability APIs** (with link to API & Semantic Stability).
- [ ] **CHANGELOG** or release notes draft lists v0.1.0 main capabilities and stability commitment (e.g. stable REST APIs, History/Trace semantics, invariant violation header).

**How to verify:** Open README, search for "Where to start" and "Observability APIs"; confirm CHANGELOG or release notes mention v0.1.0 and stable APIs.

---

## Sign-off

- [ ] All sections above checked.
- [ ] Tag and release (e.g. `git tag v0.1.0` and push; create GitHub release with notes).
