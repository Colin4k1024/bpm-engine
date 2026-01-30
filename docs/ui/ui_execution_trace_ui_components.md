# Execution Trace UI —— 组件级原型设计

> 目标：把 BPM Engine 的 **执行状态 / 并发 / 异常 / 重试** 以「一眼能看懂」的方式呈现出来。
> 本文是 **组件级 UI 设计蓝图**，可直接交付前端实现。

---

## 1️⃣ 页面总览（Execution Trace Page）

```
┌──────────────────────────────────────────────────────────┐
│ InstanceHeader                                           │
├───────────────────────┬─────────────────────────────────┤
│ ProcessDiagramPanel   │ ExecutionTimelinePanel           │
│                       │                                 │
│                       │                                 │
├───────────────────────┴─────────────────────────────────┤
│ EventDetailPanel                                         │
└──────────────────────────────────────────────────────────┘
```

核心思想：
- **左 = 结构**（流程长什么样）
- **右 = 时间**（发生了什么）
- **下 = 细节**（为什么）

---

## 2️⃣ InstanceHeader（实例头部）

### 职责
- 当前流程实例的全局状态
- 快速判断「是不是卡住了」

### 组件结构

```
InstanceHeader
├─ ProcessName
├─ InstanceId (copy)
├─ StatusBadge
├─ StartTime
└─ ControlActions
   ├─ Refresh
   └─ Replay (disabled in MVP)
```

### 状态设计

| Status | 颜色 | 含义 |
|------|----|----|
| RUNNING | 🟢 | 正在执行 |
| WAITING | 🟡 | 等待外部任务/定时器 |
| FAILED | 🔴 | 有失败 Token |
| COMPLETED | ⚪ | 正常结束 |

---

## 3️⃣ ProcessDiagramPanel（流程图面板）

### 职责
- 展示 BPMN / Flow 结构
- **用 Token 高亮“执行到哪”**

### 子组件

```
ProcessDiagramPanel
├─ DiagramCanvas
│  ├─ NodeRenderer
│  └─ EdgeRenderer
└─ TokenOverlayLayer
   └─ TokenBadge[]
```

### TokenBadge 设计

```
◉ t-123  (ACTIVE)
◉ t-124  (WAITING)
```

- 并行时：一个节点可挂多个 TokenBadge
- 点击 TokenBadge → Timeline 自动过滤该 Token

### 与后端数据映射

- node.id ↔ engine.node_id
- token.node_id → 高亮节点

---

## 4️⃣ ExecutionTimelinePanel（执行时间线）【核心】

### 职责
- **完整展示 Execution History**
- 支持过滤 / 定位 / 回溯

### 组件结构

```
ExecutionTimelinePanel
├─ TimelineToolbar
│  ├─ TokenFilter
│  ├─ EventTypeFilter
│  └─ AutoScrollToggle
├─ TimelineList
│  └─ TimelineItem[]
└─ TimelineEmptyState
```

---

### TimelineItem 设计（最重要）

```
┌────────────────────────────────────┐
│ ⏱ 10:23:41   TASK_FAILED           │
│ token: t-123  node: payment        │
│ worker: worker-1                   │
│ error: timeout                     │
└────────────────────────────────────┘
```

#### Item 元信息

| 字段 | 来源 |
|----|----|
| event_type | history_events.type |
| token_id | history_events.token_id |
| node_id | history_events.node_id |
| timestamp | history_events.at |

#### 交互
- hover：高亮 Diagram 对应节点
- click：填充 EventDetailPanel

---

### Event Icon & 颜色规范

| Event | Icon | Color |
|----|----|----|
| TOKEN_CREATED | ⚪ | gray |
| TOKEN_FORKED | 🔱 | purple |
| TASK_CREATED | 📦 | blue |
| TASK_LOCKED | 🔒 | cyan |
| TASK_COMPLETED | ✅ | green |
| TASK_FAILED | ❌ | red |
| TASK_RETRIED | 🔁 | orange |
| TIMER_FIRED | ⏱ | yellow |

---

## 5️⃣ EventDetailPanel（调试面板）

### 职责
- **解释为什么发生这个事件**
- Debug 入口

### 组件结构

```
EventDetailPanel
├─ EventMetaSection
├─ TokenContextSection
├─ PayloadSection
└─ DebugActions (future)
```

### 示例

```json
{
  "event_type": "TASK_FAILED",
  "token_id": "t-123",
  "node_id": "payment",
  "worker_id": "worker-1",
  "error": "timeout",
  "retries_left": 2,
  "occurred_at": "2026-01-30T10:23:41Z"
}
```

---

## 6️⃣ 组件通信关系（非常关键）

```
TimelineItem.click
   ├─ highlightNode(node_id)
   ├─ selectToken(token_id)
   └─ showEventDetail(event_id)

TokenBadge.click
   └─ filterTimeline(token_id)
```

👉 **Timeline 是中心控制器**，Diagram 和 Detail 都是被动响应。

---

## 7️⃣ MVP vs V2 边界（帮你防止过度设计）

### MVP（你现在该做的）
- 单实例 Execution Trace
- 只读
- Timeline + Diagram + Detail

### V2（明确不现在做）
- 多实例对比
- Replay 控制条
- 人工 Retry / Skip
- 权限系统

---

## 8️⃣ 为什么这个 UI 设计“刚刚好”

- 不侵入引擎核心
- 100% 基于 history / token / node
- 完美体现你引擎的：
  - Token 并发模型
  - Retry / Timer / External Task

这是 **为工程师设计的 BPM UI**，不是给业务人员画流程的。

---

> 下一步：
> - 可以直接根据这个拆 React 组件
> - 或我帮你 **反向设计一套专用 Trace API**

