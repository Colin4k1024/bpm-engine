# 测试覆盖率与质量审查报告

## 1. 测试覆盖率分析

### 1.1 测试数量统计

| 层级 | 位置 | 测试数量 |
|------|------|----------|
| 集成测试 | `tests/` | 6 |
| BPMN 编译器 | `crates/bpmn/src/lib.rs` | 14 |
| Memory 适配器 | `crates/adapters/memory/src/memory_repo.rs` | 3 |
| 其他 Crate 单元测试 | core/runtime/storage/worker-sdk | 0 |
| **合计** | | **23** |

**Doc-tests**: 0（所有 crate 的 doc-tests 数量均为 0）

### 1.2 关键路径覆盖分析

| 关键函数 | 路径 | 覆盖状态 |
|----------|------|----------|
| `fetch_and_lock` | `crates/adapters/memory/src/memory_repo.rs:417` | 仅单次 happy path 测试，无并发竞争测试 |
| `claim_token` | `crates/adapters/memory/src/memory_repo.rs:175` | 有 CAS 并发测试（`only_one_claim_succeeds`），但仅限 token 层 |
| `try_join` | `crates/storage/src/parallel_join.rs:6` | **无单元测试**，依赖集成测试覆盖 |
| `claim_token` (token 层) | `crates/runtime/src/token_arrived_handler.rs:54` | 有并发测试验证 exactly-one claim |

### 1.3 覆盖缺口

**严重缺口：**
1. **ExternalTask fetch_and_lock 并发测试缺失** — 当前只有单次 `fetch_and_lock` 测试，没有验证 N workers 并发 fetch 同一 task_type 时的行为
2. **ParallelJoin 语义无单元测试** — `try_join` 边界条件（expected=1、expected=0、group_id 不匹配）未覆盖
3. **Saga 补偿路径无测试** — `integration_saga.rs.bak` 存在但未启用，saga 补偿逻辑从未被验证

**中等缺口：**
4. 各种 NodeType handler 的边界条件（ServiceTask 死代码路径、UserTask 超时、ExclusiveGateway 默认分支）
5. Error path 测试几乎为零（BPMN 验证有 8 个 error case 测试，但 runtime handlers 无 error 测试）

---

## 2. Invariant 保护分析

### 2.1 Token Exactly-Once 完成

**Invariant**: 每个 token 在其生命周期内只能完成一次（status 从 Ready → Executing → 完成）

**当前实现**:
- `TokenArrivedHandler` 第 54-61 行调用 `claim_token` 做 CAS
- `claim_token` 在单个 WriteLock 内检查 `status == Ready && version == expected`，然后原子修改

**测试充分性**: **部分充分**
- `only_one_claim_succeeds` 测试了 16 个并发 claim 但仅有一个成功
- 但未测试：token 已Executing、token 已 Waiting、version 不匹配 等失败路径

### 2.2 Parallel Fork/Join 语义

**Invariant**: ParallelJoin 必须等待所有 expected 数量的 tokens 到达后才能继续

**当前实现** (`token_arrived_handler.rs:158-196`):
```rust
NodeType::ParallelJoin { expected } => {
    let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
        join_repo.try_join(&group_id).await.unwrap_or(false)
    } else {
        // FALLBACK TO IN-MEMORY - crash = state loss  <-- P1 问题
        let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
        let mut state = self.join_state.lock().unwrap();
        // ...
    };
    // ...
}
```

**测试充分性**: **不充分**
- 无 `try_join` 的独立单元测试
- 无 `expected=1` 的边界测试
- 无 `group_id` 不匹配的异常测试
- 无 in-memory fallback 路径的测试

### 2.3 External Task 单一 Owner

**Invariant**: 任意时刻，一个 external task 只能被一个 worker 持有（Locked 状态）

**当前实现** (`memory_repo.rs:417-452`):
```rust
// TOCTOU: Read (line 428) 和 Write (line 443) 之间无原子性
let order: Vec<_> = {
    let guard = self.external_tasks.read().unwrap();  // Step 1: Read
    // ...
};
// ... 其他 workers 可能在这里修改同一 task 状态 ...
let mut guard = self.external_tasks.write().unwrap();  // Step 2: Write
for task_id in take {
    if let Some(r) = guard.get_mut(&task_id) {
        r.state = ExternalTaskState::Locked;  // 可能已被其他 worker 修改
    }
}
```

**测试充分性**: **不充分** (P0 缺陷)
- ADR-001 记录的 TOCTOU 竞态导致此 invariant **未被保证**
- 现有 `external_task_store_create_fetch_lock_complete` 测试仅验证单次获取
- **缺少并发 fetch_and_lock 测试**：应验证 N workers 同时 fetch 同一 task_type，同一 task 永远只被一个 worker 获得

---

## 3. 技术债清单

### 3.1 .bak 文件清单

