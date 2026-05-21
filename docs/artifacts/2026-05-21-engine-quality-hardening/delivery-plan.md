# Delivery Plan: Engine Quality Hardening

> 基于 PRD 及四项已确认决策，制定可执行的交付计划。

---

## 版本目标

- **版本**: v0.2.1（质量硬化版）
- **范围**: Timer 调度器实现、遗留清理、Postgres 完善、文档与 observability
- **放行标准**: `cargo test --workspace` 全通过 + `cargo clippy --workspace -- -D warnings` 零警告 + Timer 集成测试 + Postgres 集成测试

---

## 需求挑战会结论

### 核心假设与质疑

| # | 假设 | 质疑人 | 质疑内容 | 结论 |
|---|------|--------|----------|------|
| A1 | EngineContext 需要大规模重构才能解耦 | architect | 当前代码已使用 `Arc<dyn Trait>` + `Option<>` 包装，解耦工作量被高估 | **接受**：实际只需移除 `Option` 并提供 builder，影响面可控 |
| A2 | Timer 调度器可以简单地用 `tokio::spawn` + `loop` 实现 | architect | crash recovery 要求 timer 持久化，启动时需重新加载所有 pending timer | **修正**：启动时先 scan `list_due`，然后进入周期 poll loop |
| A3 | 删除 `src/legacy_engine.rs` 即可完成遗留清理 | backend-engineer | `src/` 目录含 46 个 legacy 文件，包括 `db.rs`（rusqlite）、`persistence/sqlite.rs` 等 | **扩展**：需删除整个 `src/` 中的遗留模块或将整个 root crate 精简为 re-export facade |
| A4 | `deploy/schema.sql` 直接修改无风险 | project-manager | 内嵌在 Postgres adapter `migrate()` 函数中的 schema 也需同步 | **确认**：两处（`deploy/schema.sql` + `lib.rs::migrate()`）必须同步修改 |

### 替代路径（已排除）

- **Generic EngineContext**（Q1 排除）：会使 handler 注册变复杂，失去 trait object 的灵活性
- **独立 timer crate**（Q2 排除）：当前 timer 逻辑量小（< 200 行），独立 crate 过度抽象
- **v2 migration**（Q4 排除）：v0.2 未正式发布，无向后兼容用户

### 未决项

无。所有待确认项均已在 intake 阶段决策完毕。

---

## Brownfield 上下文快照

| 维度 | 现状 |
|------|------|
| 工作区结构 | 8 个 workspace crate + 1 个 root crate（遗留） |
| EngineContext | 已用 `Arc<dyn Trait>`，但字段全为 `Option`，构造时无校验 |
| Timer | `TimerStore` trait 完整（`get_by_id` / `mark_fired` / `insert` / `list_due`），runtime 中 `scheduler.rs` 仅有 token-level poll，无 timer 执行循环 |
| Postgres | 7/8 store impl 已完成，缺 `ProcessDefinitionStore`；`migrate()` 缺 `process_definition` 表 |
| Legacy `src/` | 46 个文件，包含同步 Engine、rusqlite persistence、DSL loader、cluster 模块等被取代代码 |
| Schema | `deploy/schema.sql` 用 TEXT 时间戳，但 timer.due_at 为 BIGINT；Postgres adapter 内嵌 migrate 用 `VARCHAR(100)` |
| Doc comments | workspace crate pub items 约 40% 有文档 |
| observability | feature flag 声明存在，但无任何 metrics 注册或 exporter 代码 |

---

## Story Slices

### Slice 1: Legacy Cleanup (P0, S)

**目标**: 移除所有废弃代码和幽灵依赖，使 root crate 成为纯 re-export facade。

**验收标准**:
- `src/legacy_engine.rs` 已删除
- `src/persistence/sqlite.rs`、`src/db.rs` 已删除
- `Cargo.toml` 中 `rusqlite` 依赖已移除
- root `src/lib.rs` 仅 re-export workspace crate 的公开 API
- `cargo build --workspace` 通过
- `cargo clippy --workspace --all-targets -- -D warnings` 零警告

