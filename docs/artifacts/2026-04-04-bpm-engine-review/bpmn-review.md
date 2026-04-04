# BPMN 解析与 Token 语义审查报告

**审查对象**: bpm-engine 项目（Rust 实现的自研 BPM 引擎）
**审查日期**: 2026-04-04
**审查范围**: BPMN 编译器、XML 解析器、EL 表达式引擎、Token 语义处理

---

## 1. 编译器审查

### 1.1 ParallelGateway fork/join 判断逻辑 (`compiler.rs` L396-L403)

```rust
BpmnFlowNode::ParallelGateway { .. } => {
    let inc = incoming_count.get(id).copied().unwrap_or(0);
    if inc != 1 {
        NodeType::ParallelJoin { expected: inc }
    } else {
        NodeType::ParallelFork
    }
}
```

**判断依据**: 编译期根据入边数量（`incoming_count`）静态确定角色。
- `inc == 1` → ParallelFork（一个入边，N 个出边）
- `inc != 1` → ParallelJoin（N 个入边，一个出边）

**验证函数 `check_gateways` (L214-L247) 检查的约束**:
1. `inc != 1 && out != 1` → 错误：不能同时是 fork 和 join
2. `inc == 1 && out < 2` → 错误：fork 必须至少有 2 个出边
3. `inc < 2 && out == 1 && inc != 1` → 错误：join 必须至少有 2 个入边

**问题发现**:

**P2 - 逻辑冗余 Bug (L238)**:
```rust
if inc < 2 && out == 1 && inc != 1 {
```
条件 `inc != 1` 在 `inc < 2`（即 inc == 0 或 inc == 1）时：
- 若 `inc == 1` → `inc != 1` 为 false，条件不触发
- 若 `inc == 0` → `inc != 1` 为 true，条件触发

但 `inc == 0` 的节点在 `check_orphan_nodes` (L139) 已被标记为孤立节点错误，此处检查永远不会被执行到。逻辑冗余，但不影响正确性。

**B1 质疑的结论**:
- **设计上是正确的**：BPMN 规范要求 ParallelGateway 在图中位置决定其是 fork（出度 > 1）还是 join（入度 > 1），不能两者兼具
- **潜在运行时风险**：如果 BPMN XML 描述了一个 `inc == 1 && out == 1` 的 ParallelGateway（即既不是 fork 也不是 join），它会静态地被判定为 ParallelFork，运行时不会报错，但行为不符合预期

### 1.2 XML 解析覆盖度 (`parser.rs`)

**已覆盖的 BPMN 2.0 元素**:
- `startEvent` (L37-L40)
- `endEvent` (L41-L44)
- `serviceTask` (L45-L48, L140-L182)
- `userTask` (L49-L52, L184-L200)
- `exclusiveGateway` (L53-L56, L202-L211)
- `parallelGateway` (L57-L60, L213-L222)
- `sequenceFlow` (L61-L64, L224-L247)

**未覆盖的关键 BPMN 2.0 元素**:
- `subProcess` / `callActivity` — 不支持嵌套流程
- `boundaryEvent` — 不支持边界事件
- `timerEventDefinition` — 定时器事件需外部机制
- `messageEventDefinition` — 消息事件需外部机制
- `multiInstanceLoopCharacteristics` — 多实例（loop）不支持

**P2 - ServiceTask 解析不完整 (L140-L182)**:
```rust
// L164-L171: 只解析 Camunda 扩展命名空间
if attr.namespace() == Some(CAMUNDA_NS) && attr.name() == "topic" {
    task_type = attr.value().to_string();
}
if attr.namespace() == Some(CAMUNDA_NS) && attr.name() == "retries" {
    retries = attr.value().parse().unwrap_or(3);
}
```
仅支持 Camunda 扩展命名空间的 `topic` 和 `retries`，不支持标准 BPMN 2.0 的 `ioSpecification`、`dataInputAssociation`、`dataOutputAssociation`。

### 1.3 EL 表达式解析边界情况 (`el.rs`)

**P1 - 负数无法表达**:
```rust
let ops = [" == ", " != ", " >= ", " <= ", " > ", " < "];
// L36: "-" 不在 ops 中，无法解析 "-1" 或 "x > -1"
```

表达式 `x > -1` 会被解析为：左操作数 `x >`，右操作数 `1`，这不是有效的比较表达式。

**P2 - 运算符优先级混淆**:
```rust
if expr.contains(" or ") {  // L13
    // split by " or " first
}
if expr.contains(" and ") {  // L22
    // then split by " and "
}
```
`or` 和 `and` 按出现顺序split，不考虑优先级（and 优先级高于 or）。表达式 `a or b and c` 会被错误地按 `or` split 为 `[a, b and c]`，然后递归处理。

