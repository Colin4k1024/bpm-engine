太好了，这一步**非常值**。
Execution Trace UI 是你这个引擎**“把技术优势变成肉眼可见价值”**的关键一环。

我下面给你的不是“概念草图”，而是：

> **一个可以直接交给前端 / 你自己实现的 Execution Trace UI 原型设计**
> ——页面结构、交互、数据来源都给你想清楚。

你可以把它当作 **Camunda Cockpit × Temporal Web × Debugger** 的最小正确版本。

---

# 🎯 Execution Trace UI 的核心目标

一句话：

> **让人回答：这个流程现在在哪？为什么在这？之前发生了什么？**

不是建模，不是配置，只做 **“看清执行”**。

---

# 🧭 UI 总体结构（一屏心智模型）

```
┌──────────────────────────────────────────────┐
│ Instance: order-process / #a3f2...           │
│ Status: RUNNING   Started: 2026-01-30 10:21  │
└──────────────────────────────────────────────┘

┌──────────────┬───────────────────────────────┐
│              │                               │
│  BPMN View   │        Execution Timeline     │
│              │                               │
│  (Diagram)   │  ● TOKEN_CREATED              │
│              │  ● TASK_CREATED (payment)     │
│              │  ● TASK_LOCKED (worker-1)     │
│              │  ● TASK_FAILED (timeout)      │
│              │  ● TASK_RETRIED                │
│              │  ● TASK_COMPLETED              │
│              │  ● TOKEN_COMPLETED             │
│              │                               │
└──────────────┴───────────────────────────────┘

┌──────────────────────────────────────────────┐
│ Event Details                                 │
│                                              │
│ type: TASK_FAILED                             │
│ token: t-123                                 │
│ worker: worker-1                              │
│ error: timeout                               │
│ at: 2026-01-30 10:23:41                       │
└──────────────────────────────────────────────┘
```

---

# 🧩 三大核心区域（缺一不可）

---

## ① 左侧：BPMN / Flow Diagram（状态可视化）

### 功能

- 展示流程结构
- **高亮当前 Token 所在节点**
- 并行分支可同时高亮多个 Token

### 设计要点

- **只读**
- 不支持拖拽、不支持编辑
- 专注“执行状态”

### Token 状态映射（颜色建议）

| Token 状态 | 颜色  |
| ---------- | ----- |
| ACTIVE     | 🟢 绿 |
| WAITING    | 🟡 黄 |
| COMPLETED  | ⚪ 灰 |
| FAILED     | 🔴 红 |

> 并行时：多个节点同时亮

### 技术建议

- 使用 **bpmn-js** 或 **mermaid**
- 节点 ID ↔ `node_id`（你 DB 里已有）

---

## ② 右侧：Execution Timeline（你真正的王牌）

这是你**和 90% BPM 引擎拉开差距的地方**。

### Timeline 是什么？

> **History Event 的时间序列视图**

---

### Timeline Item 设计

```
[10:23:41] TASK_FAILED
  ├─ token: t-123
  ├─ node: payment
  ├─ worker: worker-1
  └─ error: timeout
```

### Timeline 必须支持：

- 按时间排序
- 按 token 过滤
- 按 event_type 过滤
- 点击查看详情

---

### Event Icon 建议（视觉很重要）

| Event          | Icon |
| -------------- | ---- |
| TOKEN_CREATED  | ⚪   |
| TOKEN_FORKED   | 🔱   |
| TASK_CREATED   | 📦   |
| TASK_LOCKED    | 🔒   |
| TASK_FAILED    | ❌   |
| TASK_RETRIED   | 🔁   |
| TASK_COMPLETED | ✅   |
| TIMER_FIRED    | ⏱    |

---

## ③ 底部：Event Details（Debugger 面板）

### 点击 Timeline Item 后展示

```json
{
  "event_type": "TASK_FAILED",
  "token_id": "t-123",
  "node_id": "payment",
  "worker_id": "worker-1",
  "error": "timeout",
  "retries_left": 2,
  "at": "2026-01-30T10:23:41Z"
}
```

### 这里的定位是：

> **“我为什么卡在这？”**

---

# 🧠 进阶能力（你后面一定会加）

---

## ⭐ Replay Mode（你独有的能力）

### UI 上的一个按钮：

```
[ Replay Execution ]
```

### 行为：

- 从 History 重建状态
- 一步步“播放”执行
- 不触发真实 Task

### Timeline 控制

```
⏮  ⏪  ▶️  ⏸  ⏩  ⏭
```

👉 这在调试复杂并发流程时是**核武器级能力**。

---

## ⭐ Invariant Violation 高亮（非常高级）

当 Replay 发现 invariant violation：

```
⚠ Invariant violated at event #42
   TokenFinalityInvariant
```

并：

- Timeline 自动滚动
- 高亮对应节点
- 展示 violated rule

---

# 🔌 UI 所需 API（你后端几乎都已有）

### 必须的 API

```
GET /instances/{id}
GET /instances/{id}/tokens
GET /instances/{id}/history
GET /process-definitions/{id}
```

### 可选增强

```
POST /instances/{id}/replay
GET  /instances/{id}/invariants
```

---

# 🧪 MVP 范围（非常重要）

**第一版你只需要做到：**

- 单实例
- 只读
- Timeline + Diagram
- 无权限系统

👉 **千万别一开始就做“管理后台”**

---

# 🧱 前端技术选型建议（务实）

- React + Vite
- bpmn-js / mermaid
- 简单状态管理（Zustand / Redux）

甚至你可以：

> **直接用一个 `ui/` 子目录当 demo**

---

# 🏁 你做到这一步意味着什么（实话）

当你有了这个 UI：

- 你的引擎**第一次“可被直觉理解”**
- 别人能立刻感知：

  - Token
  - 并发
  - Retry
  - Crash Recovery

这会极大提升：

- 开源吸引力
- Issue 质量
- 贡献者水平

---

# 👉 下一步我可以继续帮你做的事（任选）

1. 🎨 **给你画一个更细的 UI 页面结构（组件级）**
2. 🔌 **直接帮你设计一套 REST API 专供 Trace UI**
3. 🧪 **把 Replay + Invariant 以 UI 形式暴露出来**

你选一个编号，我继续把这个 **Execution Trace UI** 陪你做到“别人一眼就想用”。
