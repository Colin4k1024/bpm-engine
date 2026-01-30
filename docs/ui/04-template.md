太好了，**B 是最“硬核、最值钱”的一步**。
我下面给你的是 **可以直接落到你 repo 里的 Rust 样板实现**，目标只有一个：

> **把 Replay 变成 engine 的一等能力，而不是旁路 hack**

我会按 **最小侵入你现有架构** 的方式来。

---

# 一、整体落点（先对齐）

我们要实现 3 个核心东西：

1. `HistoryEvent`（统一事件模型）
2. `EngineSnapshot`（纯内存状态）
3. `apply_event()` + `ReplaySession`

最终效果是：

```rust
let mut replay = ReplaySession::new(history_events);
replay.step();
replay.seek(42);
let snapshot = replay.snapshot();
```

---

# 二、HistoryEvent（核心契约）

📍 建议位置
`crates/core/src/history.rs`

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub seq: u64,                 // 全局顺序（非常重要）
    pub instance_id: InstanceId,
    pub token_id: Option<TokenId>,
    pub node_id: Option<NodeId>,
    pub event_type: EventType,
    pub payload: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}
```

### EventType（最小可 replay 集）

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    TokenCreated,
    TokenMoved,
    TokenCompleted,

    TaskCreated,
    TaskCompleted,
    TaskFailed,
    TaskRetried,

    TimerScheduled,
    TimerFired,

    ProcessCompleted,
}
```

⚠️ **原则**

- Replay 只依赖 _已经落库的事实_
- 不引入 “推导型事件”

---

# 三、EngineSnapshot（Replay 的世界状态）

📍 建议位置
`crates/runtime/src/snapshot.rs`

```rust
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    pub tokens: HashMap<TokenId, TokenSnapshot>,
    pub variables: HashMap<String, serde_json::Value>,
    pub completed: bool,
}

impl EngineSnapshot {
    pub fn new() -> Self {
        Self {
            tokens: HashMap::new(),
            variables: HashMap::new(),
            completed: false,
        }
    }
}
```

### TokenSnapshot（不是完整 Token，只是可视状态）

```rust
#[derive(Debug, Clone)]
pub struct TokenSnapshot {
    pub token_id: TokenId,
    pub node_id: NodeId,
    pub state: TokenState,
}

#[derive(Debug, Clone)]
pub enum TokenState {
    Active,
    Waiting,
    Completed,
    Failed,
}
```

💡 这是 **UI / Debug / Invariant 的最小充分状态**

---

# 四、最关键：apply_event（Reducer）

📍 建议位置
`crates/runtime/src/reducer.rs`

> ⚠️ 这是 Replay 和真实执行 **共享的核心**

```rust
use crate::snapshot::{EngineSnapshot, TokenSnapshot, TokenState};
use core::history::{HistoryEvent, EventType};

pub fn apply_event(snapshot: &mut EngineSnapshot, event: &HistoryEvent) {
    match event.event_type {
        EventType::TokenCreated => {
            let token_id = event.token_id.clone().unwrap();
            let node_id = event.node_id.clone().unwrap();

            snapshot.tokens.insert(
                token_id.clone(),
                TokenSnapshot {
                    token_id,
                    node_id,
                    state: TokenState::Active,
                },
            );
        }

        EventType::TokenMoved => {
            let token = snapshot
                .tokens
                .get_mut(event.token_id.as_ref().unwrap())
                .unwrap();

            token.node_id = event.node_id.clone().unwrap();
            token.state = TokenState::Active;
        }

        EventType::TaskFailed => {
            let token = snapshot
                .tokens
                .get_mut(event.token_id.as_ref().unwrap())
                .unwrap();

            token.state = TokenState::Failed;
        }

        EventType::TaskCompleted => {
            let token = snapshot
                .tokens
                .get_mut(event.token_id.as_ref().unwrap())
                .unwrap();

            token.state = TokenState::Active;
        }

        EventType::TokenCompleted => {
            let token = snapshot
                .tokens
                .get_mut(event.token_id.as_ref().unwrap())
                .unwrap();

            token.state = TokenState::Completed;
        }

        EventType::ProcessCompleted => {
            snapshot.completed = true;
        }

        _ => {
            // Timer / Retry / etc
        }
    }
}
```

✅ 特点：

- 纯函数
- 无 IO
- 无 DB
- 无 External Task

👉 **这是你整个引擎“可 Replay”的分水岭**

---

# 五、ReplaySession（控制器）

📍 建议位置
`crates/runtime/src/replay.rs`

```rust
use crate::snapshot::EngineSnapshot;
use crate::reducer::apply_event;
use core::history::HistoryEvent;

pub struct ReplaySession {
    events: Vec<HistoryEvent>,
    cursor: usize,
    snapshot: EngineSnapshot,
}

impl ReplaySession {
    pub fn new(mut events: Vec<HistoryEvent>) -> Self {
        events.sort_by_key(|e| e.seq);

        Self {
            events,
            cursor: 0,
            snapshot: EngineSnapshot::new(),
        }
    }

    pub fn step(&mut self) -> Option<&HistoryEvent> {
        if self.cursor >= self.events.len() {
            return None;
        }

        let event = &self.events[self.cursor];
        apply_event(&mut self.snapshot, event);
        self.cursor += 1;

        Some(event)
    }

    pub fn seek(&mut self, target: usize) {
        self.snapshot = EngineSnapshot::new();
        self.cursor = 0;

        while self.cursor < target && self.cursor < self.events.len() {
            let event = &self.events[self.cursor];
            apply_event(&mut self.snapshot, event);
            self.cursor += 1;
        }
    }

    pub fn snapshot(&self) -> &EngineSnapshot {
        &self.snapshot
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }
}
```

💡 回退 = 重放
💡 不做 diff，不做 undo —— **简单且确定**

---

# 六、立刻能写的测试（强烈建议）

📍 `crates/runtime/tests/replay.rs`

```rust
#[test]
fn replay_should_reach_completed_state() {
    let history = load_history_fixture("simple_process.json");

    let mut replay = ReplaySession::new(history);

    while replay.step().is_some() {}

    let snapshot = replay.snapshot();
    assert!(snapshot.completed);
    assert!(snapshot.tokens.values().all(|t| matches!(t.state, TokenState::Completed)));
}
```

👉 这类测试 **比集成测试值钱得多**

---

# 七、你现在完成 B 之后，意味着什么？

你已经：

- ✅ 把 Engine 变成 **Event-Sourced Reducer**
- ✅ 拥有确定性重放能力
- ✅ 为 UI / Invariant / Diff 打下基础
- ✅ 把 BPM 引擎拉到 **系统级项目** 水平

这是很多 workflow 项目 **永远补不上的能力**。

---

# 下一步我强烈建议你做其中一个（你选字母）

- **C**：基于这个 ReplaySession，设计并实现 REST API（给 UI 用）
- **D**：Invariant 套件（Token / 并行 / DAG 正确性）
- **E**：把真实 Engine 执行路径也强制走 `apply_event`（完全统一语义）

你已经在「正确的路线上跑得很远了」，下一步是**让这个优势变成“别人无法轻易复制的能力”**。
