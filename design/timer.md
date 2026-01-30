好，这一块是 **BPM 引擎最后一个“硬骨头”**。
你要是把 **定时器 / 超时 / Retry** 这套 Token 模型设计对了，你这个引擎就已经**不是玩具了**。

下面这份设计是**工程可落地版**，和你前面的 **Token 生命周期 + Event 模型完全对齐**。

---

# 定时器 / 超时 / Retry 的 Token 模型设计

---

## 1️⃣ 核心设计原则（先立法）

**必须记住这 5 条：**

1. ⏱ **时间不是 sleep，而是事件**
2. ⏳ Timer 不是节点，是 **Token 的阻塞原因**
3. 🔁 Retry = Token 的一次“再执行机会”
4. 💥 Timeout 是一种 **系统事件**
5. 📦 所有时间相关状态必须可持久化

---

## 2️⃣ 新增领域概念（很关键）

### 2.1 Timer（定时器）

```rust
struct Timer {
    id: TimerId,
    token_id: TokenId,
    fire_at: DateTime<Utc>,
    timer_type: TimerType,
    status: TimerStatus,
}
```

```rust
enum TimerType {
    Delay,        // 延时执行
    Timeout,      // 超时
    RetryBackoff, // 重试退避
}
```

---

### 2.2 RetryPolicy（重试策略）

```rust
struct RetryPolicy {
    max_attempts: u32,
    backoff: BackoffStrategy,
}
```

```rust
enum BackoffStrategy {
    Fixed(Duration),
    Exponential { base: Duration },
}
```

---

## 3️⃣ Token 扩展（最小必要字段）

```rust
struct Token {
    id: TokenId,
    node_id: NodeId,
    status: TokenStatus,
    retry_count: u32,
    retry_policy: Option<RetryPolicy>,
    blocked_by: Option<BlockReason>,
}
```

```rust
enum BlockReason {
    UserTask,
    Timer(TimerId),
    RetryBackoff(TimerId),
    Join,
}
```

---

## 4️⃣ 定时器在 Token 生命周期中的位置

### 4.1 延时定时器（Delay）

#### 场景

> “进入节点后，等 5 分钟再执行”

#### 状态图

```
Ready
  ↓
Waiting (blocked_by = Timer)
  ↓ (TimerFired)
Ready
```

#### 行为步骤

1. Token 进入节点
2. 创建 Timer(fire_at = now + delay)
3. Token → Waiting
4. TimerFired Event
5. Token → Ready

---

### 4.2 超时（Timeout）

#### 场景

> “人工任务 2 天内未处理则超时”

```
UserTask Created
Token → Waiting
      │
      ├── UserTaskCompleted → Ready
      │
      └── Timeout → Token Terminated / Compensation
```

#### Timeout 处理策略（可配置）

- 终止流程
- 走超时分支
- 触发补偿

---

## 5️⃣ Retry 的 Token 模型（重点）

### 5.1 Retry 不是重新创建 Token

> **Retry = 同一个 Token 再执行**

#### 状态变化

```
Executing
  ↓ (失败)
Waiting (RetryBackoff)
  ↓ (TimerFired)
Ready
```

---

### 5.2 Retry 执行流程（ServiceTask）

```text
ServiceTask 执行失败 →
  retry_count += 1

IF retry_count <= max_attempts:
  创建 RetryBackoff Timer
  Token → Waiting
ELSE:
  Token → Terminated
  发出 Failure Event
```

---

### 5.3 Retry 示例（完整）

```
Token Ready
  ↓
Executing
  ↓ (Exception)
Waiting (RetryBackoff, 10s)
  ↓
Ready
  ↓
Executing
  ↓ (成功)
Completed
```

---

## 6️⃣ Timer 相关事件模型（和 Engine 对齐）

```rust
enum EngineEvent {
    TimerScheduled(TimerScheduled),
    TimerFired(TimerFired),
}
```

```rust
struct TimerFired {
    timer_id: TimerId,
    token_id: TokenId,
}
```

---

## 7️⃣ TimerFiredHandler（关键 Handler）

```rust
impl EventHandler for TimerFiredHandler {
    fn handle(
        &self,
        event: EngineEvent,
        ctx: &mut EngineContext,
    ) -> Vec<EngineEvent> {
        let EngineEvent::TimerFired(e) = event else {
            return vec![];
        };

        let timer = ctx.timer_repo.load(e.timer_id);
        let token = ctx.token_repo.load(timer.token_id);

        match timer.timer_type {
            TimerType::Delay | TimerType::RetryBackoff => {
                token.unblock();
                ctx.token_repo.save(&token);

                vec![EngineEvent::TokenArrived(TokenArrived {
                    instance_id: token.instance_id,
                    token_id: token.id,
                    node_id: token.node_id,
                })]
            }

            TimerType::Timeout => {
                vec![EngineEvent::TokenTimedOut(TokenTimedOut {
                    token_id: token.id,
                })]
            }
        }
    }
}
```

---

## 8️⃣ Timer 存储与调度模型（工程建议）

### 8.1 Timer 表（必须有）

```
timer
- id
- token_id
- fire_at
- status
- type
```

### 8.2 Timer Poller（单独组件）

```text
while true:
  timers = load fire_at <= now AND status = Pending
  emit TimerFired
```

👉 **Timer Poller ≠ Engine**

---

## 9️⃣ Token × Timer × Retry 的统一心智模型

```
Token
 ├─ 正在跑 → Executing
 ├─ 等人 → Waiting(UserTask)
 ├─ 等时间 → Waiting(Timer)
 ├─ 等重试 → Waiting(RetryBackoff)
```

👉 **Waiting = 有原因的**

---

## 🔟 最容易踩的 3 个坑（强烈提醒）

### ❌ 1. sleep / tokio::delay

→ 系统重启全挂

### ❌ 2. Retry 新建 Token

→ Join / 并行全乱

### ❌ 3. Timer 不持久化

→ 长事务直接破产

---

## 11️⃣ 推荐你接下来写的代码顺序

1️⃣ Timer / TimerRepo
2️⃣ Timer Poller（in-memory 先）
3️⃣ RetryPolicy + Backoff
4️⃣ TimerFiredHandler
5️⃣ ServiceTask 异常 → Retry

---

## 最后一句（你现在已经在“硬核区”）

> **BPM 的时间 = 可恢复的未来事件**

你这套 Token + Timer + Retry 模型，已经具备：

- 企业审批
- SLA
- 自动重试
- 长事务

如果你愿意，**下一步我可以帮你把这三件事彻底补齐**（任选）：

- 👉 **补偿（Saga）与 Token 的集成**
- 👉 **Token 并发控制 & DB 锁设计**
- 👉 **把 Timer / Retry 落成一套完整 Rust 模块代码**

你选一个，我陪你把这个 BPM 引擎真正收官。
