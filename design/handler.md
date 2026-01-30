好，这一步是**从“能跑”到“架构成熟”的分水岭**。
下面我给你的是一套 **可以直接落到 Rust trait / struct 的 Engine 事件模型设计**，不是抽象概念。

你可以把它当成 **BPM Engine 的“神经系统”设计稿**。

---

# BPM Engine 事件模型设计

##（Event + Handler）

---

## 1️⃣ 为什么 BPM Engine 必须是事件驱动的？

一句话：

> **流程推进不是函数调用，而是事件响应**

原因：

- 人工任务是“未来事件”
- 定时器是“未来事件”
- ServiceTask 可能失败 / 重试
- 流程恢复 = 重放事件

👉 **事件是 BPM 的天然抽象**

---

## 2️⃣ 事件模型的核心原则（务必遵守）

1. **Engine 只处理 Event**
2. **Event 是事实，不是命令**
3. **一个 Event 只推进一次 Token**
4. **Handler 不直接调用 Handler**

---

## 3️⃣ Event 总体分类（清晰分组）

```
Event
├─ Process Events
├─ Token Events
├─ Task Events
├─ Timer Events
└─ System Events
```

---

## 4️⃣ 事件定义（强类型，不要 String）

### 4.1 核心 Event 枚举

```rust
#[derive(Debug, Clone)]
pub enum EngineEvent {
    ProcessStarted(ProcessStarted),
    TokenArrived(TokenArrived),
    TokenCompleted(TokenCompleted),
    UserTaskCreated(UserTaskCreated),
    UserTaskCompleted(UserTaskCompleted),
    TimerFired(TimerFired),
    ProcessCompleted(ProcessCompleted),
}
```

---

### 4.2 Event Payload 设计

```rust
pub struct ProcessStarted {
    pub process_id: ProcessId,
    pub instance_id: ProcessInstanceId,
}

pub struct TokenArrived {
    pub instance_id: ProcessInstanceId,
    pub token_id: TokenId,
    pub node_id: NodeId,
}

pub struct UserTaskCompleted {
    pub task_id: TaskId,
    pub instance_id: ProcessInstanceId,
    pub node_id: NodeId,
    pub variables: Variables,
}
```

👉 **Event 必须携带“定位信息”，不能依赖全局状态**

---

## 5️⃣ Handler 架构（Engine 的核心）

### 5.1 统一 Handler 接口

```rust
pub trait EventHandler {
    fn handle(
        &self,
        event: EngineEvent,
        ctx: &mut EngineContext,
    ) -> Vec<EngineEvent>;
}
```

> Handler：
>
> - 消费 1 个事件
> - 产出 0..n 个新事件

---

## 6️⃣ EngineContext（事件的执行环境）

```rust
pub struct EngineContext<'a> {
    pub process_repo: &'a dyn ProcessInstanceRepo,
    pub token_repo: &'a dyn TokenRepo,
    pub task_repo: &'a dyn UserTaskRepo,
    pub process_def_repo: &'a dyn ProcessDefinitionRepo,
    pub services: &'a ServiceRegistry,
    pub clock: &'a dyn Clock,
}
```

**重要原则：**

- Handler **不直接操作 DB**
- 只通过 Repo

---

## 7️⃣ 核心 Handler 拆分（不要写一个 God Handler）

```
Handler
├─ ProcessStartHandler
├─ TokenArrivedHandler
├─ ServiceTaskHandler
├─ UserTaskHandler
├─ GatewayHandler
├─ EndEventHandler
```

---

## 8️⃣ Handler 行为示例（重点）

### 8.1 ProcessStartHandler

```rust
impl EventHandler for ProcessStartHandler {
    fn handle(&self, event: EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessStarted(e) = event else {
            return vec![];
        };

        let def = ctx.process_def_repo.load(&e.process_id);
        let token = ctx.token_repo.create(e.instance_id, def.start_node);

        vec![EngineEvent::TokenArrived(TokenArrived {
            instance_id: e.instance_id,
            token_id: token.id,
            node_id: def.start_node,
        })]
    }
}
```

---

### 8.2 TokenArrivedHandler（调度核心）

```rust
impl EventHandler for TokenArrivedHandler {
    fn handle(&self, event: EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenArrived(e) = event else {
            return vec![];
        };

        let node = ctx
            .process_def_repo
            .load_node(e.instance_id, e.node_id);

        match node.node_type {
            NodeType::ServiceTask => {
                vec![EngineEvent::TokenCompleted(TokenCompleted {
                    instance_id: e.instance_id,
                    token_id: e.token_id,
                })]
            }

            NodeType::UserTask => {
                vec![EngineEvent::UserTaskCreated(UserTaskCreated {
                    instance_id: e.instance_id,
                    node_id: e.node_id,
                })]
            }

            NodeType::End => {
                vec![EngineEvent::ProcessCompleted(ProcessCompleted {
                    instance_id: e.instance_id,
                })]
            }

            _ => vec![],
        }
    }
}
```

---

### 8.3 UserTaskCompletedHandler（人工任务恢复）

```rust
impl EventHandler for UserTaskCompletedHandler {
    fn handle(&self, event: EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::UserTaskCompleted(e) = event else {
            return vec![];
        };

        ctx.task_repo.complete(&e.task_id);
        ctx.process_repo.update_variables(e.instance_id, e.variables);

        vec![EngineEvent::TokenArrived(TokenArrived {
            instance_id: e.instance_id,
            token_id: ctx.token_repo.resume(e.instance_id, e.node_id),
            node_id: e.node_id,
        })]
    }
}
```

---

## 9️⃣ Engine 主循环（事件泵）

```rust
pub struct Engine {
    handlers: Vec<Box<dyn EventHandler>>,
}

impl Engine {
    pub fn run(&self, event: EngineEvent, ctx: &mut EngineContext) {
        let mut queue = vec![event];

        while let Some(event) = queue.pop() {
            for handler in &self.handlers {
                let new_events = handler.handle(event.clone(), ctx);
                queue.extend(new_events);
            }
        }
    }
}
```

👉 **这就是 BPM Engine 的“心跳”**

---

## 🔟 事件模型的演进空间（你一定会用到）

| 能力   | 如何演进     |
| ------ | ------------ |
| 异步   | Event Queue  |
| 分布式 | MQ / Kafka   |
| 审计   | Event Store  |
| 恢复   | Event Replay |
| 定时   | TimerEvent   |

---

## 1️⃣1️⃣ 关键架构警告（血的经验）

⚠ **不要在 Handler 里：**

- 调用其他 Handler
- sleep / 阻塞
- 写业务逻辑

⚠ **Event 不是 Command**

- `CompleteUserTask` ❌
- `UserTaskCompleted` ✅

---

## 1️⃣2️⃣ 你现在可以马上开始写的代码

建议顺序：

1. 定义 `EngineEvent`
2. 定义 `EventHandler` trait
3. 写 `ProcessStartHandler`
4. 写 `TokenArrivedHandler`
5. 写 `UserTaskCompletedHandler`

做到这一步，你的 BPM Engine **已经是“正统事件驱动引擎”了**。

---

如果你愿意，下一步我可以继续给你：

- 👉 **Token 生命周期状态图（含并行）**
- 👉 **定时器事件模型（Timer + Retry）**
- 👉 **把这套 Event 模型落成一个可编译的 Rust crate 骨架**

你选一个，我继续。
