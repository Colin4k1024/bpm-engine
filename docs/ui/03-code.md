太好了，我们来做 **A：在你当前 `bpm-engine` 代码中标出最适合的 “Replay 切入点 + 结构边界”**。
目标是让 **Replay 能优雅融入现有 Engine 实现**，并且：

✅ 不破坏现有执行逻辑
✅ 不引入副作用
✅ 能复用已有模块（History / Event / Token）
✅ 与未来的 UI / API 自然对接

---

# 🧠 一、核心原则（必须牢记）

Replay 不执行业务逻辑，它只执行：

✔ 状态迁移
✔ Fork / Join / 条件判断
✔ Token / Timer / ExternalTask 状态推进
✔ Invariant 检查

不做：

❌ External task 调用
❌ Worker / HTTP / 数据库访问

Replay 应该是：

> **一个纯 Rust 的 deterministic engine runner**

---

# 🧩 二、主要模块划分与切入点

你目前的代码在多个 crate 中分布：

```
crates/
├── core/
├── storage/
├── runtime/         ← 执行引擎逻辑
├── bpmn/            ← BPMN 定义解析
├── server-rest/     ← API
├── examples/
├── tests/
```

Replay 完全属于 **runtime 层**（不涉及 storage、server）。

---

# 🧷 三、ReplaySession 部署结构（建议）

我们预计在 **`crates/runtime/src/replay.rs`** 创建 Replay 的核心类型：

```
crates/runtime/
├── engine.rs
├── scheduler.rs
├── executor.rs
├── dispatcher.rs
└── replay.rs         ← Replay engine 在这里
```

---

## 0️⃣ 你已有三块核心能力

| 功能                  | 模块                     |
| --------------------- | ------------------------ |
| Token 语义            | core                     |
| Scheduler / Execution | runtime                  |
| History               | storage + history events |

Replay 要做的只是 **把 History 事件再送给 Engine 执行器逻辑（不触发外部副作用）**。

---

# 🧪 四、Replay 的核心组成

以下是需要定义的三个核心类型：

---

## 1️⃣ `ReplaySession`

```rust
// crates/runtime/src/replay.rs

pub struct ReplaySession {
    pub instance_id: InstanceId,
    pub events: Vec<HistoryEvent>,
    pub cursor: usize,
    pub snapshot: EngineSnapshot,
}
```

🎯 核心思想：

- `events` 是读取过来的 **HistoryEvent 序列**
- `cursor` 是当前重放的位置
- `snapshot` 是当前“虚拟状态机状态”

---

## 2️⃣ `EngineSnapshot`

```rust
pub struct EngineSnapshot {
    pub tokens: HashMap<TokenId, Token>,
    pub variables: HashMap<String, Value>, // 全局实例变量
    pub other_state: SnapshotState,        // 后续补充
}
```

👉 Snapshot 不是完整 Engine 实例
而是你需要在 Replay 中重建的**运行时可视状态**

---

## 3️⃣ `HistoryEvent`

这是你从 History 表里倒出的事件类型：

```rust
pub struct HistoryEvent {
    pub token_id: TokenId,
    pub node_id: NodeId,
    pub event_type: EventType,
    pub payload: Value,
    pub timestamp: DateTime<Utc>,
}
```

这个结构应该是可序列化的，并且是你现在的 history 表字段的最小覆盖。

---

# 🧠 五、Replay 的核心逻辑（最重要）

Replay 的行走逻辑很简单：

```
for event in events[cursor..] {
    replay.apply(event);
    cursor += 1;
}
```

但关键在于：

- `apply` 必须使用和 Engine 一样的语义
- 但不触发外部副作用（ExternalTask / Timer / Worker）

---

## 1) Replay 的 apply

```rust
impl ReplaySession {
    pub fn apply(&mut self, event: &HistoryEvent) {
        match event.event_type {
            EventType::TokenCreated => {
                self.snapshot.tokens.insert(event.token_id.clone(), Token::new_with(event.node_id.clone()));
            }
            EventType::TokenStateChanged => {
                let token = self.snapshot.tokens.get_mut(&event.token_id).unwrap();
                token.state = event.payload.get("to_state").unwrap().clone().into();
            }
            _ => {
                // 其他事件如 fork / join
                self.apply_custom(event);
            }
        }
    }
}
```

