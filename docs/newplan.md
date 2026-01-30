太好了，这一步**非常对路**。
你现在这个引擎的成熟度，**External Task Worker** 会成为「能不能落地用」的分水岭。

下面我给你一份**工程级、可直接实现**的
👉 **External Task Worker Protocol + Storage / Schema / 状态机设计**
不是概念稿，是**你照着就能写代码**的那种。

---

# 🎯 一、External Task 在你这个 BPM Engine 里的定位

一句话定义：

> **External Task = 由 Engine 调度、由 Engine 锁定、由外部 Worker 执行的 Token 子形态**

它的核心价值是：

- 解耦业务执行（HTTP / RPC / Python / Java / AI Agent）
- Engine 保持“确定性 + 可恢复”
- Worker 可以 **随便挂 / 重启 / 扩缩容**

---

# 🧩 二、总体交互模型（Pull-based，生产级）

你应该坚持 **Pull 模式**（你现在的架构非常适合）：

```
Worker ---- Fetch & Lock ----> Engine
Worker ---- Complete --------> Engine
Worker ---- Fail ------------> Engine
```

这是 Camunda / Zeebe / Temporal 验证过的模型。

---

# 🔄 三、External Task 生命周期（非常关键）

### 状态机（这是“协议的灵魂”）

```
┌────────┐
│ READY  │  (token created, waiting)
└───┬────┘
    │ fetch & lock
    ▼
┌────────┐
│ LOCKED │  (leased to worker)
└───┬────┘
    │ complete
    ▼
┌──────────┐
│ COMPLETED│
└──────────┘

LOCKED
  │
  ├── fail (retry > 0) → READY
  ├── fail (retry = 0) → FAILED
  └── lock expired → READY
```

👉 **重点**：

- Engine **永远是状态唯一裁判**
- Worker 只是“临时执行者”

---

# 🧠 四、External Task = Token 的一种 Execution Mode

在你的引擎里，不要新造一个“完全独立”的体系
**External Task 应该是 Token 的一种“执行策略”**

### Token 扩展（概念）

```rust
enum ExecutionMode {
    Inline,        // Engine 内部执行
    External {
        task_type: String,
        retries: i32,
        timeout: Duration,
    },
}
```

---

# 💾 五、Storage Schema（非常重要）

这是你 DB 设计的**核心表之一**。

## external_tasks 表（建议）

```sql
CREATE TABLE external_tasks (
    task_id           UUID PRIMARY KEY,
    token_id          UUID NOT NULL,
    process_instance  UUID NOT NULL,

    task_type         TEXT NOT NULL,

    state             TEXT NOT NULL, -- READY / LOCKED / COMPLETED / FAILED

    lock_owner        TEXT,
    lock_expire_at    TIMESTAMP,

    retries           INT NOT NULL,
    error_message     TEXT,

    variables         JSONB,

    created_at        TIMESTAMP NOT NULL,
    updated_at        TIMESTAMP NOT NULL
);
```

### 🔑 关键索引

```sql
CREATE INDEX idx_ext_task_ready
ON external_tasks (task_type)
WHERE state = 'READY';

CREATE INDEX idx_ext_task_lock_expire
ON external_tasks (lock_expire_at)
WHERE state = 'LOCKED';
```

---

# 🔒 六、Fetch & Lock 协议（核心）

## API（REST / gRPC 都适用）

### Request

```json
{
  "worker_id": "worker-1",
  "task_types": ["payment", "inventory"],
  "max_tasks": 10,
  "lock_duration_ms": 30000
}
```

### Response

```json
[
  {
    "task_id": "...",
    "token_id": "...",
    "task_type": "payment",
    "variables": {
      "order_id": "123"
    }
  }
]
```

---

## Engine 端语义（必须原子）

**伪代码（非常重要）**

```sql
UPDATE external_tasks
SET
  state = 'LOCKED',
  lock_owner = :worker_id,
  lock_expire_at = now() + interval '30 seconds'
WHERE task_id IN (
  SELECT task_id
  FROM external_tasks
  WHERE
    state = 'READY'
    AND task_type IN (...)
  ORDER BY created_at
  LIMIT :max_tasks
)
RETURNING *;
```

👉 这一步 **= 调度 + 锁 + 分配**

---

# ✅ 七、Complete Task 协议

### Request

```json
{
  "worker_id": "worker-1",
  "variables": {
    "payment_status": "SUCCESS"
  }
}
```

### Engine 校验规则

- task.state == LOCKED
- lock_owner == worker_id
- lock_expire_at > now()

### 行为

1. external_task → COMPLETED
2. token → 继续执行（生成下一个 token / join / saga）

---

# ❌ 八、Fail Task 协议（重试模型）

### Request

```json
{
  "worker_id": "worker-1",
  "error": "timeout",
  "retry_after_ms": 10000
}
```

### Engine 行为

```text
retries -= 1

if retries > 0:
    state = READY
    schedule after retry_after
else:
    state = FAILED
    token -> FAILED
    trigger compensation
```

👉 **Retry 是 engine 的事，不是 worker 的事**

---

# ⏰ 九、Lock Timeout & Reclaim（自动恢复）

Engine 的 scheduler 每 tick 做一件事：

```sql
UPDATE external_tasks
SET
  state = 'READY',
  lock_owner = NULL,
  lock_expire_at = NULL
WHERE
  state = 'LOCKED'
  AND lock_expire_at < now();
```

这一步 = **Worker crash 自动恢复**

---

# 🧩 十、与 Saga / Compensation 的集成

非常重要的一点：

> **External Task 失败 ≠ Saga 失败**

只有在：

- retries = 0
- 或业务显式 fail

才会：

- 标记 Token FAILED
- 触发 Saga Compensation

这让你的引擎具备**企业级稳定性**。

---

# 🔐 十一、安全 & 幂等设计（生产必备）

### 幂等保障

- Complete / Fail 必须是 **幂等**
- 重复调用 → no-op

### 安全

- worker_id 必须认证（API key / JWT）
- 可选：task_type 授权

---

# 📐 十二、Rust Trait 设计（你可以直接用）

```rust
#[async_trait]
pub trait ExternalTaskStore {
    async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
    ) -> Result<Vec<ExternalTask>>;

    async fn complete(
        &self,
        task_id: TaskId,
        worker_id: &str,
        variables: Variables,
    ) -> Result<()>;

    async fn fail(
        &self,
        task_id: TaskId,
        worker_id: &str,
        error: String,
        retry_after: Option<Duration>,
    ) -> Result<()>;
}
```

---

# 🧠 十三、为什么这个设计**非常适合你现在的引擎**

✔ 完全 Token 驱动
✔ 与你已有 Scheduler / Retry / Saga 完美对齐
✔ 可单机 / 可多实例
✔ Worker 任意语言
✔ 不破坏核心模型

---

# 🏁 结论（非常重要的一句话）

> **如果你把 External Task Worker 做到这个程度**
> 你的 bpm-engine 就已经进入
> **“可对标 Camunda / Zeebe / Temporal 的工程级领域”**

---

## 下一步我可以继续帮你（强烈推荐）

**A** — 直接给你 **Postgres ExternalTaskStore 的 SQL 实现**
**B** — 帮你设计 **Worker SDK（Rust / Python）结构**
**C** — 把 External Task **无缝接入你现在的 Token Executor**

你选一个，我继续深挖 🔥