**Owner**: backend-engineer
**依赖**: 无
**风险**: 可能有隐藏的 integration test 引用 legacy 路径

---

### Slice 2: Schema Timestamp Unification (P1, S)

**目标**: 统一 `deploy/schema.sql` 和 Postgres adapter `migrate()` 中的时间戳类型为 `TEXT (ISO 8601 UTC)`。

**验收标准**:
- `deploy/schema.sql` 中 `timer.due_at` 从 BIGINT 改为 TEXT
- 所有 `TIMESTAMP` 类型统一为 `TEXT`（ISO 8601）或显式 `TIMESTAMPTZ`
- Postgres adapter `migrate()` 中的 DDL 与 `deploy/schema.sql` 一致
- `TimerStore` 实现与新类型匹配

**Owner**: backend-engineer
**依赖**: 无
**风险**: 低

---

### Slice 3: EngineContext Builder (P1, M)

**目标**: 将 EngineContext 从 `Option<Arc<dyn ...>>` 改为必填字段 + builder pattern，消除运行时 unwrap 风险。

**验收标准**:
- `EngineContext` 字段从 `Option<Arc<dyn T>>` 改为 `Arc<dyn T>`
- 提供 `EngineContextBuilder` 强制校验必填 store（至少 token_store + process_store + process_def_store）
- 所有 handler 中的 `.unwrap()` 或 `if let Some(...)` 简化为直接访问
- REST server 和 examples 使用 builder 构造
- 既有测试全部通过

**Owner**: backend-engineer
**依赖**: Slice 1（清理后避免改动冲突）
**风险**: handler 代码改动面广，需逐文件验证

---

### Slice 4: Timer Scheduler Implementation (P0, M)

**目标**: 实现 timer 执行循环，使 BPMN timer 节点能在到期后自动推进 token。

**验收标准**:
- 新增 `crates/runtime/src/timer_scheduler.rs`
- `TimerScheduler` 作为 `tokio::spawn` 后台任务，周期性调用 `timer_store.list_due()`
- 到期 timer 产生 `EngineEvent::TimerFired` 并送入 EventPump
- 启动时扫描所有 `Scheduled` 状态 timer（crash recovery）
- 集成测试：创建带 Timer 节点的流程 → 等待到期 → 断言 token 到达下一节点
- Timer 精度在 ±2s 内（poll interval = 1s）

**Owner**: backend-engineer
**依赖**: Slice 3（EngineContext 不再 Option，scheduler 直接访问 store）
**风险**: tokio 定时器在高负载下可能延迟；文档声明 best-effort

---

### Slice 5: Postgres ProcessDefinitionStore (P1, M)

**目标**: 补全 Postgres adapter 中缺失的 ProcessDefinitionStore 实现。

**验收标准**:
- `migrate()` 新增 `process_definition` 表（columns: id, name, xml, compiled_json, version, created_at）
- `deploy/schema.sql` 同步新增该表
- `PostgresProcessDefinitionStore` 实现 `ProcessDefinitionStore::load()`
- 新增 `deploy()` 方法用于存储定义（REST server 的 deploy endpoint 需要）
- 编译通过 + 单元测试

**Owner**: backend-engineer
**依赖**: Slice 2（schema 统一后再加表）
**风险**: 低

---

### Slice 6: Postgres Integration Tests (P1, M)

**目标**: 通过 testcontainers 在 CI 中验证 Postgres adapter 全链路正确性。

**验收标准**:
- `crates/adapters/postgres/tests/` 新增集成测试文件
- 使用 `testcontainers` crate 拉起 Postgres 容器
- 覆盖：deploy definition → start instance → external task complete → timer fire → history query
- 所有测试标记 `#[ignore]`，CI 通过 `cargo test -p bpm-engine-adapter-postgres -- --ignored` 执行
- CI workflow 新增独立 job 跑 Postgres 测试

**Owner**: backend-engineer
**依赖**: Slice 5（ProcessDefinitionStore 完成后才能测 deploy 全链路）
**风险**: CI 耗时增加 ~2min

---

### Slice 7: API Documentation (P2, M)

**目标**: 为三个核心 crate 的所有 pub items 补充 `///` 文档注释。