**P2 - 带引号字符串中的分隔符被错误 split**:
```rust
// L14: 纯文本 split，不检查引号
let parts: Vec<&str> = expr.split(" or ").map(str::trim).collect();
```
表达式 `name == "foo or bar"` 会错误地被 split 为 `name == "foo` 和 `bar"`。

**P2 - VariableEq 条件类型被丢弃**:
在 BPMN 到引擎的编译过程中 (`compiler.rs` L352-L366`):
```rust
fn parse_condition(raw: &str, is_default: bool) -> EdgeCondition {
    if is_default {
        return EdgeCondition::Default;
    }
    // 所有非 default 条件都被解析为 Expression
    EdgeCondition::Expression(inner.to_string())
}
```
`ServiceTask` 的 BPMN XML 中可能包含 `conditionExpression` 但没有 type 属性时，DSL 层不支持 VariableEq 变体（`VariableEq { key, value }`），只有 `Expression(String)`。

---

## 2. Token 语义审查

### 2.1 ParallelFork 处理 (`token_arrived_handler.rs` L141-L157)

```rust
NodeType::ParallelFork => {
    let group_id = uuid::Uuid::new_v4().to_string();  // 每次 fork 生成新 UUID
    if let Some(ref join_repo) = ctx.parallel_join_repo {
        let expected = node.outgoing_edges.len() as u32;
        let _ = join_repo.ensure_group(&group_id, expected).await;
    }
    // ...
    let new_tokens = move_token_with_group(node, group_id.clone());  // 所有子 token 共享 group_id
}
```

**语义正确**：
- Fork 生成新的随机 UUID 作为 `group_id`
- 所有子 token 通过 `move_token_with_group` 创建，继承相同的 `group_id`

### 2.2 ParallelJoin 处理 (`token_arrived_handler.rs` L158-L196)

```rust
NodeType::ParallelJoin { expected } => {
    let group_id = instance.tokens[token_idx]
        .parallel_group_id
        .clone()
        .unwrap_or_default();

    let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
        join_repo.try_join(&group_id).await.unwrap_or(false)
    } else {
        // in-memory fallback
        let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
        let mut state = self.join_state.lock().unwrap();
        let (exp, arrived) = state.entry(key.clone()).or_insert((*expected, HashSet::new()));
        arrived.insert(e.token_id.clone());
        let done = arrived.len() >= *exp;
        if done { state.remove(&key); }
        done
    };

    if done {
        // 清除同 group 的所有 token，创建新 token
        instance.tokens.retain(|t| {
            !(t.node_id == e.node_id && t.parallel_group_id.as_deref() == Some(group_id.as_str()))
        });
        // ...
    }
}
```

### 2.3 B2 核心问题：parallel_group_id 语义混淆

**问题场景**：当两个独立的 ParallelFork 路径汇聚到同一个 ParallelJoin 时

```
Fork1 ─┐
       ├──► Join ──► Next
