太好了，这一步**非常像一个真正要做“产品级内核”的人**在问的问题 👍
下面我给你的不是“说明文”，而是**可以直接当作你这个 BPM 框架的「架构设计文档 v0.1」**。

你完全可以照着它，一层一层把代码补齐。

---

# 原生 Rust BPM 流程框架

## 整体架构设计（Architecture Design）

---

## 0️⃣ 设计目标与边界（先立规矩）

### 🎯 设计目标

- **纯 Rust 原生**
- **流程可暂停 / 可恢复**
- **支持人工任务（Human Task）**
- **支持长事务**
- **流程状态可持久化**
- **Engine 与业务逻辑解耦**

### ❌ 明确不做（至少 v1 不做）

- BPMN XML 解析
- 可视化流程建模
- 低代码
- 分布式一致性（先单实例）

👉 这是一个 **BPM Runtime Engine**，不是 BPM Suite。

---

## 1️⃣ 总体架构分层（核心图）

```
┌──────────────────────────────────────┐
│          API / Adapter Layer         │
│  REST / gRPC / CLI / Message Queue   │
└──────────────────────────────────────┘
                ↓
┌──────────────────────────────────────┐
│          Application Layer           │
│  ProcessService / TaskService        │
└──────────────────────────────────────┘
                ↓
┌──────────────────────────────────────┐
│            BPM Engine                │
│  - Token Scheduler                   │
│  - Node Executor                     │
│  - Gateway Evaluator                 │
│  - Event Dispatcher                  │
└──────────────────────────────────────┘
                ↓
┌──────────────────────────────────────┐
│         Persistence Layer             │
│  Repo / Unit of Work / Locking        │
└──────────────────────────────────────┘
                ↓
┌──────────────────────────────────────┐
│        Infrastructure Layer          │
│  DB / Clock / Expression / Logger    │
└──────────────────────────────────────┘
```

---

## 2️⃣ 核心领域模型（Domain Model）

### 2.1 流程定义（静态）

> **流程定义 = 不变的蓝图**

```rust
ProcessDefinition
 ├─ id
 ├─ nodes: Map<NodeId, Node>
 └─ start_node
```

```rust
Node
 ├─ id
 ├─ node_type
 └─ outgoing: Vec<SequenceFlow>
```

```rust
SequenceFlow
 ├─ target_node_id
 └─ condition: Option<Expr>
```

---

### 2.2 流程实例（动态）

> **流程实例 = 运行中的状态机**

```rust
ProcessInstance
 ├─ id
 ├─ process_definition_id
 ├─ state: Running | Completed | Terminated
 ├─ variables
 └─ tokens
```

---

### 2.3 Token（BPM 的核心）

> **Token = 流程中的执行权**

```rust
Token
 ├─ id
 ├─ node_id
 ├─ status: Active | Waiting | Completed
```

**关键设计原则：**

- 一个流程实例可以有 **多个 Token**
- Token 是 **推进流程的唯一实体**

---

### 2.4 人工任务（Human Task）

```rust
UserTaskInstance
 ├─ id
 ├─ process_instance_id
 ├─ node_id
 ├─ assignee / candidate
 ├─ status: Created | Completed
```

**重要原则：**

- UserTask ≠ Token
- UserTask 是 Token 的一种 **阻塞原因**

---

## 3️⃣ BPM Engine 内部结构（最关键）

```
BpmEngine
 ├─ TokenScheduler
 ├─ NodeExecutor
 ├─ GatewayEvaluator
 ├─ EventBus
```

---

### 3.1 TokenScheduler（调度器）

**职责：**

- 找出可执行 Token
- 推进 Token 生命周期

```rust
trait TokenScheduler {
    fn poll(&self) -> Vec<Token>;
}
```

> 这是你将来支持 **异步 / 分布式 / 多实例** 的入口点

---

### 3.2 NodeExecutor（节点执行器）

