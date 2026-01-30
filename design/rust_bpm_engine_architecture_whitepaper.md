# Rust 原生 BPM Engine

> 一个**事件驱动、Token 驱动、可恢复、支持并行 / 定时 / 重试 / Saga 补偿**的原生 Rust BPM 流程引擎

---

## 1. 项目背景与目标

### 1.1 背景

在 Rust 生态中，缺乏一个：

- 原生实现
- 不依赖 BPMN XML
- 面向工程、可嵌入
- 支持长事务与人工流程

的 BPM（Business Process Management）运行时引擎。

本项目旨在构建一个 **Rust 原生 BPM Runtime Engine**，聚焦 **流程执行内核**，而非低代码或可视化套件。

---

### 1.2 设计目标

- ✅ 纯 Rust 实现（no JVM / no BPMN 强绑定）
- ✅ 流程可暂停、可恢复
- ✅ 支持人工任务（Human Task）
- ✅ 支持并行流程（Fork / Join）
- ✅ 支持定时器、超时、Retry
- ✅ 支持 Saga 补偿（长事务一致性）
- ✅ 事件驱动、可持久化、可扩展

---

### 1.3 明确不做（v1 范围）

- ❌ BPMN XML 解析
- ❌ 可视化建模器
- ❌ 低代码平台
- ❌ 分布式一致性（先单引擎实例）

> 本项目定位为 **BPM Runtime Engine（内核）**

---

## 2. 核心设计理念

### 2.1 BPM 的本质

> **BPM ≠ 调度系统**  
> **BPM = 状态推进引擎**

流程不是函数调用，而是：

- 状态 + 事件 + 时间

---

### 2.2 三大核心抽象

| 抽象 | 含义 |
|----|----|
| Process | 流程定义（静态） |
| Token | 执行权 + 当前位置 |
| Event | 事实驱动的状态变化 |

---

## 3. 总体架构

```
┌──────────────────────────────┐
│        API / Adapter         │
│  REST / gRPC / CLI / MQ      │
└──────────────┬───────────────┘
               ↓
┌──────────────────────────────┐
│      Application Layer       │
│  ProcessService / TaskSvc    │
└──────────────┬───────────────┘
               ↓
┌──────────────────────────────┐
│          BPM Engine          │
│  - Event Dispatcher          │
│  - Token Scheduler           │
│  - Node Executor             │
│  - Gateway Evaluator         │
│  - Saga Coordinator          │
└──────────────┬───────────────┘
               ↓
┌──────────────────────────────┐
│       Persistence Layer      │
│  Repo / UoW / Locking        │
└──────────────┬───────────────┘
               ↓
┌──────────────────────────────┐
│    Infrastructure Layer     │
│  DB / Clock / Logger / Expr  │
└──────────────────────────────┘
```

---

## 4. 核心领域模型

### 4.1 流程定义（Process Definition）

```text
ProcessDefinition
 ├─ id
 ├─ start_node
 └─ nodes: Map<NodeId, Node>
```

```text
Node
 ├─ id
 ├─ type (Start / Service / User / Gateway / End)
 └─ outgoing: Vec<SequenceFlow>
```

---

### 4.2 流程实例（Process Instance）

```text
ProcessInstance
 ├─ id
 ├─ process_definition_id
 ├─ state (Running / Completed / Terminated)
 ├─ variables
 └─ tokens
```

---

### 4.3 Token（核心执行模型）

```text
Token
 ├─ id
 ├─ instance_id
 ├─ node_id
 ├─ status
 ├─ mode (Forward / Compensation)
 └─ parallel_group_id?
```

> **并行不是多线程，而是多 Token**

---

## 5. Token 生命周期（含并行）

### 5.1 Token 状态

- Created
- Ready
- Executing
- Waiting
- Completed
- Terminated

---

### 5.2 并行模型

#### Parallel Fork

- 1 个 Token → N 个 Token
- 原 Token Completed
- 新 Token Ready

#### Parallel Join

- 同一 parallel_group 的 Token 全部到齐
- 创建新 Token 继续流程

---

## 6. 事件驱动模型（Engine Heartbeat）

### 6.1 Event 是事实，不是命令

```text
EngineEvent
 ├─ ProcessStarted
 ├─ TokenArrived
 ├─ TokenCompleted
 ├─ UserTaskCreated
 ├─ UserTaskCompleted
 ├─ TimerScheduled
 ├─ TimerFired
 ├─ TokenFailed
 ├─ SagaStarted
 └─ SagaCompleted
```

---

### 6.2 Handler 模型

```text
Event → Handler → New Events
```

- Handler 只处理事件
- Handler 不直接调用 Handler
- 所有推进都通过 Event

---

## 7. 人工任务（Human Task）

- UserTask 是 Token 的阻塞原因
- UserTask 独立于 Token 存储
- 完成人工任务 = 触发 UserTaskCompleted 事件

---

## 8. 定时器 / 超时 / Retry

