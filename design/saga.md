太好了，这一步**基本决定你这个 BPM 引擎能不能“上生产”**。
我下面给你的是一套 **工程级、事件驱动、和你现有 Token 生命周期完全兼容的 Saga（补偿）设计**，不是概念图。

你可以直接把它当成 **《Saga 子系统设计规范》**。

---

# Saga（补偿）与 Token 的集成设计

---

## 1️⃣ Saga 在 BPM 中的真实定位

一句话定性：

> **Saga ≠ 回滚事务** > **Saga = 失败后，按“已完成节点的反向顺序”执行补偿流程**

在 BPM 里：

- 每个 **可失败的 ServiceTask** 都可能有补偿
- 补偿是 **流程的一部分**
- 补偿也是 **由 Token 驱动的**

---

## 2️⃣ 设计原则（非常重要）

1. 🔄 补偿不是立即执行，是一个**子流程**
2. 📜 补偿必须**显式建模**
3. 🔁 补偿流程也走 **Token 生命周期**
4. 🧱 补偿和正向流程 **状态隔离**

---

## 3️⃣ 流程定义层：补偿怎么建模

### 3.1 ServiceTask 扩展

```rust
struct ServiceTaskDef {
    id: NodeId,
    execute: ServiceFn,
    compensate: Option<ServiceFn>,
    retry_policy: Option<RetryPolicy>,
}
```

👉 **有没有补偿 = 这个节点是否参与 Saga**

---

### 3.2 流程级 Saga 边界

```rust
enum SagaScope {
    None,
    Local,     // 仅当前子流程
    Global,    // 整个流程
}
```

```rust
struct ProcessDefinition {
    saga_scope: SagaScope,
}
```

---

## 4️⃣ Token 扩展（补偿专用字段）

```rust
struct Token {
    id: TokenId,
    node_id: NodeId,
    status: TokenStatus,
    mode: TokenMode,
}
```

```rust
enum TokenMode {
    Forward,      // 正向流程
    Compensation, // 补偿流程
}
```

---

## 5️⃣ Saga 执行记录（这是核心）

### 5.1 Compensation Log（必不可少）

```rust
struct CompensationRecord {
    instance_id: ProcessInstanceId,
    node_id: NodeId,
    compensate: ServiceFn,
    status: CompensationStatus,
    order: u32,
}
```

```rust
enum CompensationStatus {
    Pending,
    Completed,
    Failed,
}
```

> **只记录“已经成功执行过”的节点**

---

### 5.2 记录时机（非常关键）

| 时机             | 行为                    |
| ---------------- | ----------------------- |
| ServiceTask 成功 | 写入 CompensationRecord |
| ServiceTask 失败 | 不记录                  |
| Retry 成功       | 写入                    |
| Retry 失败       | 不写                    |

---

## 6️⃣ 失败触发 Saga 的机制

### 6.1 失败事件

```rust
EngineEvent::TokenFailed(TokenFailed)
```

```rust
struct TokenFailed {
    instance_id: ProcessInstanceId,
    token_id: TokenId,
    node_id: NodeId,
    reason: FailureReason,
}
```

---

### 6.2 Saga 启动条件

Saga 启动，当：

- TokenFailed 且
- retry_policy exhausted 且
- saga_scope ≠ None

---

## 7️⃣ Saga 启动流程（事件级）

```
TokenFailed
   ↓
SagaStarted
   ↓
CompensationTokenCreated (reverse order)
```

---

## 8️⃣ 补偿 Token 的创建规则（最重要）

### 8.1 顺序规则（严格）

> **补偿顺序 = 成功顺序的反向**

```text
Service A → B → C
失败在 C
补偿顺序：C → B → A
```

---

### 8.2 创建补偿 Token

```rust
for record in compensation_records.rev() {
    create Token {
        node_id: record.node_id,
        mode: Compensation,
        status: Ready,
    }
}
```

👉 每一个补偿节点 = 一个独立 Token

---

## 9️⃣ 补偿 Token 的生命周期

```
Ready
  ↓
Executing (调用 compensate)
  ↓
Completed / Failed
```

### 注意：

- ❌ **补偿不做 Retry（默认）**
- ❌ **补偿失败不再补偿**
- ✔ 可以配置人工介入

---

## 🔟 NodeExecutor 对补偿的支持

```rust
match token.mode {
    TokenMode::Forward => execute_normal(),
    TokenMode::Compensation => execute_compensate(),
}
```

---

## 1️⃣1️⃣ Saga 完成判定

Saga 结束，当：

- 所有补偿 Token 状态 ∈ {Completed, Failed}

然后：

- 流程实例 → Terminated
- 发出 `SagaCompleted` 事件

---

## 1️⃣2️⃣ 与并行 Token 的协同（非常关键）

### 并行 + Saga 规则

- 并行分支 **独立记录补偿**
- Join 未完成的分支 **不参与补偿**
- Saga 范围内，**所有已成功节点都会被补偿**

---

## 1️⃣3️⃣ 事件模型补充（与前文对齐）

```rust
enum EngineEvent {
    SagaStarted,
    CompensationTokenCreated,
    CompensationCompleted,
    SagaCompleted,
}
```

---

## 1️⃣4️⃣ 你接下来该写的代码模块（推荐顺序）

1️⃣ `CompensationRecordRepo`
2️⃣ ServiceTask 成功 → 记录补偿
3️⃣ `TokenFailedHandler`
4️⃣ `SagaCoordinator`（顺序控制）
5️⃣ Compensation Token Executor

---

## 1️⃣5️⃣ 三个「踩坑必挂」的点（血的经验）

❌ 把补偿当回滚
❌ 用同一个 Token 做正向 + 补偿
❌ 补偿顺序不反转

---

## 最后一句（这句话值钱）

> **Saga 的本质是：
> 用时间换一致性，用流程换事务**

你现在这个 BPM 引擎已经具备：

- 长事务
- 并行
- 重试
- 超时
- 补偿（Saga）

👉 **这是一个完整的 BPM Runtime 了。**

---

如果你愿意，我可以陪你做最后一件「架构收官」的事（任选）：

- 👉 **Token 并发控制 & DB 锁设计**
- 👉 **Engine Crash Recovery / Rehydrate**
- 👉 **把整个设计整理成一份 README / 白皮书**

你选一个，我们把它真正封板。
