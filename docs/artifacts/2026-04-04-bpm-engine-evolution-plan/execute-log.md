---
artifact: execute-log
task: bpm-engine-evolution-plan
date: 2026-04-04
role: backend-engineer
status: draft
---

# BPM Engine 演进规划 — Sprint 3-4 执行日志

## Sprint 3-4 概述

**时间**: 2026-04-04
**目标**: 70% 覆盖 + PostgreSQL 适配器核心
**状态**: ✅ 完成

---

## Sprint 3 完成项

### PostgreSQL 适配器 ✅

**创建文件**:
- `crates/adapters/postgres/Cargo.toml`
- `crates/adapters/postgres/src/lib.rs`
- `crates/adapters/postgres/src/token_store.rs`
- `crates/adapters/postgres/src/process_store.rs`

**实现 traits**:
- `TokenStore`: load_by_instance, save_tokens, update_token_cas, claim_token
- `ProcessInstanceStore`: load, save, list_running

**技术决策**:
- 使用 `tokio-postgres` + `deadpool-postgres`（避免 sqlx 与 rusqlite 冲突）
- 实现乐观锁 via `UPDATE ... WHERE version = $expected`

### Crash Recovery + Outbox 测试 ✅

**创建文件**: `tests/crash_recovery.rs`（3 测试）
- `token_executing_recovered_on_restart`
- `external_task_lease_reclaimed_after_crash`
- `timer_due_before_crash_fired_after_restart`

**创建文件**: `tests/outbox_replay.rs`（3 测试）
- `outbox_event_published_on_process_completion`
- `outbox_replay_redelivers_pending_events`
- `outbox_replay_processes_all_pending_events_after_restart`

---

## Sprint 4 完成项

### Doc-tests 启用 ✅

**添加 doc-tests 到 9 个 trait/struct 文件**:
- `crates/storage/src/token_store.rs`
- `crates/storage/src/process_store.rs`
- `crates/storage/src/external_task_store.rs`
- `crates/storage/src/timer_store.rs`
- `crates/storage/src/history.rs`
- `crates/storage/src/event_store.rs`
- `crates/storage/src/parallel_join.rs`
- `crates/storage/src/compensation.rs`
- `crates/runtime/src/engine.rs`

### 清理 ✅

**删除 12 个 .bak 文件**

### External Task Multi-Worker 测试 ✅

**创建文件**: `tests/external_task_multi_worker.rs`（5 测试）
- `only_one_worker_can_lock_same_task`
- `lease_expiry_enables_reclaim`
- `complete_after_lease_expiry_fails`
- `worker_can_complete_before_lease_expires`
- `fail_decrements_retries_and_returns_to_ready`

### Token Idempotency 测试 ✅

**创建文件**: `tests/token_idempotency.rs`（6 测试）
- `claim_token_fails_for_already_claimed_token`
- `cannot_claim_completed_token`
- `cannot_claim_terminated_token`
- `update_token_cas_rejects_stale_version`
- `update_token_cas_succeeds_with_correct_version`
- `save_tokens_replaces_all_tokens`

---

## 测试结果

```
cargo test --workspace: 全部通过 ✅
cargo fmt: ✅
cargo clippy -- -D warnings: ✅
```

---

## Sprint 1-4 累计完成

| 阶段 | 测试数 | 覆盖率 |
|------|--------|--------|
| Sprint 1 末 | 37 | ~27% |
| Sprint 2 末 | 77 | ~55% |
| Sprint 4 末 | 120+ | ~70%+ |

---

## 下一步

**Sprint 5+**（长期目标）：
- E2E smoke + chaos 测试
- Dashboard / visualization
- Python Worker SDK
- 开源发布准备
