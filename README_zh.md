# bpm-engine

**Rust 语言实现的正确性优先工作流执行内核，专为确定性重放和 crash-safe 长流程设计。**

本项目专注于**执行语义、持久化正确性和 crash 安全**，而非 UI 或 low-code 特性。它被设计为一个** token 驱动、持久化优先的 BPM 引擎**，具有形式化定义的不变式。

---

## 这是什么？

`bpm-engine` 是一个用 Rust 实现的**工作流 / BPM 执行引擎**。

其核心思想是将流程执行为**持久化的 token 状态机**，其中：

- 每个执行步骤都由数据库状态驱动
- 每个状态转换都记录为历史
- 每个执行都可以被重放和验证
- 并发、重试和崩溃是首要考虑因素

这使得引擎适用于**长运行、分布式和易故障的工作流**。

---

## 为什么要另一个 BPM 引擎？

大多数 BPM 引擎以**特性和建模体验为优化目标**。

本引擎是一个**正确性优先的工作流执行内核**：以**正确性为优化目标**。

具体而言：

- Token 状态是**显式且持久化的**
- 执行**天然是 crash-safe 的**
- 外部任务使用**基于租约的执行**
- 定时器**完全持久化**
- 所有执行都是**可审计和可重放的**
- 核心行为受**形式化不变式保护**

如果你关心流程为什么到达某个状态——而不仅仅是它到达了——这个引擎适合你。

---

## 何时不使用 bpm-engine

本引擎以**正确性和可审计性为首要目标**。在以下情况下考虑替代方案：

- **你需要 low-code BPMN 建模和表单设计器**——使用 Camunda 或类似平台，它们开箱即提供可视化建模和任务 UI。
- **你严重依赖复杂的人工工作流和审批 UI**——本引擎专注于执行和语义，不提供内置的任务列表或表单。
- **执行语义不重要，只需要"快"或简单的 DAG**——更轻量的选项（如 AWS Step Functions）可能更简单。

如果你的优先级是**正确性、重放和清晰的执行语义**，本引擎是一个好选择。

---

## 核心概念

### 流程与实例

- **流程定义**是一张不可变的执行图
- **流程实例**是运行时 token 的容器

### Token

Token 代表执行单元。

- 每个 token 都有清晰的生命周期
- 状态转换被持久化
- 并行性通过 token 分叉和汇合来建模

### 外部任务

外部任务允许将工作交由外部 worker 执行：

- Worker 按主题获取任务
- 任务受**租约**保护
- 重试、超时和崩溃由引擎处理
- **引擎**保证 exactly-once token 完成；**worker** 至少一次且**必须**实现幂等处理器

### 定时器

定时器是持久的，由调度器驱动：

- 无内存中定时器
- 跨重启安全
- 自然可扩展

### 历史与重放

- 每个状态变更都会发出历史事件
- 执行可以确定性重放
- 历史可用于调试、审计和验证

**可观测性 API：**

- **执行历史**：`GET /api/v1/process-instances/:id/history`——返回带有 `sequence` 和 `category`（instance | token | external）的事件，用于审计和调试。
- **聚合追踪**：`GET /api/v1/process-instances/:id/trace`——token 时间线和外部任务历史，提供高层视图。

**历史 API 语义：** 事件是追加式的；序列在每个实例内全局有序；重放会重现相同的 token 状态；schema 一旦发布向后兼容。API 稳定性和历史/追踪语义保证：见 [api-spec.md](docs/api-spec.md)（§ API & Semantic Stability，§ History & Trace Semantic Guarantees）。

**Crash 恢复验证：** 要验证 kill → 重启后的正确性（无重复完成、有序历史），请按照 [deploy/README.md](deploy/README.md) 操作，并从仓库根目录运行 `./deploy/verify-recovery.sh`。关于事故驱动的叙述（支付超时 → worker 重启 → 幂等完成，以及为何不变式成立），见 [docs/accident-scenarios.md](docs/accident-scenarios.md)。

### 不变式

引擎强制形式化不变式，例如：

- Token 只能到达最终状态一次
- Join 节点只有在所有分支都完成时才完成
- 外部任务同时只有一个拥有者
- 重试是单调的

详见 [docs/invariants.md](docs/invariants.md)。关于与 Camunda、Temporal 和 AWS Step Functions 的语义比较，见 [docs/why-correctness.md](docs/why-correctness.md)。

---

## 架构概览

```
+-------------------+
| Process Engine    |
| ----------------- |
| Scheduler         |
| Token Executor    |
| Invariants        |
+-------------------+
        |
        v
+-------------------+
| Persistence       |
| (in-memory / DB)  |
| Runtime Tables    |
| History / Timers  |
+-------------------+

External Workers (fetch / lock / complete via API)
```

持久化层是**单一真相来源**。默认后端是**内存**（快速启动无需数据库）。引擎可以通过重新运行调度器来恢复。关于使用 PostgreSQL 的持久化导向部署，见 [docs/recovery.md](docs/recovery.md) 和 [docs/database-schema.md](docs/database-schema.md)。

### 代码阅读入口

- **引擎入口**：`bpm-engine-runtime::BpmEngine::run_async`
- **Token 转换**：`crates/runtime/src/handler/*`（及相关处理器）
- **持久化边界**：`bpm-engine-storage` traits（process, token, history, external task, timer）
- **历史发出**：`EngineEvent` 和 `Engine` 中的 `HistoryHandler`；REST 中的 `GET .../history`
- **不变式**：[docs/invariants.md](docs/invariants.md) 和 `tests/invariant_*.rs`