### 8.1 Timer 是 Token 的阻塞原因

- Delay
- Timeout
- RetryBackoff

```text
Token (Waiting)
  ↑
 Timer
```

---

### 8.2 Retry 模型

- Retry 不创建新 Token
- 同一 Token 再执行
- Retry 由 Timer 驱动

---

## 9. Saga（补偿）模型

### 9.1 Saga 的本质

> 用流程换事务，用时间换一致性

---

### 9.2 补偿执行规则

- 只补偿 **已成功执行** 的节点
- 补偿顺序 = 正向执行顺序的反向
- 补偿使用 **独立 Token**

---

### 9.3 Saga 生命周期

```text
TokenFailed
  ↓
SagaStarted
  ↓
Compensation Tokens Created
  ↓
SagaCompleted
```

---

## 10. 持久化与恢复

### 10.1 必须持久化的对象

- ProcessInstance
- Token
- UserTask
- Timer
- CompensationRecord

---

### 10.2 Crash Recovery

- Engine 重启
- 加载 Waiting / Ready Token
- 重新投递 Event
- 流程继续执行

---

## 11. Token 并发、乐观锁与事务边界设计

> 本章节定义 **BPM Engine 的并发模型、数据库一致性策略以及事务边界**，是保证引擎“能跑且不乱”的核心。

---

## 11.1 并发设计的基本原则

### 核心结论（先给结论）

1. **并发最小单元 = Token**
2. **同一个 Token 在任意时刻只能被一个 Executor 处理**
3. **不同 Token 之间允许并发**（包括同一流程实例）
4. **不使用数据库锁流程实例**

> 👉 这意味着：流程是并行的，但 Token 是串行的。

---

## 11.2 并发冲突来源分析

### 可能发生并发的场景

| 场景 | 风险 |
|----|----|
| 多线程 / 多 async task 执行 Token | 重复执行 |
| Timer 触发 + 用户完成任务 | 状态覆盖 |
| 并行 Join | 多次 Join |
| Crash Recovery 重放事件 | 幂等性问题 |

因此我们需要：

- Token 级别的并发控制
- 幂等的 Event Handler

---

## 11.3 Token 乐观锁模型（核心）

### Token 持久化结构（关键字段）

```text
Token
 ├─ id
 ├─ instance_id
 ├─ node_id
 ├─ status
 ├─ mode
 ├─ version        ← 乐观锁
 └─ updated_at
```

---

### 更新规则

任何对 Token 的状态变更，必须满足：

```sql
UPDATE token
SET status = ?, version = version + 1
WHERE id = ? AND version = ?
```

- 影响行数 = 1 → 成功
- 影响行数 = 0 → 并发冲突，放弃执行

---

### 设计效果

- ❌ 无需 SELECT FOR UPDATE
- ❌ 不锁流程实例
- ✅ 天然支持多线程 / 多 executor

---

## 11.4 Token Claim（领取）机制

### 为什么需要 Claim

避免多个 Executor 同时执行同一个 Ready Token。

---

### Claim 过程（事务内）

```text
BEGIN
  UPDATE token
  SET status = Executing
  WHERE id = ? AND status = Ready AND version = ?
COMMIT
```

- 成功 → 当前 Executor 获得执行权
- 失败 → Token 已被其他 Executor 处理

> **Claim = 并发闸门**

---

## 11.5 Engine 的事务边界设计

### 核心原则

> **一次 Event Handler 执行 = 一次数据库事务**

---

### Handler 内允许做什么

✅ 允许：
- 修改 Token 状态
- 创建 / 终止 Token
- 写 Timer / UserTask / CompensationRecord
- 产生新的 EngineEvent（Outbox）

❌ 不允许：
- 调用其他 Handler
- 执行耗时 IO（HTTP / RPC）

---

### 推荐事务边界

```text
Handle(Event)
 ├─ BEGIN TRANSACTION
 │   ├─ Load required state
 │   ├─ CAS update Token
 │   ├─ Persist side effects
 │   └─ Persist new Events
 └─ COMMIT
```

---

## 11.6 Event Outbox 模式（强烈推荐）

### 问题

- 事务提交了，但事件没投递
- 事件投递了，但事务回滚

---

### 解决方案：Outbox

```text
outbox_event
 ├─ id
 ├─ payload
 ├─ status (Pending / Published)
```

- Handler 事务内写 Outbox
- Dispatcher 异步投递

---

## 11.7 并行 Join 的并发安全设计

### Join 判定规则

- 同一 parallel_group_id
- 所有 Token 状态 ∈ Completed

---

### 防止多次 Join（关键）

```text
parallel_join
 ├─ group_id (unique)
 └─ joined (bool)
```

Join 时：

```sql
UPDATE parallel_join
SET joined = true
WHERE group_id = ? AND joined = false
```

- 成功 → 创建新 Token
- 失败 → Join 已完成，直接退出

---

## 11.8 Crash Recovery 下的并发保证

### 恢复流程

