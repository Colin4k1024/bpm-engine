---
artifact: bpmn-guide
task: chinese-docs-support
date: 2026-04-04
role: backend-engineer
status: draft
---

# BPMN 2.0 用户指南

## 概述

BPMN（Business Process Model and Notation）2.0 是一种业务流程建模标准，通过图形化表示法描述业务流程的流转逻辑。本引擎实现了 BPMN 2.0 的核心执行语义，将业务流程转换为持久化状态机进行驱动。

### 引擎执行模型

本引擎采用 **Token 驱动** 的执行模型：

- **Token**：执行单元，代表在特定节点上执行的权利
- **Process Instance**：流程实例，持有 Token 和变量，拥有完整的生命周期（Running / Completed / Terminated）
- **EngineEvent**：不可变事件，驱动所有状态转换

所有状态转换均通过事件驱动实现，确保了：
- **可观测性**：完整的执行轨迹
- **可重放性**：从任意状态恢复
- **Crash Safety**：所有状态持久化，引擎可在崩溃后恢复执行

### BPMN 2.0 与引擎的映射关系

| BPMN 2.0 概念 | 引擎抽象 | 说明 |
|---------------|---------|------|
| 流程定义（Process Definition） | ProcessDefinition | 静态图结构，包含节点和顺序流 |
| 流程实例（Process Instance） | ProcessInstance | 流程定义的运行时实例 |
| 执行令牌（Token） | Token | 在节点间移动的执行权利单元 |
| 事件（Event） | EngineEvent | 驱动状态转换的不可变消息 |

---

## 支持的节点元素

### 支持元素表

| BPMN 2.0 元素 | 引擎节点类型 | 说明 |
|--------------|-------------|------|
| StartEvent | Start | 每个流程有且仅有一个起始节点 |
| EndEvent | End | 流程结束节点 |
| UserTask | UserTask | 用户任务，需要人工参与完成 |
| ServiceTask | ServiceTask | 服务任务，需指定 `handler_ref` 关联注册的处理程序 |
| ExclusiveGateway | ExclusiveGateway | 排他网关，根据条件选择唯一分支 |
| ParallelGateway (fork) | ParallelFork | 并行分支网关，一个入口，多个出口 |
| ParallelGateway (join) | ParallelJoin | 并行汇聚网关，多个入口，一个出口，`expected` 字段指定汇聚数量 |

### 节点详细说明

#### StartEvent（起始节点）

流程的唯一入口点。每个流程定义必须包含一个 StartEvent。

```
节点类型值: "StartEvent"
```

#### EndEvent（结束节点）

流程的结束点。流程执行到 EndEvent 时，流程实例标记为 Completed。

```
节点类型值: "EndEvent"
```

#### UserTask（用户任务）

需要人工操作的任务。引擎会为 UserTask 创建等待中的 Token，直到用户提交完成信号。

```
节点类型值: "UserTask"
```

#### ServiceTask（服务任务）

自动执行的后台任务。通过 `handler_ref` 字段关联已注册的处理程序。

```
节点类型值: "ServiceTask"
必须字段: handler_ref  # 指向已注册的 handler 标识符
```

#### ExclusiveGateway（排他网关）

根据条件表达式选择唯一一个外出分支。条件类型支持：
- `VariableEq`：变量等于指定值
- `Expression`：自定义表达式
- `Default`：默认分支（无其他条件满足时）

```
节点类型值: "ExclusiveGateway"
外出顺序流需携带条件表达式
```

#### ParallelGateway（并行网关）

**Fork（分支）**：
- 一个入口 Token 产生 N 个外出 Token
- 用于并行执行多个分支

```
节点类型值: "ParallelGateway"
模式: "fork"
```

**Join（汇聚）**：
- 等待所有同组的 Token 到达后，生成一个外出 Token
- `expected` 字段指定需要等待的 Token 数量

```
节点类型值: "ParallelGateway"
模式: "join"
expected: <等待数量>
```

