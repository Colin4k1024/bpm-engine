好，选 **B** 非常关键 👍
这一步会把你的 `bpm-engine` **从“自定义工作流内核”正式推进到“工业级 BPM 引擎”**。

下面我给你一份 **「BPMN 2.0 → 你当前 Engine 的 Runtime 映射设计」**，目标是：

> **不推翻你现有 Token / Scheduler / External Task / Saga 设计**
> 只在 **Definition Layer** 做 BPMN 适配

这是**可分阶段落地**、**不会把你项目拖进泥潭**的方案。

---

# 🧠 一、总体设计思想（非常重要）

## 核心原则（你一定要坚持）

### ✅ 1. BPMN 只是「定义语言」，不是执行模型

- BPMN → **编译** → Engine 内部 Process Definition
- 运行时 **完全不感知 BPMN**

### ✅ 2. Token 模型保持不变

- Token = 执行的唯一实体
- BPMN 节点 = Token 的「落点 / 转移规则」

### ✅ 3. 只支持 BPMN 的「可执行子集」

第一阶段**绝不追求全量 BPMN**（那是地狱）

---

# 🧩 二、分层架构（Definition → Runtime）

```
BPMN XML
   │
   ▼
BpmnParser
   │
   ▼
BpmnModel (AST)
   │
   ▼
ProcessCompiler
   │
   ▼
Engine ProcessDefinition
   │
   ▼
Token Runtime (你已有的)
```

👉 **关键点**：
`ProcessCompiler` 是整个设计的“心脏”

---

# 📐 三、你需要支持的 BPMN 最小可执行集（MVP）

### 第一阶段（90% 实际业务）

| BPMN 元素          | 是否必须 | 说明          |
| ------------------ | -------- | ------------- |
| startEvent         | ✅       | 单一 start    |
| endEvent           | ✅       | 正常结束      |
| sequenceFlow       | ✅       | 带条件        |
| serviceTask        | ✅       | External Task |
| userTask           | ✅       | Human Task    |
| exclusiveGateway   | ✅       | if / else     |
| parallelGateway    | ✅       | fork / join   |
| boundaryTimerEvent | ⭕       | 可第二阶段    |

⚠️ **暂时不支持**：

- SubProcess
- Event SubProcess
- Call Activity
- Message / Signal（以后可以加）

---

# 🧬 四、BPMN → Engine Core Model 映射

## 1️⃣ BPMN Node → Engine Node

你可以引入一个**统一的中间表示**：

```rust
pub struct CompiledNode {
    pub node_id: NodeId,
    pub node_type: NodeType,
    pub outgoing: Vec<SequenceFlow>,
}
```

### NodeType 映射

```rust
pub enum NodeType {
    Start,
    End,

    ServiceTask {
        task_type: String,
    },

    UserTask {
        form_key: Option<String>,
    },

    ExclusiveGateway,
    ParallelGateway,
}
```

---

## 2️⃣ Sequence Flow（核心）

```rust
pub struct SequenceFlow {
    pub target: NodeId,
    pub condition: Option<Expression>,
}
```

> Expression 可以先支持：

- `${var == "xxx"}`
- `${amount > 100}`

后续你可以换成 CEL / rhai / evalexpr。

---

# 🔁 五、Token 行为映射（重点）

## 🟢 StartEvent

```text
create Token
→ move to outgoing[0]
```

---

## 🔵 ServiceTask（External Task）

```text
Token arrives
→ create ExternalTask
→ Token WAITING
→ Worker complete
→ Token resumes
→ evaluate outgoing
```

对应你现在已有的 External Task 机制，**完全匹配**。

---

## 🟡 UserTask

```text
Token arrives
→ create HumanTask
→ WAITING
→ user completes
→ Token resumes
```

HumanTask 和 ExternalTask 在 Engine 里可以共用一个抽象（Execution）。

---

## 🔀 ExclusiveGateway（XOR）

```text
Token arrives
→ evaluate outgoing in order
→ pick first condition == true
→ move Token
```

⚠️ **BPMN 规范**：

- 最多一个 default flow
- 没有匹配 → error

---

## 🔱 ParallelGateway（Fork）

```text
Token arrives
→ for each outgoing
    spawn new Token
→ original token consumed
```

---

## 🔱 ParallelGateway（Join）

```text
Token arrives
→ wait until all incoming tokens arrived
→ merge → single Token
→ move forward
```

你已经有并行 Token 设计，这一步是 **映射而非重写**。

---

# 🧠 六、Join 的关键：Execution Scope

BPMN 的 join 本质是：

> **同一流程实例、同一 Gateway、同一执行作用域**

### 推荐设计（非常重要）

```rust
struct JoinKey {
    process_instance_id,
    gateway_id,
}
```

- 每个 Token 到达 Join：

  - 写一条 arrival 记录

- 当 arrival_count == incoming_count：

  - 合并 Token
  - 清理 arrival

👉 你已有 Token Join 的话，只需要**挂一个 gateway_id**

---

# 🧩 七、ProcessCompiler（最关键的模块）

### 输入

- `BpmnModel`

### 输出

- `ProcessDefinition`

```rust
pub struct ProcessDefinition {
    pub id: ProcessId,
    pub nodes: HashMap<NodeId, CompiledNode>,
    pub start_node: NodeId,
}
```

---

## Compiler 做 5 件事（缺一不可）

1. 校验 BPMN 合法性
2. 构建节点图
3. 解析 condition expression
4. 识别 gateway join / fork
5. 生成 Engine 可执行结构

---

# 🧪 八、最小实现路径（别一次做太多）

## Phase 1（2–3 周）

- BPMN XML Parser（基于 roxmltree / quick-xml）
- Start / End / ServiceTask
- SequenceFlow（无条件）
- External Task 跑通

👉 **目标：一个 BPMN 文件能跑**

---

## Phase 2（2–3 周）

- ExclusiveGateway + condition
- ParallelGateway fork / join
- UserTask
- 错误处理

---

## Phase 3（可选）

- Boundary Timer Event
- BPMN Error Event
- SubProcess（最难）

---

# 📌 九、你的 repo 中我建议新增的目录

```text
bpm-engine
├── bpmn/
│   ├── parser.rs
│   ├── model.rs
│   ├── compiler.rs
│   └── errors.rs
├── engine/
│   └── process_definition.rs
```

BPMN **永远不要污染 engine runtime 包**

---

# 🏁 十、关键结论（请你记住）

> **BPMN 是 DSL，不是 Runtime**

你现在这个引擎：

- Token / External Task / Saga / Scheduler
  已经 **比 BPMN 强**

BPMN 只是：

> **让用户用标准方式“描述”你已经能执行的东西**

---

## 下一步我可以继续（强烈建议）

你可以继续选一个：

- **B1** → BPMN XML Parser 结构 & 示例代码
- **B2** → Exclusive / Parallel Gateway Join 的并发细节
- **B3** → BPMN Boundary Timer / Error Event 设计
- **B4** → BPMN → Engine 编译错误模型（诊断友好）

直接回 **B1 / B2 / B3 / B4**
我继续陪你把这个引擎打到「专业级」 💪
