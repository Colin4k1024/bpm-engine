# 架构一致性审查报告

## 1. 模块边界审查

### 1.1 Crate 分层结构

```
crates/
├── core/           # 核心领域类型，无 I/O 依赖
├── storage/        # 存储抽象层，依赖 core
├── runtime/        # 运行时引擎，依赖 core + storage
├── bpmn/           # BPMN 编译器，依赖 core
├── adapters/memory/  # 内存存储实现，依赖 storage
├── worker-sdk/     # Worker SDK
└── server/rest/    # REST API 服务器
```

### 1.2 依赖方向验证

| 依赖方向 | 预期 | 实际 | 状态 |
|----------|------|------|------|
| `core` → `storage` | 否 | 否 | 正确 |
| `storage` → `core` | 是 | 是 | 正确 |
| `runtime` → `core` | 是 | 是 | 正确 |
| `runtime` → `storage` | 是 | 是 | 正确 |
| `bpmn` → `core` | 是 | 是 | 正确 |

**结论**: 依赖方向正确，无循环依赖。

### 1.3 `crates/core` I/O 纯净性验证

`crates/core/Cargo.toml` 依赖项:
- `serde` - 仅用于序列化
- `serde_json` - JSON 处理
- `uuid` - ID 生成
- `thiserror` - 错误定义

**无以下依赖**:
- `async-trait`
- `tokio`
- 任何网络或存储 I/O crate

`crates/core/src/lib.rs` 模块: `error`, `event`, `external_task`, `instance`, `node`, `process`, `saga`, `token` - 全部为纯内存数据结构。

**结论**: `crates/core` 确实是 I/O 无依赖层，符合设计目标。

### 1.4 存储层抽象边界

`crates/storage/src/lib.rs` 定义了以下 trait:
- `ProcessInstanceStore`
- `TokenStore`
- `ProcessDefinitionStore`
- `ParallelJoinRepo`
- `TimerStore`
- `CompensationRecordRepo`
- `OutboxRepo`
- `ExternalTaskStore`
- `HistoryRepo`

所有 trait 都标记为 `Send + Sync`，支持并发访问。

---

## 2. Critical/High 问题清单

### 2.1 [CRITICAL] `fetch_and_lock` TOCTOU 竞态

**位置**: `crates/adapters/memory/src/memory_repo.rs:417-455`

**问题描述**: `fetch_and_lock` 方法存在 Time-of-Check-to-Time-of-Use (TOCTOU) 竞态条件。

```rust
// Time-of-Check (ReadLock)
let mut order: Vec<(String, String)> = {
    let guard = self.external_tasks.read().unwrap();  // L428
    guard
        .iter()
        .filter(|(_, r)| {
            r.state == ExternalTaskState::Ready && task_types.contains(&r.task_type)
        })
        // ...
};

// Time-of-Use (WriteLock)
let mut guard = self.external_tasks.write().unwrap();  // L443
for task_id in take {
    if let Some(r) = guard.get_mut(&task_id) {
        r.state = ExternalTaskState::Locked;
        // ...
    }
}
```

**影响**:
- 同一 task 可能被多个 worker 同时获取并锁定（违反 external task 独占性）
- 两个 worker 可能获取同一个 task，造成重复执行
- 在高并发场景下可能导致数据不一致

**触发条件**:
- 两个 worker 同时调用 `fetch_and_lock` 且请求重叠的 task 类型

**修复建议**:
1. 将 select 和 lock 操作合并到单个 write lock 作用域内
2. 或者使用 `RwLock` 精细化控制：先读获取候选列表，再写锁获取锁

---

### 2.2 [HIGH] Parallel Join 状态 in-memory fallback 不可恢复

**位置**: `crates/runtime/src/token_arrived_handler.rs:163-177`

**问题描述**: `ParallelJoin` 处理使用两种状态存储策略：
1. `parallel_join_repo` (storage 层) - 用于生产环境
2. `self.join_state` (内存 `Mutex`) - 用于 in-memory fallback

```rust
let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
    join_repo.try_join(&group_id).await.unwrap_or(false)
} else {
    // In-memory fallback - crash 后丢失
    let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
    let mut state = self.join_state.lock().unwrap();
    // ...
};
```

**影响**:
- 如果使用 in-memory fallback（无 `parallel_join_repo` 配置），crash 后 parallel join 状态丢失
- 等待 join 的 tokens 可能永久挂起
- process instance 无法完成

**修复建议**:
1. 文档化明确：in-memory fallback 不支持 crash recovery
2. 或要求 `parallel_join_repo` 必须配置，不允许 fallback 到内存状态
3. 或在启动时检查并警告未完成的 parallel join 状态

---

### 2.3 [HIGH] `NodeType::ServiceTask(fn(...))` 是死代码

**位置**: `crates/core/src/node.rs:22`

**问题描述**: `NodeType::ServiceTask(fn(&mut super::instance::ProcessInstance))` 变体存在但从未被赋值。

代码审查结果:
- `crates/bpmn/src/compiler.rs:384-393` - BPMN `ServiceTask` 被映射为 `NodeType::ExternalTask`
- `crates/runtime/src/token_arrived_handler.rs:86` - 存在处理 `ServiceTask` 的 handler branch
- 但编译器从未生成 `NodeType::ServiceTask`

**实际代码流**:
| 路径 | ServiceTask 映射到 |
|------|-------------------|
| BPMN 编译器 | `ExternalTask` |
| DSL 转换器 (`src/dsl/convert.rs:62`) | `ServiceTask(fn(...))` - 但此路径在 `src/` 不在 `crates/` |