```rust
trait NodeExecutor {
    fn execute(
        &self,
        node: &Node,
        token: &mut Token,
        ctx: &mut ExecutionContext,
    );
}
```

不同 NodeType 的行为：

| NodeType    | 行为               |
| ----------- | ------------------ |
| Start       | 创建 Token         |
| ServiceTask | 调用业务逻辑       |
| UserTask    | 创建人工任务并暂停 |
| Gateway     | 路由 Token         |
| End         | 完成流程           |

---

### 3.3 GatewayEvaluator（网关）

```rust
trait GatewayEvaluator {
    fn evaluate(
        &self,
        gateway: &GatewayDef,
        ctx: &ExecutionContext,
    ) -> Vec<NodeId>;
}
```

- Exclusive Gateway：选一条
- Parallel Gateway：fork / join（v2）

---

### 3.4 ExecutionContext（执行上下文）

```rust
ExecutionContext
 ├─ process_instance
 ├─ variables
 ├─ services
 └─ clock
```

> 所有业务逻辑都只能通过 Context 访问世界

---

## 4️⃣ 事件驱动模型（非常重要）

### 4.1 Engine 是事件驱动的，不是 while 循环

```text
Event
 ├─ ProcessStarted
 ├─ TokenArrived
 ├─ UserTaskCompleted
 ├─ TimerFired
```

```rust
Engine::handle(event)
```

👉 这是你后面加 **消息队列 / 定时器** 的基础。

---

## 5️⃣ 持久化架构设计（v1 推荐）

### 5.1 Repository 模式

```rust
trait ProcessInstanceRepo {
    fn load(id: &str) -> ProcessInstance;
    fn save(instance: &ProcessInstance);
}
```

```rust
trait TokenRepo { ... }
trait UserTaskRepo { ... }
```

---

### 5.2 数据一致性策略

- **流程推进 = 一个事务**
- Optimistic Lock（version）
- Token 状态是并发边界

---

## 6️⃣ API / Application 层设计

### 6.1 流程控制 API

```text
POST /process/{id}/start
POST /task/{id}/complete
GET  /process/{id}
```

### 6.2 应用层职责

- 参数校验
- 权限
- 调用 Engine
- 不写流程逻辑

---

## 7️⃣ 可扩展点（你未来一定会用到）

| 扩展点            | 说明        |
| ----------------- | ----------- |
| Expression Engine | Rhai / CEL  |
| Timer             | 定时器节点  |
| Compensation      | 反向节点    |
| History           | 审计 / 监控 |
| BPMN              | 最外层适配  |

---

## 8️⃣ 推荐 crate 结构（你可以直接用）

```text
bpm-core/
├── domain/
│   ├── process.rs
│   ├── instance.rs
│   └── token.rs
├── engine/
│   ├── mod.rs
│   ├── scheduler.rs
│   ├── executor.rs
│   └── gateway.rs
├── persistence/
│   ├── repo.rs
│   └── memory.rs
├── api/
│   └── service.rs
```

---

## 9️⃣ 架构设计的“底层哲学”（很重要）

> **BPM ≠ 调度系统** > **BPM = 状态推进引擎**

你这个框架成功与否，取决于三件事：

1. Token 模型是否清晰
2. UserTask 是否彻底解耦
3. Engine 是否事件驱动

---

## 下一步建议（非常具体）

**你现在最该做的是：**

1️⃣ 把 `BpmEngine` 拆成 Scheduler / Executor
2️⃣ 给 Token 加 `status` 和 `version`
3️⃣ 把 UserTask 独立成子模块

如果你愿意，下一步我可以直接帮你做一件“架构级”的事：

- 👉 **画一张「Token 生命周期状态图」**
- 👉 **设计 Engine 事件模型（Event + Handler）**
- 👉 **把这个架构映射成 trait + struct 的 Rust 接口**

你选一个，我继续陪你把这个框架“打磨成真东西”。