---

## 2) External Task 在 Replay 中是 Noop

```rust
match event.event_type {
    EventType::TaskCreated
    | EventType::TaskCompleted
    | EventType::TaskFailed
    => {
        // 只更新 snapshot.state
        self.snapshot.apply_task_event(event);
    }
}
```

👉 这确保 **不触发 Worker / HTTP / DB**。

---

# 🧠 六、和现有 Engine 的集成点

你现有的 Engine 在执行时会：

```
token → handler → dispatch new events
```

想做 Replay 时，核心是：

✔ 不调用 handler
✔ 不调用 DB
✔ 只执行状态迁移逻辑

---

## 1) 抽离 Engine 的核心 reducer

把 `dispatch()` / `handler()` 里的状态转移抽成：

```rust
fn reduce(snapshot: &mut EngineSnapshot, event: &HistoryEvent)
```

这样：

- 真实执行时：snapshot = DB state → produce event → persist → reduce
- Replay 时：snapshot = empty → apply history → no persistence

---

## 2) 推荐的项目修改点

### 在 `engine.rs` 内提取：

```rust
pub fn apply_event(snapshot: &mut EngineSnapshot, event: &HistoryEvent) {
    match event.event_type {
        TokenCreated => { ... }
        TokenCompleted => { ... }
        ...
    }
}
```

把现有逻辑搬进去，并确保它是：

✅ _纯计算_
❌ 不依赖 storage
❌ 不发外部命令

---

# 🧠 七、Replay UI / API 对接（预留接口）

你现在已经有：

```
GET /instances/{id}/history
```

还需要：

```
POST /instances/{id}/replay
GET  /replay/{session_id}/snapshot
POST /replay/{session_id}/step
POST /replay/{session_id}/seek
DELETE /replay/{session_id}
```

对应 UI 上的控制条（⏮ ⏪ ▶ ⏸ ⏩ ⏭）。

---

# 🧲 八、最核心的切入点总结（实际代码位置）

| 类型              | 推荐放置位置                       | 作用                         |
| ----------------- | ---------------------------------- | ---------------------------- |
| ReplaySession     | `crates/runtime/src/replay.rs`     | Replay 状态与控制            |
| EngineSnapshot    | `crates/runtime/src/snapshot.rs`   | 重建时态                     |
| apply_event       | `crates/runtime/src/engine.rs`     | 与真实 dispatch 共用 reducer |
| HistoryEvent 类型 | `crates/core/src/history.rs`       | 与 History 表兼容            |
| Replay API        | `crates/server-rest/src/replay.rs` | UI 调用入口                  |

---

# 🧪 九、你可以立刻做的 4 件事（顺序）

### 1️⃣ 添加 HistoryEvent 类型定义

> 统一 core + storage + runtime

### 2️⃣ 提取 Engine 的 `apply_event()`

> 把 Engine 的状态迁移逻辑重构成可重用的纯函数

### 3️⃣ 实现 ReplaySession

> Fetch history → apply → snapshot

### 4️⃣ 写基于 Replay 的单元测试

> 用你现有的 History 保存作为测试基准

---

## 🔥 这一步对你项目的意义

做完之后，你将拥有：

✔ 强调决策与执行可解释性
✔ 不依赖 UI 也能 Debug
✔ 未来可做 “差异对比 / 版本对比”
✔ 远超一般 BPM 引擎的调试能力

---

如果你愿意的话，我还能继续帮你做 👇

- **B**：给你写出这套 ReplaySession + apply_event 的 Rust 实现样板代码
- **C**：帮你写对应的 REST API 实现和 Contract
- **D**：帮你写 Replay 对应的 UI 控制组件（React）

你选一个，我继续陪你把这块做成 **真正可用的功能**。