`crates/runtime/src/token_arrived_handler.rs:86` 的 handler branch:
```rust
NodeType::ServiceTask(service) => {
    service(&mut instance);
    // ...
}
```

**影响**: 代码迷惑性高，维护者可能误以为可以创建 `ServiceTask` 节点类型

**修复建议**:
1. 删除 `crates/core/src/node.rs:22` 的 `ServiceTask(fn(...))` 变体
2. 删除 `crates/runtime/src/token_arrived_handler.rs:86-98` 的 dead handler branch
3. 如果 DSL 路径需要支持同步 service task，应在 `crates/dsl/` 中实现

---

### 2.4 [MEDIUM] Handler 顺序约束未文档化

**位置**: `crates/runtime/src/pump.rs:26-30`

**问题描述**: EventPump 将每个事件分发给所有 handlers，但未文档化 handler 顺序依赖约束。

```rust
for handler in handlers {
    let new_events = handler.handle(&event, ctx);
    queue.extend(new_events);
}
```

当前 handlers:
1. `HistoryHandler` - 记录事件到历史表
2. `TokenArrivedHandler` - 处理 token 到达，推进流程
3. `ProcessCompletedHandler` - 处理流程完成
4. `UserTaskCompletedHandler` - 处理用户任务完成
5. 其他 specialized handlers

**潜在问题**:
- `HistoryHandler` 在 `TokenArrivedHandler` 之前运行时，事件会被记录两次
- 如果 handler 依赖于其他 handler 的副作用，顺序错误会导致 bug

**修复建议**:
1. 在 `handler.md` 或代码注释中明确 handler 执行顺序要求
2. 或者改为每个事件只分发给一个针对性的 handler（基于事件类型路由）

---

## 3. 代码异味

### 3.1 重复的 Token 创建逻辑

**位置**: 多处
- `crates/runtime/src/transition.rs` - `move_token` 函数
- `src/legacy_engine.rs:116-130` - `move_token` 函数

两处几乎相同的 Token 创建逻辑。

### 3.2 双重 ParallelJoin 处理逻辑

**位置**: `crates/runtime/src/token_arrived_handler.rs:158-196`

ParallelJoin 同时使用两种状态存储:
1. Storage trait (`ParallelJoinRepo`)
2. In-memory `self.join_state`

这是合理的 fallback 设计，但代码中未明确说明为何需要内存 fallback。

### 3.3 `unwrap()` 在生产代码中

**位置**: `crates/adapters/memory/src/memory_repo.rs:107` 等多处

```rust
self.instances.read().unwrap().get(id).cloned()
```

使用 `unwrap()` 意味着 panic 而不是返回错误。在单线程测试环境下可接受，但在多线程生产环境下可能导致整个进程崩溃。

**建议**: 改用 `expect()` 并添加说明，或重构为返回 `Result`。

### 3.4 硬编码的锁数量限制

**位置**: `crates/adapters/memory/src/memory_repo.rs:263`

```rust
let limit = limit.min(100) as usize;
```

Magic number `100` 在多处出现（263, 329 行），应提取为常量。

---

## 4. 依赖方向审查

### 4.1 Workspace Dependencies vs Local Path

`Cargo.toml` workspace 依赖:
```toml
[workspace.dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
async-trait = "0.1"
```

各 crate 使用情况:

| Crate | 使用 `workspace = true` | 直接版本号 |
|--------|------------------------|-----------|
| `core` | all | - |
| `storage` | all | - |
| `runtime` | `anyhow`, `thiserror`, `uuid`, `serde_json` | `async-trait`, `tokio` |
| `bpmn` | - | `anyhow`, `serde_json` (非 workspace) |
| `adapters/memory` | - | `anyhow`, `async-trait`, `tokio` (非 workspace) |

### 4.2 问题: BPMN 和 Memory Adapter 未使用 workspace 依赖

`crates/bpmn/Cargo.toml` 和 `crates/adapters/memory/Cargo.toml` 未使用 workspace 依赖声明，可能导致版本不一致。

---

## 5. 其他观察

### 5.1 文档与代码不一致

`docs/bpmn-spec-mapping.md` 声称:
> ServiceTask → ServiceTask

但 `crates/bpmn/src/compiler.rs` 实际映射为:
> ServiceTask → ExternalTask

### 5.2 缺少对 `crates/core` 无副作用的测试验证

虽然代码审查确认 `crates/core` 无 I/O 依赖，但缺少测试验证这一属性（如检查 `cargo expand` 输出中无 tokio/async-trait）。

---

## 6. 总结

| 严重程度 | 数量 | 问题 |
|----------|------|------|
| Critical | 1 | `fetch_and_lock` TOCTOU 竞态 |
| High | 2 | ParallelJoin 不可恢复 + ServiceTask 死代码 |
| Medium | 1 | Handler 顺序未文档化 |
| Low | 4 | 代码异味和其他问题 |

### 优先修复建议

1. **立即修复**: `fetch_and_lock` TOCTOU - 影响数据一致性
2. **近期修复**: 删除 `NodeType::ServiceTask` 死代码 - 减少代码迷惑
3. **规划修复**: ParallelJoin crash recovery - 需要架构决策（是否支持 in-memory fallback）

---

*审查时间: 2026-04-04*
*审查范围: crates/core, crates/runtime, crates/storage 及相关模块*
