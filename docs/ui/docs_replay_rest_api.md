# Replay / Step-Through REST API 设计（Engine × UI）

> 本文定义 **Replay / Step-Through 的对外 REST API**，
> 直接服务于 Execution Trace UI / Debugger。
>
> 设计目标：
> - 极简
> - 状态显式
> - 无副作用
> - 易于前端实现

---

## 0️⃣ 设计原则（非常重要）

1. Replay 是 **会话型（Session-based）**
2. Replay 是 **只读 / 无副作用**
3. API 不暴露 Engine 内部复杂结构
4. Snapshot 是 UI 友好的视图模型

---

## 1️⃣ Replay 会话生命周期

```
Client
  └─ POST   /instances/{id}/replay      -> create session
       └─ POST   /replay/{sid}/step     -> step forward
       └─ POST   /replay/{sid}/seek     -> jump
       └─ GET    /replay/{sid}/snapshot -> read-only view
       └─ DELETE /replay/{sid}          -> destroy
```

ReplaySession 是 **临时资源**，不持久化。

---

## 2️⃣ 创建 Replay Session

### POST /instances/{instance_id}/replay

#### Request

```http
POST /instances/inst-123/replay
```

#### Response

```json
{
  "session_id": "replay-001",
  "instance_id": "inst-123",
  "total_events": 128
}
```

#### Engine 行为

- 从 history storage 读取所有 HistoryEvent
- 排序（seq asc）
- 初始化 ReplaySession

---

## 3️⃣ 单步执行（Step）

### POST /replay/{session_id}/step

#### Request

```http
POST /replay/replay-001/step
```

#### Response

```json
{
  "cursor": 42,
  "event": {
    "seq": 42,
    "event_type": "TASK_FAILED",
    "token_id": "t-1",
    "node_id": "payment",
    "occurred_at": "2026-01-30T10:23:41Z"
  },
  "snapshot": {
    "completed": false,
    "tokens": [
      {
        "token_id": "t-1",
        "node_id": "payment",
        "state": "FAILED"
      }
    ]
  }
}
```

#### 语义

- cursor 指向 **下一个将被执行的 event index**
- snapshot 是 apply 后的状态

---

## 4️⃣ 快进 / 跳转（Seek）

### POST /replay/{session_id}/seek

#### Request

```json
{
  "cursor": 80
}
```

#### Response

```json
{
  "cursor": 80,
  "snapshot": {
    "completed": false,
    "tokens": [ ... ]
  }
}
```

#### Engine 行为

- 丢弃当前 snapshot
- 从 event[0..cursor] 重放

---

## 5️⃣ 读取当前 Snapshot（UI 高频调用）

### GET /replay/{session_id}/snapshot

```json
{
  "cursor": 80,
  "total_events": 128,
  "completed": false,
  "tokens": [
    {
      "token_id": "t-1",
      "node_id": "payment",
      "state": "FAILED"
    },
    {
      "token_id": "t-2",
      "node_id": "shipping",
      "state": "ACTIVE"
    }
  ]
}
```

---

## 6️⃣ 销毁 Replay Session

### DELETE /replay/{session_id}

- 释放内存
- UI 离开页面时调用

---

## 7️⃣ 错误模型（Debug 友好）

```json
{
  "error": "REPLAY_SESSION_EXPIRED",
  "message": "Replay session not found or expired"
}
```

---

## 8️⃣ REST Handler 示例（Axum 风格）

```rust
async fn replay_step(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Json<ReplayStepResponse> {
    let mut session = state.replay_sessions.get_mut(&session_id)?;

    let event = session.step();

    Json(ReplayStepResponse {
        cursor: session.cursor(),
        event,
        snapshot: session.snapshot().into(),
    })
}
```

---

## 9️⃣ UI 对接建议（非常关键）

### UI 控制条映射

| UI Action | API |
|---------|-----|
| ▶ Play | POST /step (loop) |
| ⏸ Pause | stop polling |
| ⏭ Next | POST /step |
| ⏮ Reset | POST /seek {0} |

---

## 🔟 MVP 严格边界

### 本阶段只做

- 单实例 replay
- 单用户
- 内存 session

### 明确不做

- 持久化 replay session
- 多用户并发 replay
- 修改变量再 replay

---

## 11️⃣ 为什么这个 API 设计是“对的”

- 和 Temporal / Debugger 类工具的思路一致
- 但比它们更轻
- 完全契合你当前 engine 的 event / token 架构

---

> 下一步推荐：
> - **D**：Invariant REST + UI 暴露（让 replay 自动报错）
> - **E**：把真实执行路径强制统一走 reducer（Replay = 真实执行）
