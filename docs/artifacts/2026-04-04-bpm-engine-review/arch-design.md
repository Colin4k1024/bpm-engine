---
artifact: arch-design
task: bpm-engine-review
date: 2026-04-04
role: architect
status: draft
---

# BPM Engine 架构审查记录

> 本文档记录对 bpm-engine 项目架构的审查发现，不是新的设计方案。

---

## 1. 系统边界

### 组件拆分

```
bpm-engine (workspace root)
├── crates/core          # 纯逻辑：ProcessDefinition, NodeType, Token, EngineEvent, Saga
├── crates/storage       # 异步存储 traits（ProcessInstanceStore, TokenStore, ExternalTaskStore, TimerStore）
├── crates/runtime       # BpmEngine 事件循环、EngineContext、EventHandlers
├── crates/adapters/memory  # MemoryRepo（in-memory storage 实现）
├── crates/bpmn          # BPMN 2.0 XML parser → ProcessDefinition compiler
├── crates/server/rest   # HTTP API server (axum)
└── crates/worker-sdk    # External task worker runtime
```

### 边界约束

- `crates/core` **无 I/O** — 不依赖 async-trait、tokio、存储traits
- `crates/runtime` **依赖 traits 而非具体实现** — 可接入 MemoryRepo 或 PostgreSQL
- 外部 worker 通过 REST API 与 engine 通信，无直接进程内存共享

---

## 2. 核心数据流

### Token 生命周期

```
TokenCreated → TokenArrived(node) → [NodeHandler] → TokenAdvanced/TokenCompleted
                    ↓
              EngineEvent 驱动
```

### EventPump 处理模型

```rust
// pump.rs
for handler in handlers {
    let new_events = handler.handle(&event, ctx).await;
    queue.extend(new_events);  // 新事件进入 queue 尾部，下一轮 iteration 处理
}
```

**关键约束**：同一 event 的所有 handler 顺序遍历执行，新 events 进入 queue 尾部供下一轮处理。

### Handler 依赖关系

| Handler | 读取 | 写入 |
|---------|------|------|
| TokenArrivedHandler | process_store, token_store | process_store, token_store, parallel_join_repo |
| HistoryHandler | event | history store |
| ProcessStartHandler | — | process_store |
| ProcessCompletedHandler | process_store | process_store |

**已知问题**：HistoryHandler 读取的是原始 event，不是 handler 处理后的 instance 状态。非原子。

---

## 3. 关键设计决策记录

### 决策 1：Token 为执行单元

**结论**：Token 是并发调度的最小单位，每个 token 有独立状态机。

**依据**：支持并行 fork-join 语义，多 token 可同时运行。

### 决策 2：Event-driven 架构

**结论**：所有状态转换由 immutable EngineEvent 驱动。

**依据**：保证可观测性、可重放、崩溃恢复。

### 决策 3：Storage traits 抽象

**结论**：所有存储操作通过 `Option<Arc<dyn XxxStore>>` trait 注入。

**依据**：支持 in-memory 开发、PostgreSQL 生产部署。

---

## 4. 发现的问题

### P0 — fetch_and_lock TOCTOU 竞态

**位置**：`crates/adapters/memory/src/memory_repo.rs` — `ExternalTaskStore::fetch_and_lock`

**描述**：
```rust
// 步骤 1: Read - 筛选 Ready 任务（ReadLock）
let order: Vec<_> = self.external_tasks
    .read().unwrap()
    .iter()  // ...
    .collect();

// 步骤 2: Write - 标记 Locked（WriteLock）
let mut guard = self.external_tasks.write().unwrap();
// 在这两个步骤之间，其他 worker 可能已经修改了同一 task 状态
```

**影响**：多个 worker 可能获得同一 external task，违背"单一 owner" invariant。

**修复方向**：将 Read + Write 合并为单个 WriteLock 临界区，或使用 CAS 原子操作。

### P1 — ParallelJoin group_id 语义

**位置**：`crates/runtime/src/token_arrived_handler.rs` 第 158-196 行

**描述**：
- `ParallelFork` 生成随机 UUID 作为 `group_id`
- `ParallelJoin` 等待 `expected` 个带相同 `group_id` 的 token
- 但 `expected` = 所有进入该节点的所有 incoming sequence flows 总数
- 如果两个独立 fork 的分支汇聚到同一 join，无法区分来源

**影响**：在复杂 BPMN 拓扑下，join 可能过早或过迟触发。

### P1 — in-memory join_state fallback

**位置**：`crates/runtime/src/token_arrived_handler.rs` 第 159-177 行

**描述**：
```rust
let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
    join_repo.try_join(&group_id).await.unwrap_or(false)
} else {
    // FALLBACK TO IN-MEMORY - crash = state loss
    let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
    let mut state = self.join_state.lock().unwrap();
    // ...
};
```

**影响**：与文档承诺的 "Persistence over memory" 矛盾，crash 后 parallel join 无法恢复。

### P2 — NodeType::ServiceTask 死代码

**位置**：`crates/core/src/node.rs` 第 22 行

**描述**：`NodeType::ServiceTask(fn(&mut ProcessInstance))` 变体存在但从未被赋值。BPMN `ServiceTask` 被编译器映射为 `ExternalTask`。

**影响**：代码误导，新开发者可能误用此变体。

### P2 — .bak 文件引用已废弃 API

**位置**：`tests/integration_saga.rs.bak`、`tests/integration_outbox.rs.bak`

**描述**：引用 `bpm_engine::persistence`（不存在），`bpm_engine::model`（不是 re-export）。

**影响**：这些测试从未被正式启用，saga 补偿路径没有通过集成测试验证。

---

## 5. Handler 顺序约束（未文档化）

`design/handler.md` 写道 "Handler 不直接调用 Handler"，但：
- 没有说明是否可以**并发执行**同一 process instance 的 events
- 没有说明 HistoryHandler 记录的时机（event 还是 handler 处理后状态）
- 新 events `extend` 到 queue 尾部的语义没有显式说明

---

## 6. 存储层 async-trait 使用

当前所有 storage traits 使用 `#[async_trait]` 0.1：
- 每个 async 方法调用产生 `Box<dyn Future>` heap allocation
- `EngineContext` 中大量 `Option<Arc<dyn XxxStore>>` 导致 hot path 上的 `Arc::clone` + `as_ref()` 开销
- Rust 1.75+ 支持原生 `async fn` in traits，但需要 MSRV 确认

---

## 7. 文档与代码不一致

| 文档位置 | 文档描述 | 实际代码 | 需更新 |
|----------|----------|----------|--------|
| `docs/bpmn-spec-mapping.md` | ServiceTask → ServiceTask | ServiceTask → ExternalTask | 是 |
| `docs/architecture.md` | Handler 不直接调用 Handler | 正确，但缺少顺序约束 | 部分 |
| `docs/recovery.md` | Memory contains no critical state | TokenArrivedHandler.in_memory_join_state 违反 | 是 |