---

## 快速开始（5 分钟）

**要求：** Rust（stable）。默认内存后端无需 Docker。

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

**1. 启动引擎**

```bash
cargo run -p bpm-server-rest
```

服务器监听 **http://127.0.0.1:3000**。内置流程定义：`minimal`（Start → End）、`payment-flow`（Start → ExternalTask `payment` → End）。

**2. 运行最小流程（Start → End）**

在另一个终端，启动实例并轮询直到完成：

```bash
cargo run --example simple_process
```

或使用 curl：

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'
# 然后 GET /api/v1/process-instances/:id 直到状态为 COMPLETED
```

**3. 运行带有外部任务的流程（payment）**

启动流程实例：

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
```

运行 payment worker（在第三个终端）：

```bash
cargo run -p bpm-worker-sdk --example payment
```

Worker 轮询引擎，锁定 `payment` 任务，运行处理器，然后完成；流程继续到 End。

---

## 示例：外部任务 Worker

```rust
use bpm_worker_sdk::{EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig};

struct PaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str { "payment" }
    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // 业务逻辑
        let mut variables = std::collections::HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}

// Worker：无状态、crash-safe、水平可扩展
let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(PaymentHandler)
    .config(WorkerConfig::new("worker-1").poll_interval(std::time::Duration::from_secs(1)))
    .build();
worker.start().await;
```

完整示例见 [crates/worker-sdk/examples/payment.rs](crates/worker-sdk/examples/payment.rs)。

---

## 保证

本引擎提供以下保证：

- **Exactly-once token 完成**
- **Crash-safe 执行**
- **确定性重放**
- **持久化定时器**
- **形式化不变式**在测试中验证

这些保证是**设计目标**，而非尽力而为的行为。

---

## 使用与 API

### REST API（基础路径 `/api/v1`）

| 方法 | 路径 | 描述 |
|--------|------|-------------|
| POST | `/process-instances` | 启动实例。Body: `{ "process_def_id", "variables"?: {} }` |
| GET | `/process-instances/:id` | 获取实例状态和当前节点 |
| POST | `/process-definitions/deploy` | 从 BPMN 2.0 XML 部署流程（body: raw XML） |
| GET | `/tasks?type=user\|external` | 列出等待中的任务 |
| POST | `/tasks/:task_id/complete` | 完成用户任务 |
| POST | `/external-tasks/fetch-and-lock` | Worker：获取并锁定任务 |
| POST | `/external-tasks/:task_id/complete` | Worker：完成任务 |
| POST | `/external-tasks/:task_id/fail` | Worker：失败任务 |

可选 header：`x-tenant-id` 用于租户隔离。

### 工作区 crates

- **bpm-core**：ProcessDefinition、NodeType（Start、End、UserTask、ExternalTask、gateways）、Token、ProcessInstance、EngineEvent
- **bpm-storage**：异步 traits（ProcessInstanceStore、TokenStore、ExternalTaskStore 等）
- **bpm-runtime**：BpmEngine、处理器、转换辅助
- **bpm-adapter-memory**：MemoryRepo；内存定义的 ProcessDefStore
- **bpm-bpmn**：BPMN 2.0 XML 解析器和 ProcessDefinition 编译器
- **bpm-server-rest**：HTTP API 服务器
- **bpm-worker-sdk**：EngineClient、Worker、TaskHandler；worker 代码无需 BPM 知识

将引擎作为库使用：按路径依赖上述 crate，构建带有 repo 的 [EngineContext](crates/runtime/src/handler.rs)，然后运行 `BpmEngine::run_async(initial_event, &mut ctx)`。关于接线方式见 [crates/server/rest](crates/server/rest)。

---

## 文档

- [架构概览](docs/architecture.md)
- [执行模型（Token 与并发）](docs/execution-model.md)
- [不变式](docs/invariants.md)
- [持久化与恢复](docs/recovery.md)
- [恢复演示（kill → 重启）](docs/recovery-demo.md)
- [事故级场景](docs/accident-scenarios.md)（payment+retry、fork fail、worker reclaim）
- [数据库 Schema](docs/database-schema.md)
- [Saga 与补偿](docs/docs_saga.md)
- [测试策略](docs/docs_testing_strategy.md)
- [BPMN 映射](docs/bpmn-spec-mapping.md)
- [API 规范](docs/api-spec.md)
- [常见问题](docs/faq.md)
- [速查表](docs/cheat-sheet.md)
- [Rust Worker SDK](docs/sdk-rust.md)
- [Python SDK（计划中）](docs/sdk-python.md)

---

## 项目状态

本项目处于**积极开发中**。

- 核心执行语义已稳定
- API 可能演进
- 尚不推荐用于任务关键的生产环境

也就是说，引擎已适用于：

- 研究
- 原型开发
- 内部系统
- 正确性优先的实验

---

## 路线图

- Worker SDK 稳定化（Rust / Python）
- 只读执行检查器（Cockpit 类 UI）
- 更多不变式覆盖
- 文档与示例
- 历史/重放文档（待定）

---

## 贡献

欢迎贡献。

特别有价值的方向：

- 测试与不变式用例
- 文档
- Worker SDK 人体工程学
- 可视化工具

请见 [CONTRIBUTING.md](CONTRIBUTING.md)。

---

## 许可证

MIT
