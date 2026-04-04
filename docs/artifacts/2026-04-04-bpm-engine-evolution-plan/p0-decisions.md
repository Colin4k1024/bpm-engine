# P0 决策组结论

**讨论组**: rust-reviewer + tech-lead
**日期**: 2026-04-04
**状态**: 已完成

---

## P0-1 fetch_and_lock TOCTOU 竞态

**推荐方案**: A（已实施）

**状态确认**: ADR-001 已在 `crates/adapters/memory/src/memory_repo.rs` 实施，代码审查通过。

**理由**:
1. 将 Read(filter) + Write(state update) 合并为单一 WriteLock 临界区，彻底消除 TOCTOU
2. 符合 `docs/invariants.md` 第 3 条 - External task "Exactly one owner"
3. external task fetch 不是高频路径，性能影响可控

**Rust Idiomatic 评估**: 当前实现是 idiomatic 的。

**可改进点（非 blocking）**: `order.sort_by()` 是 O(n log n)，可用 `Vec::select_nth_unstable_by` 在 O(n) 找到 top-k。

**验证方案**:
- **缺失**: N workers 并发 fetch 同一 task_type 的并发测试
- **建议**: 补充 `tests/concurrent_external_task_fetch.rs`

---

## P0-2 ParallelJoin group_id 语义

**推荐方案**: B（基于路径的 group_id）

**理由**:

### BPMN 规范角度
- BPMN 2.0 规范允许 ParallelGateway 的 fork 和 join 解耦——一个 join 可以接收来自多个 fork 的 token（"merged parallel gateway" 变体）
- 当前实现问题：`expected` 是静态编译时计算，`group_id` 是运行时随机 UUID，两个独立 fork 汇聚到同一 join 时行为错误

### 实现复杂度角度
- 方案 A（严格语义）：最简单，但限制合法 BPMN 拓扑
- **方案 B（路径 group_id）**：需额外追踪 `fork_instance_counter`，但复杂度可控
- 方案 C（接受限制）：无代码改动，但用户体验最差

### 用户影响角度
- 方案 B 支持所有合法 BPMN 拓扑，用户体验最好

**实现要点**:
```rust
// ParallelFork: 使用 (node_id, instance_counter) 作为 group_id
let fork_instance = self.fork_counter.fetch_add(1, Relaxed);
let group_id = format!("{}-{}", node.id, fork_instance);
let expected = node.outgoing_edges.len() as u32;
```

**关键修复点**:
1. `ParallelFork` 生成确定性 group_id（fork node id + instance counter）
2. `ParallelJoin` 动态追踪每个 group_id 的到达计数，而非静态 `expected`

**文档更新**:
- `docs/invariants.md` 第 2 条需更新
- `docs/bpmn-spec-mapping.md` 需补充多 fork 汇聚场景说明

---

## 总结

| P0 | 推荐方案 | 理由 |
|----|---------|------|
| P0-1 fetch_and_lock | **A（已实施）** | 正确保护 Exactly-one-owner invariant |
| P0-2 ParallelJoin | **B** | 支持所有合法 BPMN 拓扑，用户影响最小 |

**后续行动**:
1. P0-1: 补充并发测试 `tests/concurrent_external_task_fetch.rs`
2. P0-2: 修改 `ParallelFork` group_id 生成逻辑 + 修改 `ParallelJoin` expected 追踪机制
3. Both: 更新 ADR 状态为 decided
