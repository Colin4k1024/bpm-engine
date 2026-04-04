---
artifact: final-report
task: bpm-engine-review
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 项目审查 — 最终报告

## 执行摘要

对 `bpm-engine` 项目进行了全方面只读审查，涵盖架构一致性、BPMN 解析、测试覆盖率、文档完整性、技术债 5 个维度。

**CI 状态**：`cargo fmt ✓` `cargo clippy ✓` `cargo test (23 tests) ✓`

---

## 问题优先级总览

| 级别 | 数量 | 代表问题 |
|------|------|----------|
| **Critical** | 1 | ExternalTask `fetch_and_lock` TOCTOU 竞态 |
| **High** | 3 | ParallelJoin 语义错误、in-memory fallback 状态丢失、EL 不支持负数 |
| **Medium** | 6 | Handler 顺序约束未文档化、.bak 文件、0 doc-tests 等 |
| **Low** | 3 | workspace 依赖声明不规范等 |

---

## Critical/High 问题详解

### P0 — fetch_and_lock TOCTOU 竞态

**位置**：`crates/adapters/memory/src/memory_repo.rs` 第 417-455 行

**问题**：`fetch_and_lock` 的 select(ReadLock) 和 lock(WriteLock) 非原子，多 worker 同时调用时同一 task 可能被两个 worker 获得，违背"外部任务单一 owner" invariant。

**修复建议**：将 Read + Write 合并为单个 WriteLock 临界区（ADR-001 已创建）。

---

### H1 — ParallelJoin group_id 语义错误

**位置**：`crates/runtime/src/token_arrived_handler.rs` 第 158-196 行；`crates/bpmn/src/compiler.rs` 第 396-402 行

**问题**：当两个独立 ParallelFork 的输出汇聚到同一 Join 时，各自 token 携带不同 `group_id`，Join 使用到达 token 的 `group_id` 判断完成，导致永远等待。

**修复建议**：明确 ParallelJoin 只接收来自唯一一个 ParallelFork 的 token，或修改 group_id 语义（ADR-002 已创建）。

---

### H2 — in-memory fallback parallel join 状态丢失

**位置**：`crates/runtime/src/token_arrived_handler.rs` 第 166-177 行

**问题**：`TokenArrivedHandler` 的 `join_state: Mutex<HashMap<...>>` 完全存在于内存中，crash 后无法恢复。与文档承诺的 "Persistence over memory" 矛盾。

**修复建议**：
1. 确保 `parallel_join_repo` 始终被传入
2. 或将 in-memory fallback 状态也持久化
3. 在文档中明确说明 MemoryRepo 不保证 crash 恢复

---

### H3 — EL 表达式不支持负数

**位置**：`crates/runtime/src/el.rs` 第 100-113 行

**问题**：`parse_f64` 不支持负数字面量（如 `-5`），因为 `-` 被 Rust 的 unary minus 处理，而非作为数字的一部分。

**修复建议**：在 EL 表达式解析时，对数字前的 `-` 进行特殊处理。

---

## 测试覆盖率分析

| 指标 | 值 | 说明 |
|------|---|------|
| 总测试数 | 23 | 14 BPMN + 3 Memory适配器 + 6 集成测试 |
| Doc-tests | 0 | 所有 crate 无文档测试 |
| 关键路径覆盖 | 部分 | `fetch_and_lock` 无并发测试；`try_join` 无边界测试 |

**Invariant 保护评估**：

| Invariant | 覆盖充分性 |
|-----------|-----------|
| Token exactly-once | 部分（claim_token 有 CAS，但失败路径未测） |
| ExternalTask 单一 owner | **不充分**（TOCTOU 竞态存在） |
| ParallelJoin 语义 | **不充分**（无独立测试） |

---

## 技术债清单

| 类型 | 数量 | 说明 |
|------|------|------|
| `.bak` 文件 | 13 个 | 均引用废弃 API 或已被有效测试替代，建议删除 |
| 死代码 | 1 | `NodeType::ServiceTask(fn(...))` 从未被赋值 |
| 文档不一致 | 2 | `docs/bpmn-spec-mapping.md` 与代码不一致 |
| 0 doc-tests | 全 workspace | 需要为 core/runtime/storage 公开 API 补充 |

---

## 文档与代码一致性

| 文档 | 实际代码 | 不一致 |
|------|----------|--------|
| `docs/bpmn-spec-mapping.md` | `ServiceTask → ExternalTask` | **是** — 文档说 `ServiceTask → ServiceTask` |
| `docs/architecture.md` | Handler 遍历顺序无文档约束 | **是** |
| `docs/recovery.md` | in-memory `join_state` 违反 "Memory contains no critical state" | **是** |

---

## 架构验证结论

| 维度 | 结论 |
|------|------|
| 模块边界 | ✅ `crates/core` 确实无 I/O（无 async-trait/tokio） |
| 依赖方向 | ✅ `core` → `storage` ← `runtime`，方向正确 |
| 存储抽象 | ✅ traits 注入，支持 in-memory 和 PostgreSQL |
| Cargo.toml | ⚠️ `bpmn` 和 `adapters/memory` 未使用 workspace 依赖声明 |
| 遗留代码 | ⚠️ `src/legacy_engine.rs` 与新架构并存，未被移除 |

---

## 建议优先级排序

### 立即修复（不影响当前功能）

1. **删除 13 个 `.bak` 文件** — 避免误导后续开发者
2. **更新 `docs/bpmn-spec-mapping.md`** — 与实际编译器行为一致
3. **为 `NodeType::ServiceTask` 添加 deprecation warning 或删除** — 消除死代码

### 短期改进（不影响稳定性）

4. **补充 doc-tests** — 为 `BpmEngine::run_async`、`ExternalTaskStore::fetch_and_lock`、`CompensationRecordRepo` 补充文档示例
5. **补充 `try_join` 边界测试** — expected=0/1 的 corner case
6. **文档化 Handler 顺序约束** — 在 `design/handler.md` 中补充

### 中期重构（需要仔细测试）

7. **修复 `fetch_and_lock` TOCTOU** — 合并为原子 WriteLock
8. **澄清 ParallelJoin group_id 语义** — 或在文档中明确限制
9. **移除 `src/legacy_engine.rs`** — 避免与新架构混淆

---

## 工件清单

| 文件 | 说明 |
|------|------|
| `prd.md` | 需求简报 |
| `delivery-plan.md` | 交付计划 + 需求挑战会结论 |
| `arch-design.md` | 架构审查记录 |
| `review-report.md` | 架构一致性报告 |
| `bpmn-review.md` | BPMN 解析审查 |
| `quality-report.md` | 测试覆盖率与质量报告 |
| `final-report.md` | 本文档 |
| `ADR-001-fetch-and-lock-race.md` | ExternalTask TOCTOU ADR |
| `ADR-002-parallel-join-semantics.md` | ParallelJoin 语义 ADR |

---

## 结论

bpm-engine 核心架构设计合理，token-driven + event-sourced 模式与设计文档一致。主要风险集中在：

1. **P0 bug**：`fetch_and_lock` TOCTOU 竞态 — 多 worker 场景下违背 external task 单一 owner invariant
2. **H1 设计问题**：ParallelJoin group_id 语义在复杂拓扑下会失效
3. **H2 持久化矛盾**：in-memory fallback 违反 crash-safe 承诺

测试覆盖率需要提升，特别是并发路径和边界条件。当前代码适合**研究与原型阶段**，生产使用前需要解决 P0 和 H1 问题。
