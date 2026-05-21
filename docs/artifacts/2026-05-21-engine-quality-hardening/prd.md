# PRD: Engine Quality Hardening

> 基于三维审核（架构 / 功能完成度 / 开源可信度）的系统性改进计划。

---

## 背景

bpm-engine 已完成核心执行语义（Token 状态机、Event Pump、External Task、Parallel Join、Saga、Replay），但三维审核暴露了若干阻塞生产可用和开源可信度的缺口：

1. **Timer 调度器完全缺失** — 设计文档、schema、storage trait 均已就绪，但无运行时循环驱动到期 timer 转化为 token 事件。
2. **遗留废代码和幽灵依赖** — `legacy_engine.rs`、stub binaries、未使用的 `rusqlite` 依赖。
3. **Postgres 适配器不完整** — `ProcessDefinitionStore` 未实现，集成测试为零。
4. **EngineContext 对 MemoryRepo 硬耦合** — 限制了适配器切换能力。
5. **公开 API 文档注释缺失** — `cargo doc` 产物近乎空白。
6. **observability feature flag 声明未实现** — 误导用户。

---

## 目标与成功标准

| 目标 | 成功标准 |
|------|----------|
| Timer 可运行 | 创建带 Timer 节点的流程实例，Timer 到期后 Token 自动推进至下一节点 |
| 遗留清理 | `cargo build` 不再依赖 `rusqlite`；`src/legacy_engine.rs` 和 stub binaries 不存在 |
| Postgres 生产可用 | 流程定义可持久化到 Postgres；CI 包含 testcontainer 集成测试 |
| EngineContext 解耦 | EngineContext 接受 `dyn StorageRepo` 而非具体 MemoryRepo |
| 文档完整 | 核心 crate（core、storage、runtime）公开 API 100% 有 `///` 注释；docs.rs 可渲染 |
| observability 可用 | 开启 `--features observability` 后能暴露 Prometheus 指标端点 |

---

## 用户故事

### US-1: Timer 到期自动触发

**作为** 流程设计者，**我希望** 在 BPMN 中定义 Timer 节点后，引擎能在指定时间自动推进 Token，**以便** 实现超时回调、定时重试等关键业务场景。

**验收标准：**
- 集成测试：启动带 Timer 节点的流程，等待 Timer 到期，断言 Token 已到达下一个节点
- Timer 精度在 ±2s 内（内存适配器下）
- Crash recovery 后仍能恢复到期未触发的 Timer

### US-2: Postgres 端到端可用

**作为** 运维工程师，**我希望** 使用 Postgres 作为唯一存储后端时，部署 + 启动 + 运行流程 + 重启恢复全链路正常工作，**以便** 在生产环境运行引擎。

**验收标准：**
- 流程定义可 deploy 并从 Postgres 重新加载
- CI 中 `cargo test -p bpm-engine-adapter-postgres` 跑过（需 testcontainer）
- docker-compose up 后引擎可直接使用 Postgres，无额外手工步骤

### US-3: 代码库零误导

**作为** 开源贡献者，**我希望** 仓库内没有废弃代码、幽灵依赖或名不副实的 feature flag，**以便** 快速理解项目结构并安全贡献。

**验收标准：**
- `cargo build --workspace` 无 dead_code 警告
- `rusqlite` 不在依赖树中
- `src/legacy_engine.rs` 已删除或迁移为 `examples/legacy_sync.rs`

### US-4: 公开 API 可查阅

**作为** Rust 开发者，**我希望** 在 docs.rs 上看到完整的 API 文档和用法示例，**以便** 不看源码也能集成引擎。

**验收标准：**
- `bpm-engine-core`、`bpm-engine-storage`、`bpm-engine-runtime` 三个 crate 的 pub items 100% 有 doc comment
- 至少 5 个 `/// # Example` 代码块

---

## 范围

### In Scope