**验收标准**:
- `bpm-engine-core`、`bpm-engine-storage`、`bpm-engine-runtime` pub items 100% 有 doc comment
- 至少 5 个 `/// # Example` 代码块
- `cargo doc --no-deps --workspace` 无警告
- 在 CI 中增加 `#![deny(missing_docs)]` 或 lint 检查

**Owner**: backend-engineer
**依赖**: Slice 3 + Slice 4（接口稳定后再写文档）
**风险**: 工作量可能超预期；优先覆盖 storage traits 和 core types

---

### Slice 8: Observability Implementation (P2, S)

**目标**: 为 `--features observability` 提供真实的 Prometheus metrics 暴露。

**验收标准**:
- 至少注册 5 个 metrics：`bpm_events_processed_total`、`bpm_tokens_active`、`bpm_external_tasks_pending`、`bpm_timer_fired_total`、`bpm_engine_errors_total`
- 开启 feature 后暴露 `/metrics` endpoint（Prometheus text format）
- 不开启 feature 时无额外依赖和开销
- README 中说明 metrics 使用方式

**Owner**: backend-engineer
**依赖**: Slice 4（timer metrics 需要 scheduler 存在）
**风险**: 低

---

## 执行顺序与依赖图

```
Slice 1 (Legacy Cleanup)     Slice 2 (Schema)
       │                          │
       ▼                          ▼
Slice 3 (EngineContext)     Slice 5 (PG DefStore)
       │                          │
       ▼                          ▼
Slice 4 (Timer Scheduler)  Slice 6 (PG Tests)
       │
       ▼
Slice 7 (Docs)   Slice 8 (Observability)
```

**并行路径**:
- Path A: Slice 1 → Slice 3 → Slice 4 → Slice 7/8
- Path B: Slice 2 → Slice 5 → Slice 6

两条路径可并行开发，在 Slice 6（集成测试）时合流验证。

---

## 角色分工

| 角色 | 职责 |
|------|------|
| tech-lead | 整体排期、PR review、放行决策 |
| backend-engineer | 所有 Slice 实现 |
| qa-engineer | Slice 4/6 的集成测试补充、invariant 验证 |

---

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| Legacy `src/` 删除范围比预期大 | 可能破坏 root crate 的公开 API | 先列出 root crate 当前导出的 pub items，确保 workspace crate 已覆盖 |
| EngineContext builder 改动辐射广 | handler + server + examples 全需改 | 分两步：先加 builder 保持 Option 兼容，再一次性切换 |
| Timer 测试不稳定（时间敏感） | CI flaky | 使用 `tokio::time::pause()` 控制时间推进 |
| Postgres testcontainer 在 CI 中拉取慢 | CI 耗时 | 缓存 Docker image；设置 timeout |

---

## 检查节点

| 节点 | 条件 | 主责 |
|------|------|------|
| Slice 1 完成 | `cargo build` 不依赖 rusqlite，clippy 零警告 | backend-engineer |
| Slice 3+4 完成 | Timer 集成测试通过，EngineContext 无 Option | backend-engineer + qa-engineer |
| Slice 5+6 完成 | Postgres 全链路测试通过 | backend-engineer |
| 全部完成 | 所有 Slice 验收标准达成 + CI green | tech-lead |

---

## Implementation Readiness

| 前提 | 状态 |
|------|------|
| PRD 完成 | ✅ |
| 核心假设已挑战 | ✅ 4 项假设已质疑并收敛 |
| 待确认项已决策 | ✅ Q1-Q4 全部确认 |
| Design review | ✅ EngineContext 已用 trait object，Timer 设计明确 |
| 阻塞项 | 无 |

**结论**: `handoff-ready`，可进入 `/team-execute`。

---

## 技能装配清单

| 能力 | 类型 | 触发原因 | 主责 |
|------|------|----------|------|
| `rust-patterns` | shared | trait object 设计、builder pattern | backend-engineer |
| `rust-testing` | shared | testcontainers 集成测试 | backend-engineer |

---

## 不涉及

- UI 变更
- 前端交付物
- 企业治理约束（开源项目）