---

## 不支持的元素

以下 BPMN 2.0 元素在本版本中**不支持**：

| 元素类型 | 说明 |
|---------|------|
| SubProcess | 子流程 |
| CallActivity | 调用活动 |
| EventSubprocess | 事件子流程 |
| BoundaryEvent | 边界事件 |
| TimerEvent | 定时器事件 |
| MessageEvent | 消息事件（引擎有独立的 Timer 机制） |
| MultiInstance (loop) | 多实例（循环） |
| ScriptTask | 脚本任务（建议使用 ServiceTask + handler 替代） |

> **注意**：不支持的元素在流程定义中应避免使用，否则可能导致解析错误或运行时异常。

---

## 流程定义方法

### JSON 结构

引擎接受类 BPMN 的 JSON 格式进行流程定义：

```json
{
  "id": "process_def_id",
  "start": "start_node_id",
  "nodes": [
    {
      "id": "node_id",
      "type": "节点类型",
      "handler_ref": "handler_identifier",
      "outgoing_edges": [
        {
          "target": "target_node_id",
          "condition": {
            "type": "VariableEq | Expression | Default",
            "variable": "variable_name",
            "value": "expected_value"
          }
        }
      ]
    }
  ]
}
```

### 字段说明

| 字段 | 必填 | 说明 |
|-----|-----|------|
| `id` | 是 | 流程定义唯一标识符 |
| `start` | 是 | 起始节点 ID |
| `nodes` | 是 | 节点数组 |
| `nodes[].id` | 是 | 节点唯一标识符 |
| `nodes[].type` | 是 | 节点类型（见支持元素表） |
| `nodes[].handler_ref` | 否 | ServiceTask 关联的处理程序标识符 |
| `nodes[].outgoing_edges` | 否 | 外出边数组 |
| `outgoing_edges[].target` | 是 | 目标节点 ID |
| `outgoing_edges[].condition` | 否 | 条件表达式 |

### 转换方式

BPMN JSON 可通过 `dsl::to_process_definition` 转换为内部 `ProcessDefinition` 结构。
参见源码：`src/dsl/convert.rs`

---

## 示例流程

### 最小流程（Start -> End）

最简单的流程，仅包含起始节点和结束节点：

```json
{
  "id": "minimal",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent"
    },
    {
      "id": "end",
      "type": "EndEvent",
      "outgoing_edges": []
    }
  ]
}
```

> 此流程无需 Worker 处理，引擎自动完成从 Start 到 End 的执行。

---

### 顺序流程（Start -> ServiceTask -> End）

包含服务任务的线性流程：

```json
{
  "id": "order_process",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent"
    },
    {
      "id": "process_order",
      "type": "ServiceTask",
      "handler_ref": "order_handler",
      "outgoing_edges": [
        {
          "target": "end"
        }
      ]
    },
    {
      "id": "end",
      "type": "EndEvent"
    }
  ]
}
```

**执行流程**：
1. Token 从 `start` 节点开始
2. `process_order` 服务任务被触发，调用 `order_handler`
3. 任务完成后，Token 移动到 `end` 节点
4. 流程实例标记为 Completed

---

### 含排他网关的流程

使用 ExclusiveGateway 实现条件分支：

```json
{
  "id": "approval_process",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent"
    },
    {
      "id": "check_approval",
      "type": "ExclusiveGateway",
      "outgoing_edges": [
        {
          "target": "approve_task",
          "condition": {
            "type": "VariableEq",
            "variable": "approved",
            "value": true
          }
        },
        {
          "target": "reject_task",
          "condition": {
            "type": "Default"
          }
        }
      ]
    },
    {
      "id": "approve_task",
      "type": "ServiceTask",
      "handler_ref": "approve_handler",
      "outgoing_edges": [
        {
          "target": "end"
        }
      ]
    },
    {
      "id": "reject_task",
      "type": "ServiceTask",
      "handler_ref": "reject_handler",
      "outgoing_edges": [
        {
          "target": "end"
        }
      ]
    },
    {
      "id": "end",
      "type": "EndEvent"
    }
  ]
}
```

