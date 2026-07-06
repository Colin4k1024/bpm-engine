# BPM Engine Backlog Snapshot

## 快照信息

- **来源**: bpm-engine-evolution-plan (Sprint 1-4 执行完成) + quality-hardening + DLQ/extend_lock
- **更新时间**: 2026-07-05 (PRD 全部 In Scope 完成 + HIGH 风险项修复)
- **更新角色**: tech-lead

---

## Sprint 1-4 完成状态

### Sprint 1 ✅

| ID | 问题 | 状态 |
|----|------|------|
| P0-1 | fetch_and_lock 并发测试 | ✅ 完成 |
| H3-1 | EL 表达式不支持负数 | ✅ 完成 |
| H3-2 | try_join expected=0/1 边界 | ✅ 完成 |
| P1-1 | API contract 文档缺失 | ✅ 完成 |

### Sprint 2 ✅

| ID | 问题 | 状态 |
|----|------|------|
| P0-2 | ParallelJoin 方案 B 实现 | ✅ 完成 |
| P1-2 | Token 状态机无单元测试 | ✅ 完成（21 测试） |
| P1-3 | ParallelJoin 语义测试不足 | ✅ 完成（8 测试） |
| P1-4 | Saga 补偿顺序未验证 | ✅ 完成（7 测试，含并行分支补偿） |

### Sprint 3 ✅

| ID | 问题 | 状态 |
|----|------|------|
| P1-5 | PostgreSQL 适配器缺失 | ✅ 完成（核心 2 traits） |
| P2-3 | Crash recovery 测试不足 | ✅ 完成（3 测试） |
| P2-4 | Outbox 消息测试缺失 | ✅ 完成（3 测试） |

### Sprint 4 ✅

| ID | 问题 | 状态 |
|----|------|------|
| P2-1 | 13 个 .bak 文件 | ✅ 完成（删除 12 个） |
| P2-2 | Doc-tests 缺失（0 个） | ✅ 完成（9 个 trait 加 doc-tests） |
| P2-5 | External task multi-worker 测试 | ✅ 完成（5 测试） |
| P2-6 | Token exactly-once 幂等性 | ✅ 完成（6 测试） |
| P1-7 | Dead Letter Queue（全栈） | ✅ 完成（storage + memory/PG + REST + 集成测试） |
| P1-8 | extend_lock 长任务锁续期 | ✅ 完成（store + REST + Rust/Python SDK） |
| P1-9 | Schema 对齐 | ✅ 完成（deploy/schema.sql = migrate()） |

---

## 已修复的测试计划 HIGH 风险项

| 风险项 | 修复内容 | 状态 |
|--------|----------|------|
| H2: external_task_complete 绕过事件循环 | 创建 `ExternalTaskCompletedHandler`，重构 REST handler 使用事件循环 | ✅ 完成 |
| P0-C: anyhow 错误泄露4xx响应 | 定义 `ExternalTaskError` 类型化错误枚举，内部错误返回通用消息 | ✅ 完成 |

---

## 未完成项（按优先级）

### P1 — 中期处理

| ID | 问题 | 处理方案 | 状态 |
|----|------|----------|------|
| P1-6 | PostgreSQL 适配器完整实现 | 全部 10 个 store traits 已实现 | ✅ 完成 |
| P1-7 | Dead Letter Queue（全栈） | storage trait → memory/PG impl → REST routes → fail 集成 | ✅ 完成 |
| P1-8 | extend_lock 长任务锁续期 | ExternalTaskStore + REST + Rust/Python SDK | ✅ 完成 |
| P1-9 | Schema 对齐 | parent_instance_id, node_id, def_key, version, status | ✅ 完成 |

### P2 — 短期处理

| ID | 问题 | 处理方案 | 状态 |
|----|------|----------|------|
| P2-7 | API 文档注释 | core/storage/runtime pub items 补 `///` + `warn(missing_docs)` | ✅ 完成（235→0 missing docs） |

### P3 — 长期处理

| ID | 问题 | 处理方案 | 状态 |
|----|------|----------|------|
| P3-1 | src/legacy_engine.rs 未移除 | 已清理，src/lib.rs 为纯 re-export facade | ✅ 完成 |
| P3-2 | Auth & multi-tenant | PostgreSQL 后再考虑 | pending |
| P3-3 | Dashboard / visualization | 需要 PostgreSQL | pending |
| P3-4 | Python Worker SDK | 核心完成（client + worker + handler + extend_lock） | ✅ 完成 |
| P3-5 | Invariants tooling | storage trait + memory impl + REST endpoint + 13 tests | ✅ 完成 |

---

## 技术债

| 项目 | 优先级 | 说明 |
|------|--------|------|
| 1 个残留 .bak 文件 | P3 | 需手动确认 |
| API doc comments 未完整 | P2 | ✅ 已完成 — core/storage/runtime 全部补完，`warn(missing_docs)` 已启用 |
| in-memory fallback 状态丢失 | H2 | 接受限制 |

---

## 测试覆盖率

| 阶段 | 目标 | 实际 | 状态 |
|------|------|------|------|
| Sprint 1 末 | 30% | ~27% | ⚠️ |
| Sprint 2 末 | 45% | ~55% | ✅ |
| Sprint 4 末 | 70% | ~70%+ | ✅ |
| 当前 | — | 269 tests, 0 failures | ✅ |

---

## ADR 状态

| ADR | 状态 |
|-----|------|
| ADR-001 | ✅ implemented — TOCTOU 修复 + 并发测试 |
| ADR-002 | ✅ implemented — 方案 B + 语义测试 |

---

## 开源准备度

**~100%** — Sprint 3-4 全部完成，质量硬化全部完成

| 问题 | 状态 |
|------|------|
| P0 fetch_and_lock TOCTOU | ✅ 修复 + 测试 |
| H1 ParallelJoin group_id | ✅ 方案 B + 测试 |
| API contract 文档 | ✅ 完成 |
| Doc-tests | ✅ 启用 |
| PostgreSQL adapter | ✅ 全部 10 个 store 完成 |
| Dead Letter Queue | ✅ 全栈完成 |
| extend_lock 长任务支持 | ✅ Rust + Python SDK |
| Schema 对齐 | ✅ deploy/schema.sql = migrate() |
| API doc comments | ✅ 完成（core/storage/runtime `warn(missing_docs)` 启用） |
| Observability | ✅ 完成（Prometheus metrics + `/metrics` endpoint + event pump/timer 集成） |
| CHANGELOG | ✅ 完成（Unreleased + 0.2.0 + 0.1.0） |
| PG adapter README | ✅ 完成（连接配置 + 示例 + 测试说明） |
| Invariants tooling | ✅ 完成（storage trait + memory impl + REST endpoint + 13 tests） |

---

## 下一阶段候选

| 项目 | 触发条件 | 优先级 |
|------|----------|--------|
| E2E smoke + chaos 测试 | ✅ 完成（8 测试） | — |
| 开源发布准备 | 当前 | P1 |
| API doc comments + warn(missing_docs) | ✅ 完成 | — |
| Observability (Prometheus metrics) | ✅ 完成 | — |
| CHANGELOG | ✅ 完成 | — |
| PG adapter README | ✅ 完成 | — |
| Invariants tooling | ✅ 完成 | — |
| Auth & multi-tenant | 开源发布后 | P3 |
| Dashboard / visualization | 开源发布后 | P3 |

---

## 真相源

本文件为 backlog 真相源，与 delivery-plan.md 保持同步。
跨任务以本文档为准。
