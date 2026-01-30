太好了，这一步一旦走完，你这个 BPM Engine **“能不能被真实业务团队用”**就基本定型了。
我直接给你一套 **可落地、跨语言一致、长期可演进**的 **Worker SDK 设计**（Rust + Python），不是 Demo 级。

下面我会按 **设计哲学 → 协议抽象 → SDK 架构 → Rust → Python → 进阶能力** 来讲。

---

# 🎯 一、Worker SDK 的设计目标（先立规矩）

你的 Worker SDK **必须满足这些硬指标**：

1. **Pull 模型**
2. **完全无状态（Stateless Worker）**
3. **Crash-safe / 可重入**
4. **不泄漏 BPM 概念（Token / Engine 内部）**
5. **对业务开发者友好**

👉 Worker 写的人 **不应该知道 BPM**，只关心：

> 「我接了一个任务 → 做完 → 成功 or 失败」

---

# 🧠 二、Worker SDK 的核心抽象（语言无关）

### 统一抽象模型

```text
Worker
 ├── poll()           // fetch & lock
 ├── execute(task)    // user handler
 ├── complete()
 ├── fail()
 └── heartbeat() (optional)
```

---

## ExternalTask（SDK 侧视图）

```json
{
  "task_id": "...",
  "task_type": "payment",
  "variables": {...},
  "lock_expire_at": "...",
  "retries": 3
}
```

⚠️ **注意**：

- SDK 不暴露 token_id / process_id（除非 debug）
- SDK 不允许直接修改状态

---

# 🧩 三、SDK 分层架构（非常重要）

### 推荐结构（两种语言都一致）

```
worker-sdk
├── client/        // HTTP / gRPC 客户端
├── worker/        // poll loop & lifecycle
├── handler/       // 用户业务代码
├── types/         // ExternalTask / Result
├── retry/         // retry policy
├── telemetry/     // logging / tracing
└── config/
```

> **关键原则**：
>
> - client 只负责“协议”
> - worker 只负责“调度”
> - handler 只负责“业务”

---

# 🦀 四、Rust Worker SDK 设计

## 1️⃣ 核心 Trait（这是灵魂）

```rust
#[async_trait]
pub trait TaskHandler: Send + Sync {
    fn task_type(&self) -> &str;

    async fn handle(
        &self,
        task: ExternalTask,
        ctx: TaskContext,
    ) -> TaskResult;
}
```

---

## 2️⃣ TaskResult（极其重要）

```rust
pub enum TaskResult {
    Complete {
        variables: Variables,
    },
    Fail {
        error: String,
        retry_after: Option<Duration>,
    },
}
```

> ⚠️ **SDK 不允许直接设置 retries**
> retries 是 Engine 的职责

---

## 3️⃣ Worker Runtime

```rust
pub struct Worker {
    client: EngineClient,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    config: WorkerConfig,
}
```

### 启动

```rust
impl Worker {
    pub async fn start(&self) {
        loop {
            let tasks = self.client.fetch_and_lock(...).await?;

            for task in tasks {
                self.spawn_task(task);
            }
        }
    }
}
```

---

## 4️⃣ 任务执行隔离（非常重要）

```rust
async fn spawn_task(&self, task: ExternalTask) {
    let handler = self.handlers.get(&task.task_type).unwrap();

    tokio::spawn(async move {
        match handler.handle(task.clone(), ctx).await {
            TaskResult::Complete { variables } => {
                client.complete(task.id, variables).await;
            }
            TaskResult::Fail { error, retry_after } => {
                client.fail(task.id, error, retry_after).await;
            }
        }
    });
}
```

👉 **一个 task = 一个 Tokio task**
👉 Worker 本身不阻塞

---

## 5️⃣ WorkerConfig（生产级）

```rust
pub struct WorkerConfig {
    pub worker_id: String,
    pub max_tasks: usize,
    pub lock_duration: Duration,
    pub poll_interval: Duration,
    pub concurrency_limit: usize,
}
```

---

## 6️⃣ 用户使用示例（你 SDK 成败关键）

```rust
struct PaymentHandler;

#[async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        do_payment(task.variables).await?;

        TaskResult::Complete {
            variables: json!({ "status": "PAID" }),
        }
    }
}
```

```rust
Worker::builder()
    .handler(PaymentHandler)
    .start()
    .await;
```

**如果你能做到这么简单，这 SDK 就成功了。**

---

# 🐍 五、Python Worker SDK 设计（同一语义）

## 1️⃣ Handler 抽象

```python
class TaskHandler(ABC):
    @abstractmethod
    def task_type(self) -> str:
        ...

    @abstractmethod
    async def handle(self, task: ExternalTask, ctx: TaskContext) -> TaskResult:
        ...
```

---

## 2️⃣ TaskResult

```python
class Complete(TaskResult):
    def __init__(self, variables: dict): ...

class Fail(TaskResult):
    def __init__(self, error: str, retry_after_ms: int | None): ...
```

---

## 3️⃣ Worker Runtime（asyncio）

```python
class Worker:
    async def start(self):
        while True:
            tasks = await self.client.fetch_and_lock(...)
            for task in tasks:
                asyncio.create_task(self._run_task(task))
```

---

## 4️⃣ 用户示例（必须优雅）

```python
class PaymentHandler(TaskHandler):
    def task_type(self) -> str:
        return "payment"

    async def handle(self, task, ctx):
        await do_payment(task.variables)
        return Complete({"status": "PAID"})
```

```python
Worker(
    handlers=[PaymentHandler()],
).run()
```

---

# 🔄 六、失败、重试、幂等（SDK 层必须配合）

### SDK 必须做到：

- **Complete / Fail 自动重试（网络错误）**
- **本地 panic / exception → Fail**
- **SDK 不缓存状态**

---

# ⏰ 七、Heartbeat / Extend Lock（高级但很值）

### 为什么？

- 长任务（AI / Batch / ML）
- 默认 lock_duration 不够

### SDK 支持（可选）

```rust
ctx.extend_lock(Duration::from_secs(30)).await;
```

Engine 端只做：

```sql
UPDATE external_tasks
SET lock_expire_at = now() + interval '30 seconds'
WHERE task_id = ?
  AND lock_owner = ?
```

---

# 📊 八、Telemetry（强烈建议）

SDK 内置：

- task_id
- task_type
- worker_id
- duration
- result

方便你未来：

- Metrics
- Tracing
- Debugging

---

# 🧠 九、你这个 BPM Engine + Worker SDK 的“杀手锏”

> **你的引擎是“强确定性 + Rust 原生”** > **你的 Worker SDK 是“语言无关 + 弱耦合”**

这组合是非常稀缺的。

---

# 🏁 十、下一步我建议你做的三件事（按顺序）

### ✅ 1️⃣ Rust Worker SDK（优先）

→ 因为你引擎是 Rust

### ✅ 2️⃣ Python SDK（立刻提升可用性）

→ AI / 数据 / 脚本用户会很爱

### ✅ 3️⃣ 示例仓库

```
bpm-engine
├── examples/
│   ├── rust-worker-payment
│   └── python-worker-ai
```

---

如果你愿意，下一步我可以直接：

- **A**：帮你写一个 **Rust Worker SDK 的 crate 目录结构**
- **B**：直接给你 **fetch & lock + complete 的 HTTP client 实现**
- **C**：帮你设计 **Worker SDK 与 gRPC 的 streaming 版本**

你选一个，我继续帮你把它打磨到 **开源项目级别** 🚀