- 扫描 Token：
  - Ready → 可重新 Claim
  - Executing → 回滚为 Ready（根据超时策略）

---

### 幂等性保证点

- Token 更新：CAS
- Join：唯一约束
- Saga：CompensationRecord 去重

---

## 11.9 并发模型总结（一句话）

> **用 Token 切并发，用版本号抗竞争，用事件串流程。**

---

## 11.10 给实现者的建议（非常实用）

- 不要引入全局锁
- 不要用 synchronized / Mutex 包流程
- 不要跨 Handler 事务

> 如果你感觉“这里需要锁流程实例”，说明 Token 切得还不够细。

---



## 12. Engine Crash Recovery & Rehydrate 设计

> 本章节定义 **当 BPM Engine 进程异常退出后，如何保证流程不丢、不乱、不重复执行**。

---

## 12.1 Crash Recovery 的设计目标

当 Engine 崩溃或重启时，系统必须保证：

1. **不丢 Token**（已创建的执行权不会消失）
2. **不重复执行不可重入节点**
3. **流程可继续推进**
4. **Saga 补偿语义不被破坏**

---

## 12.2 可恢复的前提条件（强约束）

Crash Recovery 成立，必须满足以下前提：

- Token 状态持久化
- Event 使用 Outbox 模式
- 所有状态推进通过 Event
- Handler 事务内只做确定性操作

> 如果违反以上任何一条，恢复将不可预测。

---

## 12.3 Token 状态在 Crash 时的含义

| Token 状态 | Crash 后语义 |
|----|----|
| Ready | 可重新 Claim |
| Waiting | 等待外部事件 / Timer |
| Executing | **不确定状态，需要回滚** |
| Completed | 不再调度 |
| Terminated | 不再调度 |

---

## 12.4 Executing Token 的回滚策略（关键）

### 问题本质

- Token 进入 Executing
- Engine 崩溃
- 外部调用可能成功，也可能失败

---

### 解决策略（推荐）

```text
Executing Token + 超过 heartbeat / timeout
  ↓
回滚为 Ready
```

规则：

- Token 进入 Executing 时记录 `executing_at`
- Recovery 时：
  - 如果 `now - executing_at > max_execution_time`
  - 将 Token 重置为 Ready

> **BPM Engine 不假设外部调用是幂等的**

---

## 12.5 Recovery 启动流程（Engine Boot Sequence）

```text
Engine Boot
  ↓
Load ProcessInstances (Running)
  ↓
Load Tokens
  ↓
Reconcile Token State
  ↓
Reschedule Events
```

---

## 12.6 Token Reconcile 规则

### Reconcile 算法

```text
for token in tokens:
  match token.status:
    Ready      -> enqueue Claim
    Waiting    -> restore Timer / External Wait
    Executing  -> evaluate timeout, maybe reset to Ready
    Completed  -> ignore
    Terminated -> ignore
```

---

## 12.7 Timer 与 Recovery

### Timer 必须是持久化的

```text
Timer
 ├─ id
 ├─ token_id
 ├─ fire_at
 └─ status
```

Recovery 时：

- fire_at < now → 立即触发 TimerFired
- fire_at ≥ now → 重新调度

---

## 12.8 Event Outbox 的恢复语义

### Outbox 状态

| 状态 | 行为 |
|----|----|
| Pending | 重新投递 |
| Published | 忽略 |

> Outbox 是 **事件不丢失的最后防线**

---

## 12.9 Saga 在 Recovery 下的行为

### 补偿一致性保证

- 已记录的 CompensationRecord 永远有效
- Recovery 不新增补偿记录
- Saga 只在 TokenFailed 事件触发

---

## 12.10 幂等性保证点（Checklist）

| 模块 | 幂等手段 |
|----|----|
| Token 更新 | version CAS |
| Join | 唯一约束 |
| Event Handler | 事件唯一 ID |
| Saga | CompensationRecord 去重 |

---

## 12.11 恢复完成判定

Recovery 阶段结束条件：

- 无 Executing Token 处于超时未处理状态
- 所有 Ready Token 已进入调度队列
- Outbox 无 Pending 卡死

---

## 12.12 一句话总结

> **Recovery 不是“回到过去”，而是“在当前状态继续向前”。**

---

## 13. 推荐 Crate 结构

```text
bpm-core/
├── domain/
├── engine/
├── saga/
├── timer/
├── persistence/
├── recovery/
├──

## 13. 演进路线（Roadmap）

### v1
- 单引擎实例
- 代码定义流程
- Token + Event + Saga

### v2
- 并行 Join 优化
- Timer 精度与调度
- BPMN 适配层

### v3
- 分布式 Engine
- 可视化建模
- 多租户

---

## 14. 总结

这个 BPM Engine：

- 不是调度器
- 不是状态机玩具
- 而是一个 **事件驱动的流程执行内核**

> **Token 是灵魂，Event 是血液，Saga 是韧性。**

---

**This document is the contract between architecture and implementation.**