| 文件路径 | 引用废弃 API | 建议操作 |
|----------|--------------|----------|
| `examples/approval.rs.bak` | `InstanceRepo` (应为 `MemoryRepo`), `process_repo`/`token_repo` (应为 `process_store`/`token_store`) | **删除** — examples 已迁移到新 API |
| `examples/el_gateway.rs.bak` | 同上旧 API | **删除** |
| `examples/exclusive_gateway.rs.bak` | 同上旧 API | **删除** |
| `examples/leave_request.rs.bak` | 同上旧 API | **删除** |
| `examples/minimal.rs.bak` | 同上旧 API | **删除** |
| `examples/parallel_fork_join.rs.bak` | 同上旧 API | **删除** |
| `examples/reject_path.rs.bak` | 同上旧 API | **删除** |
| `examples/service_task_chain.rs.bak` | 同上旧 API | **删除** |
| `tests/integration_concurrent_token.rs.bak` | `bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore}` (旧路径) | **删除** — 当前 `integration_concurrent_token.rs` 已使用正确路径 |
| `tests/integration_outbox.rs.bak` | `bpm_engine::persistence::{MemoryRepo, OutboxRepo}` (不存在模块) | **删除** — 引用不存在的 re-export，代码从未有效 |
| `tests/integration_recovery.rs.bak` | 旧版 `recover()` API，与当前 `recovery::recover` 接口不同 | **删除** — 当前 `integration_recovery.rs` 已使用正确 API |
| `tests/integration_saga.rs.bak` | `bpm_engine::persistence` (不存在), `SagaCoordinator` handler | **删除** — saga 补偿路径从未通过集成测试验证 |

**结论**: 所有 13 个 .bak 文件均应删除。它们：
1. 引用不存在的 API（`bpm_engine::persistence`）
2. 或已被当前有效测试文件替代
3. 不承载任何有价值的历史信息

### 3.2 0 Doc-Tests 问题

**当前状态**: 所有 crate 的 doc-tests 数量为 0

**影响**:
- 公开 API 无使用示例
- 文档中的代码示例无法被验证（可能过时或错误）
- 违反 Rust 社区对 library 的基本期望

**建议**: 为以下公开 API 添加 doc-tests:
- `bpm_engine_core`: `ProcessDefinition`, `Token`, `EngineEvent` 等核心类型
- `bpm_engine_runtime`: `BpmEngine::new`, `EngineContext`, `EventHandler` trait
- `bpm_engine_storage`: 各 trait 的方法签名和示例

### 3.3 其他质量问题

| 级别 | 问题 | 位置 | 建议 |
|------|------|------|------|
| P2 | `NodeType::ServiceTask` 死代码 | `crates/core/src/node.rs:22` | 该变体从未被使用，应删除或标记为 `#[deprecated]` |
| P1 | In-memory `join_state` fallback | `token_arrived_handler.rs:139-142` | 与文档承诺 "Persistence over memory" 矛盾，crash 后 parallel join 无法恢复 |
| P2 | HistoryHandler 读取原始 event | `arch-design.md` 第 69 行 | 非原子，handler 顺序敏感 |

---

## 4. 测试质量评估

### 4.1 Happy Path vs 边界/失败路径

| 类别 | 覆盖评估 |
|------|----------|
| Happy path | **中等** — 主要流程有覆盖（最小流程、BPMN 验证、并发 claim） |
| 边界条件 | **弱** — `try_join` expected=1/0、`claim_token` version 不匹配等边界未覆盖 |
| 失败路径 | **弱** — 无 handler error path 测试、无存储失败模拟测试 |

### 4.2 并发测试覆盖

| 场景 | 覆盖状态 |
|------|----------|
| Token claim 并发（16 workers） | **有** — `only_one_claim_succeeds` |
| ExternalTask fetch_and_lock 并发 | **缺失** — TOCTOU 竞态未被测试验证 |
| ParallelJoin 并发 tokens | **缺失** — 多 token 同时到达同一 join 的场景未测试 |

### 4.3 文档示例覆盖

| 类型 | 当前状态 |
|------|----------|
| Doc tests | **0** — 所有 crate 无 doc-tests |
| Examples | **7 个** — 但均已迁移到新 API（.bak 文件需删除） |
| Integration tests (tests/) | **6 个** — smoke + 3 个 BPMN 验证 + 1 个并发 + 1 个恢复 |

---

## 5. 总结

| 维度 | 评估 |
|------|------|
| 测试总数 | 23 个（14 BPMN + 3 Memory + 6 集成） |
| Doc-tests | 0 |
| 关键路径覆盖 | Token claim 充分；fetch_and_lock 不充分；try_join 缺失 |
| Invariant 保护 | Token exactly-once 部分充分；ExternalTask 单一 owner 存在 P0 竞态；ParallelJoin 语义测试缺失 |
| 并发测试 | 仅 token claim 有并发测试；fetch_and_lock 和 join 并发缺失 |
| 技术债 | 13 个 .bak 文件需清理；0 doc-tests 需补充 |

**最高优先级修复项**:
1. **P0**: 补充 `fetch_and_lock` 并发测试，验证 TOCTOU 竞态已修复
2. **P0**: 修复 `fetch_and_lock` TOCTOU 竞态（ADR-001 方案 A 或 B）
3. **P1**: 补充 `try_join` 单元测试和边界条件测试
4. **P2**: 删除所有 13 个 .bak 文件
5. **P2**: 补充 doc-tests（至少覆盖核心 public API）