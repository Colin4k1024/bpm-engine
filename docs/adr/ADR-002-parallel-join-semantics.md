# ADR-002: ParallelJoin group_id 语义澄清

- **编号**: ADR-002
- **标题**: ParallelJoin group_id 语义澄清
- **状态**: accepted（方案 B：基于路径的 group_id）
- **日期**: 2026-04-04
- **Owner**: tech-lead

## 背景与约束

- `ParallelFork` 生成随机 UUID 作为 `parallel_group_id`
- `ParallelJoin` 等待 `expected` 个带相同 `group_id` 的 token
- `expected` = 所有进入该节点的所有 incoming sequence flows 总数（不是单个 fork 的分支数）
- BPMN 语义要求：join 应等待**对应 fork 的所有分支**，而非所有碰巧有相同 group_id 的 token

## 问题分析

当前实现的问题：

1. **group_id 是随机 UUID**，与 fork 的语义身份无关
2. **`expected` 是 incoming flows 总数**，如果两个 fork 汇聚到同一 join，会混合计数
3. **没有验证 token 真的来自对应的 fork**

场景示例：
```
fork1 ─┬─→ join1
       ├─→ (end)
fork2 ─┴─→ join1
```

当前实现中，fork1 和 fork2 可能产生相同的 group_id（因为 group_id 是 fork 时随机生成的），join1 的 `expected` 会累加两个 fork 的分支数，导致计数错误。

## 备选方案

### 方案 A：严格 group_id 语义

保留当前 `expected = incoming_count`，但确保：
- 同一 join 节点只接收来自**唯一一个** ParallelFork 的 token
- 如果两个 fork 的输出汇聚到同一 join，这是**非法 BPMN**，编译器应报错

**优点**：保持简单性
**缺点**：限制了一些合法的 BPMN 拓扑

### 方案 B：基于路径的 group_id

使用 (fork_node_id, fork_instance_counter) 作为 group_id，确保不同 fork 的 token 有不同 group_id：

```rust
let group_id = format!("{}-{}", node_id, self.fork_counter.fetch_add(1));
```

然后 `expected` 在 join 处设为该 group_id 对应的计数器值。

**优点**：支持多个 fork 汇聚到同一 join
**缺点**：需要额外状态追踪

### 方案 C：接受当前限制

在文档中明确说明："当前实现要求每个 ParallelJoin 只能接收来自唯一一个 ParallelFork 的 token。不支持多个 fork 汇聚到同一 join。"

**优点**：无需改动
**缺点**：限制 BPMN 拓扑

## 决策结果

**采用方案**：**方案 B — 基于路径的 group_id**

**决策日期**：2026-04-04

**决策理由**：
1. BPMN 2.0 规范允许 ParallelGateway 的 fork 和 join 解耦（"merged parallel gateway" 变体）
2. 方案 B 支持所有合法 BPMN 拓扑，用户体验最好
3. 实现复杂度可控（仅需额外追踪 `fork_instance_counter`）
4. 方案 A 限制合法 BPMN 拓扑，方案 C 用户影响最大

**实施要点**：
```rust
// ParallelFork: 使用 (node_id, instance_counter) 作为 group_id
let fork_instance = self.fork_counter.fetch_add(1, Relaxed);
let group_id = format!("{}-{}", node.id, fork_instance);
let expected = node.outgoing_edges.len() as u32;
```

**关键修复点**：
1. `ParallelFork` 生成确定性 group_id（fork node id + instance counter）
2. `ParallelJoin` 动态追踪每个 group_id 的到达计数，而非静态 `expected`

**Crash Recovery 注意事项**：
- `fork_counter` 需持久化到 ProcessInstanceStore，确保 crash 后连续性
- 在 PostgreSQL 适配器开发时需同步实现

## 影响范围

- `crates/runtime/src/token_arrived_handler.rs` — ParallelJoin 处理
- `crates/compiler.rs` — ParallelGateway fork/join 判断逻辑
- `crates/bpmn/src/compiler.rs` — BPMN 编译验证

## 企业内控补充

N/A — 开源项目，无企业内控约束。

## 后续动作

- [x] 确认 BPMN 规范中多个 fork 汇聚到同一 join 是否合法（**已确认：允许**）
- [x] ParallelJoin 方案 B 实现决策（**Sprint 2 执行**）
- [ ] 实现 ParallelFork group_id 生成逻辑（`Sprint 2`）
- [ ] 实现 ParallelJoin 动态 group_id 追踪（`Sprint 2`）
- [ ] 补充并发测试：构造多 fork 汇聚到同一 join 的 BPMN，验证 join 行为（`Sprint 2`）
- [ ] 更新 `docs/invariants.md` 第 2 条
- [ ] 更新 `docs/bpmn-spec-mapping.md` 补充多 fork 汇聚场景说明
