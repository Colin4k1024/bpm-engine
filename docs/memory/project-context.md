# Project Context

## 项目信息

- **项目名**: bpm-engine
- **类型**: 开源 Rust BPM 引擎
- **当前任务**: bpm-engine-evolution-plan（已关闭）
- **任务日期**: 2026-04-04
- **任务状态**: closed

## Tech Stack

- **语言**: Rust (2021 edition)
- **架构**: Token-driven, Event-sourced BPM Engine
- **关键依赖**:
  - `async-trait` 0.1 — async trait 语法
  - `axum` 0.7 — REST API server
  - `uuid` 1.x — token/group ID 生成
  - `rusqlite` 0.31 — 可选 SQLite 支持
  - `tokio-postgres` + `deadpool-postgres` — PostgreSQL 适配器

## 工作区结构

```
crates/core          # 纯逻辑，无 I/O
crates/storage      # Async storage traits
crates/runtime      # BpmEngine 事件循环
crates/adapters/memory  # In-memory storage
crates/adapters/postgres # PostgreSQL storage（新增）
crates/bpmn         # BPMN 2.0 XML parser
crates/server/rest  # HTTP API（/api/v1/）
crates/worker-sdk   # External task worker
```

## 当前状态

**演进规划已完成** — 所有 Sprint 1-4 工作项已完成

| 指标 | 值 |
|------|---|
| 总测试数 | 104 |
| 测试覆盖率 | ~70% |
| ADR 状态 | ADR-001 implemented, ADR-002 implemented |
| PostgreSQL 适配器 | 核心 traits 已实现 |
| Doc-tests | 9 个 trait 已启用 |

## 活跃风险

| 风险 | 级别 | 状态 |
|------|------|------|
| fetch_and_lock TOCTOU | P0 | ✅ ADR-001 implemented |
| ParallelJoin group_id 语义 | P0 | ✅ ADR-002 implemented（方案 B） |
| in-memory fallback 状态丢失 | H2 | ⚠️ 接受限制 |
| EL 表达式不支持负数 | H3 | ✅ 已修复 |

## 技术债

| 项目 | 优先级 | 状态 |
|------|--------|------|
| 12 个 .bak 文件 | P2 | ✅ 已删除 |
| 0 doc-tests | P2 | ✅ 已启用（9 个 trait） |
| src/legacy_engine.rs | P3 | pending |
| PostgreSQL 适配器完整实现 | P1 | pending（核心已完成） |

## 下一步

1. **短期**：无紧急项
2. **中期**：PostgreSQL 适配器完整实现
3. **长期**：开源发布准备、Dashboard、Python Worker SDK

## 已完成交付物

- `docs/artifacts/2026-04-04-bpm-engine-evolution-plan/` — 演进规划全套产出物
- `docs/adr/ADR-001-fetch-and-lock-race.md` — P0 bug ADR（implemented）
- `docs/adr/ADR-002-parallel-join-semantics.md` — H1 设计 ADR（implemented）
