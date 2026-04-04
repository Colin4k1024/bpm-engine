# CLAUDE_zh.md

本文件为 Claude Code (claude.ai/code) 在此仓库工作时提供指导。

## 项目概览

`bpm-engine` 是一个用 Rust 编写的** token 驱动、持久化优先的 BPM 引擎**。它将长流程执行为持久化状态机，其中：
- 每个执行步骤都由数据库状态驱动
- 每个状态转换都记录为历史
- 执行天然是 crash-safe 的
- 外部任务使用基于租约的执行
- 定时器完全持久化

## 构建与测试命令

```bash
# 构建所有 crates
cargo build

# 运行所有测试（workspace）
cargo test --workspace

# 运行特定 crate 的测试
cargo test -p bpm-core
cargo test -p bpm-runtime

# 运行特定测试
cargo test -p bpm-core -- test_token_name

# 格式化代码
cargo fmt

# Lint（警告即失败）
cargo clippy --workspace --all-targets -- -D warnings
```

## 本地运行

```bash
# 终端 1：启动 REST 服务器（http://127.0.0.1:3000）
cargo run -p bpm-server-rest

# 终端 2：运行简单流程示例
cargo run --example simple_process

# 终端 3：运行 payment worker（外部任务）
cargo run -p bpm-worker-sdk --example payment
```

## Workspace Crates

| Crate | 职责 |
|-------|------|
| `crates/core` | 核心语义：ProcessDefinition、NodeType、Token、EngineEvent、Saga。**无 I/O，无存储。**不要随意修改。 |
| `crates/storage` | 异步持久化 traits（ProcessInstanceStore、TokenStore、ExternalTaskStore、TimerStore 等） |
| `crates/runtime` | BpmEngine 事件循环、EngineContext、事件处理器、网关评估。仅依赖存储 traits。 |
| `crates/adapters/memory` | 存储 traits 的内存实现。开发/测试默认使用。 |
| `crates/bpmn` | BPMN 2.0 XML 解析器 → ProcessDefinition 编译器 |
| `crates/server/rest` | HTTP API 服务器（axum）。将 EngineContext 与内存适配器连接。 |
| `crates/worker-sdk` | 外部任务 worker 运行时：EngineClient、Worker、TaskHandler。Worker 无状态、可水平扩展。 |

## 核心抽象

**Token** — 执行单元。代表在特定节点执行的授权。多个 token 实现并行。Token 状态转换被持久化。

**EngineEvent** — 驱动所有状态转换的不可变事件。处理器是确定性和事务性的。保证可观测性、可重放性和 crash 安全。

**ProcessInstance** — 持有 token 和变量的运行时容器。有生命周期：Running → Completed/Terminated。

**ExternalTask** — 委托给外部 worker 的工作。受租约保护（同一时间只有一个拥有者）。支持重试、超时和 crash 处理。

## 关键入口点

- `BpmEngine::run_async(event, &mut ctx)` — 主事件循环（crates/runtime）
- `EngineContext` — 持有所有存储引用，由服务器/嵌入器构造
- `bpm_server_rest::serve()` — HTTP 服务器入口点

## 设计原则

- **Token 优于 thread** — 并发以 token 为作用域
- **Event 优于调用栈** — 所有状态转换都是事件驱动的
- **补偿优于回滚** — Saga 模式实现长流程一致性
- **持久化优于内存** — 所有状态都被持久化；引擎通过重放恢复

## 不变式

引擎强制形式化不变式（见 `docs/invariants.md`）：
- Token 只能到达最终状态一次
- Join 节点只有在所有分支都完成时才完成
- 外部任务同时只有一个拥有者
- 重试是单调的

## 重要约束

- `crates/core` 是**纯逻辑**——无 I/O、无 async、无存储 traits。随意修改会影响所有下游 crates。
- REST 服务器默认使用**内存适配器**（无数据库）。生产环境需实现基于 PostgreSQL 的存储 traits（见 `docs/database-schema.md`）。
- CI 强制要求：`cargo fmt`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace`

## 特性标志

- `api` — 启用基于 axum 的 REST 服务器（`bpm-server-rest` crate）
- `observability` — 启用 metrics + Prometheus 导出器

## 关键文档

- `docs/architecture.md` — 运行时架构和设计原则
- `docs/execution-model.md` — Token 生命周期和并发模型
- `docs/invariants.md` — 引擎保证的形式化不变式
- `docs/recovery.md` — Crash 恢复机制
- `docs/database-schema.md` — 持久化 schema 参考