**执行逻辑**：
- 当 Token 到达 `check_approval` 网关时
- 引擎根据 `approved` 变量的值选择分支
- `approved == true` -> `approve_task`
- 否则 -> `reject_task`

---

### 含并行网关的流程

使用 ParallelGateway 实现并行分支和汇聚：

```json
{
  "id": "parallel_process",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent"
    },
    {
      "id": "fork",
      "type": "ParallelGateway",
      "mode": "fork",
      "outgoing_edges": [
        { "target": "task_a" },
        { "target": "task_b" },
        { "target": "task_c" }
      ]
    },
    {
      "id": "task_a",
      "type": "ServiceTask",
      "handler_ref": "handler_a",
      "outgoing_edges": [
        { "target": "join" }
      ]
    },
    {
      "id": "task_b",
      "type": "ServiceTask",
      "handler_ref": "handler_b",
      "outgoing_edges": [
        { "target": "join" }
      ]
    },
    {
      "id": "task_c",
      "type": "ServiceTask",
      "handler_ref": "handler_c",
      "outgoing_edges": [
        { "target": "join" }
      ]
    },
    {
      "id": "join",
      "type": "ParallelGateway",
      "mode": "join",
      "expected": 3,
      "outgoing_edges": [
        { "target": "end" }
      ]
    },
    {
      "id": "end",
      "type": "EndEvent"
    }
  ]
}
```

**执行逻辑**：
1. Token 到达 `fork` 网关
2. 引擎创建 3 个子 Token，分别执行 `task_a`、`task_b`、`task_c`
3. 所有子 Token 完成后，汇聚到 `join` 网关
4. `join` 确认所有 3 个 Token 到达后，生成新 Token 流向 `end`

---

### 复杂示例：审批与并行结合

综合使用 UserTask、ExclusiveGateway 和 ParallelGateway：

```json
{
  "id": "complex_approval",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent"
    },
    {
      "id": "submit_task",
      "type": "UserTask",
      "outgoing_edges": [
        { "target": "check_amount" }
      ]
    },
    {
      "id": "check_amount",
      "type": "ExclusiveGateway",
      "outgoing_edges": [
        {
          "target": "parallel_approval",
          "condition": {
            "type": "VariableEq",
            "variable": "amount",
            "value": "high"
          }
        },
        {
          "target": "quick_approve",
          "condition": {
            "type": "Default"
          }
        }
      ]
    },
    {
      "id": "parallel_approval",
      "type": "ParallelGateway",
      "mode": "fork",
      "outgoing_edges": [
        { "target": "manager_approve" },
        { "target": "finance_approve" }
      ]
    },
    {
      "id": "manager_approve",
      "type": "UserTask",
      "outgoing_edges": [
        { "target": "join" }
      ]
    },
    {
      "id": "finance_approve",
      "type": "UserTask",
      "outgoing_edges": [
        { "target": "join" }
      ]
    },
    {
      "id": "join",
      "type": "ParallelGateway",
      "mode": "join",
      "expected": 2,
      "outgoing_edges": [
        { "target": "end" }
      ]
    },
    {
      "id": "quick_approve",
      "type": "ServiceTask",
      "handler_ref": "auto_approve_handler",
      "outgoing_edges": [
        { "target": "end" }
      ]
    },
    {
      "id": "end",
      "type": "EndEvent"
    }
  ]
}
```

**执行场景**：
- **小额（quick_approve）**：直接自动审批
- **大额（parallel_approval）**：经理和财务并行审批后汇聚结束

---

## BPMN 建模最佳实践

### 流程设计原则

1. **单一职责**
   - 每个节点应代表单一业务动作
   - 避免将复杂业务逻辑压缩到单个 ServiceTask

