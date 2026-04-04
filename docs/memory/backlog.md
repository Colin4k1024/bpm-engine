# BPM Engine Backlog Snapshot

## 快照信息

- **来源**: bpm-engine-evolution-plan (Sprint 1-4 执行完成)
- **更新时间**: 2026-04-04
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
| P1-4 | Saga 补偿顺序未验证 | ✅ 完成（5 测试） |

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

---

## 未完成项（按优先级）

### P1 — 中期处理

| ID | 问题 | 处理方案 | 状态 |
|----|------|----------|------|
| P1-6 | PostgreSQL 适配器完整实现 | 完成剩余 traits（TimerStore, ExternalTaskStore 等） | pending |

### P3 — 长期处理

| ID | 问题 | 处理方案 | 状态 |
|----|------|----------|------|
| P3-1 | src/legacy_engine.rs 未移除 | 删除 + lib.rs 更新 | pending |
| P3-2 | Auth & multi-tenant | PostgreSQL 后再考虑 | pending |
| P3-3 | Dashboard / visualization | 需要 PostgreSQL | pending |
| P3-4 | Python Worker SDK | Rust SDK 稳定后 | pending |
| P3-5 | Invariants tooling | — | pending |

---

## 技术债

| 项目 | 优先级 | 说明 |
|------|--------|------|
| 1 个残留 .bak 文件 | P3 | 需手动确认 |
| src/legacy_engine.rs | P3 | 与新架构并存 |
| in-memory fallback 状态丢失 | H2 | 接受限制 |

---

## 测试覆盖率

| 阶段 | 目标 | 实际 | 状态 |
|------|------|------|------|
| Sprint 1 末 | 30% | ~27% | ⚠️ |
| Sprint 2 末 | 45% | ~55% | ✅ |
| Sprint 4 末 | 70% | ~70%+ | ✅ |

---

## ADR 状态

| ADR | 状态 |
|-----|------|
| ADR-001 | ✅ implemented — TOCTOU 修复 + 并发测试 |
| ADR-002 | ✅ implemented — 方案 B + 语义测试 |

---

## 开源准备度

**~90%** — 准备就绪

| 问题 | 状态 |
|------|------|
| P0 fetch_and_lock TOCTOU | ✅ 修复 + 测试 |
| H1 ParallelJoin group_id | ✅ 方案 B + 测试 |
| API contract 文档 | ✅ 完成 |
| Doc-tests | ✅ 启用 |
| PostgreSQL adapter | ✅ 核心完成 |

---

## 下一阶段候选

| 项目 | 触发条件 | 优先级 |
|------|----------|--------|
| E2E smoke + chaos 测试 | Sprint 4 结束后 | P2 |
| 开源发布准备 | 当前 | P1 |
| Python Worker SDK | Rust SDK 稳定后 | P3 |

---

## 真相源

本文件为 backlog 真相源，与 delivery-plan.md 保持同步。
跨任务以本文档为准。