Fork2 ─┘
```

**问题分析**：

1. Fork1 执行时：生成 `group_id = G1`，创建 token B、C
2. Fork2 执行时：生成 `group_id = G2`，创建 token D、E
3. Join 期望：4 个 token 全部到达后触发

**当前实现的语义**：
- Join 节点的 `expected = 4`（从 compiler 可知，join 的入边数量）
- 每个 token 携带各自的 `group_id`（G1 或 G2）
- Join 处理时：使用到达 token 的 `group_id` 作为 key
- **问题**：Join 检查时只计算同 `group_id` 的 token 是否到齐，而非所有入边 token

**具体代码行为**：

若 token B（group_id=G1）先到达 Join：
```rust
let done = /* check if G1 has 2 tokens arrived */;  // false，不触发
// B 进入 Waiting
```

若 D（group_id=G2）随后到达 Join：
```rust
let done = /* check if G2 has 2 tokens arrived */;  // false，不触发
// D 进入 Waiting
```

此时 G1 有 B(1), G2 有 D(1)，都无法触发 join，永远等待。

**根本原因**：
- BPMN 规范的 ParallelGateway join 需要等待**所有入边**的 token
- 但 `parallel_group_id` 设计为区分**同一 fork 的分支**
- 两者语义不匹配：多个 fork 共享 join 时，`parallel_group_id` 无法表达"所有入边"的概念

**建议修复方向**：
1. Join 不应使用 `parallel_group_id` 来判断完成
2. Join 应检查**该节点所有入边**对应的 token 是否都已到达
3. `expected` 应从编译器传入的是"入边数量"，而非从 token 的 `group_id` 推断

---

## 3. Critical/High 问题清单

### Critical

（无）

### High

| ID | 问题 | 位置 | 描述 |
|----|------|------|------|
| H1 | ParallelJoin 语义错误 | `token_arrived_handler.rs` L158-L196 | 当多个 ParallelFork 汇聚到同一 Join 时，使用 `group_id` 判断完成导致永远等待。Join 应等待所有入边 token，不应依赖 `parallel_group_id` |
| H2 | EL 表达式不支持负数 | `el.rs` L35 | 表达式 `x > -1` 无法正确解析，`-` 不在操作符列表中 |

### Medium

| ID | 问题 | 位置 | 描述 |
|----|------|------|------|
| M1 | EL 运算符优先级混淆 | `el.rs` L13-L30 | `or` 和 `and` 按文本顺序 split，不考虑 and 优先级高于 or |
| M2 | EL 引号内分隔符误 split | `el.rs` L14 | 表达式 `name == "foo or bar"` 会被错误解析 |
| M3 | ServiceTask 解析不完整 | `parser.rs` L164-L171 | 只支持 Camunda 扩展命名空间，不支持标准 BPMN 2.0 属性 |
| M4 | check_gateways 条件冗余 | `compiler.rs` L238 | `inc < 2 && out == 1 && inc != 1` 逻辑冗余，inc==0 已被 orphan 检查覆盖 |
| M5 | VariableEq 条件类型丢失 | `compiler.rs` L352-L366 | BPMN 的 VariableEq 条件在编译时被转换为 Expression，DSL 层不支持 VariableEq 变体 |

---

## 4. 文档一致性

### 4.1 `docs/bpmn-spec-mapping.md` 与代码不一致

**B3 问题确认**：

| 元素 | 文档描述 | 实际代码行为 | 状态 |
|------|----------|--------------|------|
| `ServiceTask` | `ServiceTask` | `ExternalTask { task_type, retries, timeout_secs }` | **不一致** |

**文档 (L12)**:
```markdown
| ServiceTask | ServiceTask | Requires `handler_ref` or extension to map to registered handler. |
```

**代码 (compiler.rs L384-L393)**:
```rust
BpmnFlowNode::ServiceTask { task_type, retries, timeout_secs, .. } => NodeType::ExternalTask {
    task_type: task_type.clone(),
    retries: *retries,
    timeout_secs: *timeout_secs,
},
```

**结论**：文档声称 `ServiceTask → ServiceTask`，但实际编译器将 `ServiceTask` 映射为 `ExternalTask`。这是文档错误，不是代码错误。

### 4.2 其他文档不一致

| 文档 | 位置 | 问题 |
|------|------|------|
| `design/core.md` L132 | `parallel_group_id` 绑定描述 | 描述正确 |
| `docs/docs_execution_model.md` L31, L124 | Token 结构描述 | 与代码一致 |
| `docs/adr/ADR-002-parallel-join-semantics.md` | ParallelJoin 语义 ADR | 存在，需对比实现 |

---

## 5. 审查结论

### 5.1 B1 质疑结论（ParallelGateway fork/join 静态判断）

**结论：设计合理，但存在边界情况风险**

编译器基于图结构的静态判断符合 BPMN 规范。风险点在于：
- 如果 BPMN XML 描述 `inc == 1 && out == 1` 的 ParallelGateway，会被静默判定为 ParallelFork，运行时行为不符合预期
- 建议在 `check_gateways` 中增加对 `inc == 1 && out == 1` 的检查并报错

### 5.2 B2 质疑结论（parallel_group_id 语义）

**结论：存在语义混淆，复杂拓扑下会出错**

当两个独立 ParallelFork 汇聚到同一 Join 时，当前的 `group_id` 机制无法正确处理。建议修改 Join 的完成判断逻辑，不依赖 `parallel_group_id`，而是通过入边计数判断。

### 5.3 B3 质疑结论（文档一致性）

**结论：确认不一致**

`docs/bpmn-spec-mapping.md` 描述 `ServiceTask → ServiceTask`，实际为 `ServiceTask → ExternalTask`。文档需要更新。

---

## 附录：相关文件路径

| 文件 | 路径 |
|------|------|
| BPMN 编译器 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/bpmn/src/compiler.rs` |
| XML 解析器 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/bpmn/src/parser.rs` |
| EL 表达式引擎 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/runtime/src/el.rs` |
| Token 到达处理器 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/runtime/src/token_arrived_handler.rs` |
| Token 转移辅助函数 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/runtime/src/transition.rs` |
| BPMN 规范映射文档 | `/Users/jiafan/Desktop/poc/bpm-engine/docs/bpmn-spec-mapping.md` |
| Token 定义 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/core/src/token.rs` |
| NodeType 定义 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/core/src/node.rs` |
| ParallelJoin 仓储 | `/Users/jiafan/Desktop/poc/bpm-engine/crates/adapters/memory/src/memory_repo.rs` |