| 优先级 | 工作项 | 预估复杂度 |
|--------|--------|-----------|
| P0 | Timer 调度器实现 | M |
| P0 | 删除 `src/legacy_engine.rs`、stub binaries、rusqlite 依赖 | S |
| P1 | Postgres `ProcessDefinitionStore` 实现 + schema 扩展 | M |
| P1 | EngineContext 泛化（`dyn StorageRepo` 或 trait bound） | M |
| P1 | `deploy/schema.sql` 时间戳类型统一 | S |
| P1 | Postgres adapter testcontainer 集成测试 | M |
| P2 | 公开 API `///` 文档注释 | M |
| P2 | observability feature 真实实现（≥5 个 metric） | S |
| P2 | CHANGELOG 补充近期变更 | S |
| P2 | Postgres adapter README 连接配置示例 | S |

### Out of Scope

- BPMN 高级节点（BoundaryEvent、SubProcess、Multi-instance）
- Python Worker SDK
- 认证 / RBAC
- Dashboard / UI
- 性能调优和压测
- 新增 BPMN 元素支持

---

## 关键假设

1. Timer 调度器作为 `tokio::spawn` 后台任务运行，与 BpmEngine 生命周期绑定。
2. EngineContext 解耦方案选择 trait object（`Arc<dyn TokenStore + ...>`）而非 generic，以保持 handler Vec 可存异构对象。
3. testcontainer 通过 `testcontainers-rs` crate 在 CI 中拉起 Postgres，不依赖外部数据库服务。
4. 遗留代码删除不影响现有 `cargo test --workspace` 的任何测试。
5. 时间戳统一选择 TEXT (ISO 8601 UTC) 作为最终方案，保持与现有 Memory adapter 一致。

---

## 风险与依赖

| 风险 | 影响 | 缓解 |
|------|------|------|
| EngineContext 重构影响面大 | 所有 handler + REST server 均需改动 | 先做接口设计 ADR，再逐步迁移 |
| Timer 精度依赖 tokio 定时器 | 高负载下可能延迟 | 文档声明"best-effort, not real-time"；写 tolerance 测试 |
| testcontainer CI 耗时增加 | 可能从 1min → 3min | 用 `--ignored` 隔离 Postgres 测试，仅 CI 跑 |
| 删除 legacy_engine.rs 可能影响现有用户 | 低（v0.2 未正式发布） | CHANGELOG 记录 breaking change |

---

## 待确认项

| # | 问题 | 决策人 | 状态 | 决策结果 |
|---|------|--------|------|----------|
| Q1 | EngineContext 解耦方案：trait object vs. generic | tech-lead | ✅ 已决策 | 采用 trait object（`Arc<dyn TokenStore + ...>`），保持 handler Vec 可存异构对象 |
| Q2 | Timer 调度器是嵌入 BpmEngine 还是独立 crate | tech-lead | ✅ 已决策 | 嵌入 runtime crate，作为 `tokio::spawn` 后台任务与 BpmEngine 生命周期绑定 |
| Q3 | Postgres 测试是否加入默认 CI（增加耗时）还是仅 nightly | tech-lead | ✅ 已决策 | 用 `#[ignore]` 隔离，CI 通过 `cargo test --workspace -- --ignored` 单独阶段执行 |
| Q4 | `deploy/schema.sql` 迁移策略：直接改还是新增 v2 migration | tech-lead | ✅ 已决策 | 直接修改（v0.2 未正式发布，无向后兼容包袱） |

---

## 参与角色

| 角色 | 职责 |
|------|------|
| tech-lead | 整体排期、ADR 决策、PR review |
| architect | EngineContext 解耦方案设计 |
| backend-engineer | Timer 实现、Postgres adapter、遗留清理 |
| qa-engineer | invariant 测试补充、集成测试验证 |

---

## 企业治理待确认项

不适用 — 本项目为开源项目，无集团架构白皮书约束。

---

## 领域技能包启用建议

| 技能 | 原因 |
|------|------|
| `rust-patterns` | Timer 调度器、trait object 设计 |
| `rust-testing` | testcontainer 集成测试 |
| `api-design` | EngineContext trait 设计 |
| `database-migrations` | schema.sql 版本化 |

---

## UI 范围

不涉及 UI 变更。

---

## 需求挑战会候选分组

| 分组 | 参与者 | 挑战焦点 |
|------|--------|----------|
| Timer 设计组 | tech-lead, architect, backend-engineer | Timer 调度器生命周期、crash recovery 下的语义、精度承诺 |
| 存储解耦组 | tech-lead, architect | EngineContext 泛化路径、是否引入 `StorageRepo` 超级 trait |