2. **明确边界**
   - UserTask 应有清晰的人工操作范围
   - ServiceTask 应封装独立的自动化逻辑

3. **条件表达式清晰**
   - ExclusiveGateway 的条件分支应互斥且完整
   - 建议使用 `Default` 分支处理未知情况

4. **并行分支谨慎使用**
   - ParallelGateway 适合可并行执行且结果独立的分支
   - 注意设置正确的 `expected` 汇聚数量

### 节点命名规范

- 使用有业务意义的 ID 和名称
- 避免使用技术性或位置性命名（如 `node1`, `task2`）
- 建议命名格式：
  - UserTask：`{业务动作}_task`（如 `approve_order_task`）
  - ServiceTask：`{服务}_{操作}_task`（如 `send_notification_task`）
  - Gateway：`{判断内容}_gateway`（如 `check_amount_gateway`）

### handler_ref 注册

ServiceTask 必须关联已注册的 handler：

```rust
// handler 注册示例
engine.register_handler("order_handler", my_order_handler);
engine.register_handler("approve_handler", my_approve_handler);
```

未注册的 `handler_ref` 将导致运行时错误。

### 错误处理

- ServiceTask 失败时，引擎会生成 `TokenFailed` 事件
- 失败重试不会创建新 Token，而是重新调度同一 Token
- 建议在 handler 中实现幂等性，确保重试安全

### 变量管理

流程变量用于在节点间传递数据：

```json
{
  "id": "process_with_vars",
  "start": "start",
  "nodes": [
    {
      "id": "start",
      "type": "StartEvent",
      "outgoing_edges": [
        { "target": "task" }
      ]
    },
    {
      "id": "task",
      "type": "ServiceTask",
      "handler_ref": "task_handler",
      "outgoing_edges": [
        { "target": "end" }
      ]
    },
    {
      "id": "end",
      "type": "EndEvent"
    }
  ]
}
```

变量在流程实例生命周期内持久化，可在任意节点访问。

### 性能考量

- 避免过长的并行分支链，可能导致 Token 积压
- UserTask 应设置合理的超时时间
- 定期清理已完成的流程实例（引擎不自动清理）

---

## 与外部系统集成

### External Task Worker

ServiceTask 通过 Worker SDK 与外部系统集成：

```rust
// Worker SDK 使用示例
let worker = Worker::new("payment_worker")
    .handler("payment_handler", handle_payment)
    .start();

async fn handle_payment(ctx: &TaskContext, task: &ExternalTask) -> Result<(), TaskError> {
    // 处理支付逻辑
    let amount = task.get_variable::<f64>("amount").await?;
    // ... 执行支付
    ctx.complete(task).await?;
    Ok(())
}
```

### Timer 机制

引擎有独立的 Timer 机制，不支持 BPMN TimerEvent。Timer 用于：
- UserTask 超时提醒
- 重试间隔控制
- 延迟执行

---

## 参考文档

| 文档 | 说明 |
|-----|------|
| `architecture.md` | 运行时架构概述 |
| `execution-model.md` | Token 生命周期和并发模型 |
| `invariants.md` | 引擎形式化不变量保证 |
| `recovery.md` | 崩溃恢复机制 |
| `database-schema.md` | 持久化 schema 参考 |

---

## 附录：节点类型速查表

| 引擎节点类型 | BPMN 2.0 等价 | 适用场景 |
|------------|--------------|---------|
| `Start` | StartEvent | 流程入口 |
| `End` | EndEvent | 流程结束 |
| `UserTask` | UserTask | 需人工操作的任务 |
| `ServiceTask` | ServiceTask | 自动执行的后台任务 |
| `ExclusiveGateway` | ExclusiveGateway | 条件分支选择 |
| `ParallelFork` | ParallelGateway (fork) | 并行分支 |
| `ParallelJoin` | ParallelGateway (join) | 并行汇聚 |
