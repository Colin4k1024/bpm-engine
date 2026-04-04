# ADR-001: ExternalTask fetch_and_lock TOCTOU 竞态

- **编号**: ADR-001
- **标题**: ExternalTask fetch_and_lock TOCTOU 竞态
- **状态**: accepted
- **日期**: 2026-04-04
- **Owner**: tech-lead

## 背景与约束

- ExternalTask 需要保证"单一 owner at a time" invariant
- 当前 `MemoryRepo::fetch_and_lock` 实现分两步：Read 筛选 Ready 任务，Write 标记 Locked
- 这两步之间没有原子性保证

## 备选方案

### 方案 A：修复为原子操作

将 Read + Write 合并为单个 WriteLock 临界区：

```rust
let mut guard = self.external_tasks.write().unwrap();
let tasks: Vec<_> = guard.iter()  // 直接在 WriteLock 内筛选
    .filter(|(_, r)| r.state == ExternalTaskState::Ready && ...)
    .map(|(id, r)| (id.clone(), r.created_at.clone()))
    .collect();
// 然后在同一个 WriteLock 内修改状态
```

**优点**：彻底消除 TOCTOU，简单直接
**缺点**：锁粒度略增大，但 external task fetch 不是高频路径

### 方案 B：使用 CAS 原子操作

引入 `AtomicU64` 或 `RwLock` + 条件变量实现乐观锁：

```rust
let task = guard.get_mut(task_id).unwrap();
if task.state != ExternalTaskState::Ready { return Err(...); }
task.state = ExternalTaskState::Locked;
// compare-and-swap
```

**优点**：更细粒度并发
**缺点**：实现复杂度高

### 方案 C：不修复（接受当前行为）

在文档中明确说明"当前 MemoryRepo 不保证 external task 单一 owner，使用 PostgreSQL 适配器可获得更强保证"。

**优点**：无需改动
**缺点**：MemoryRepo 的 external task 行为不可信

## 决策结果

**采用方案**：方案 A — 原子 WriteLock

**实施日期**：2026-04-04
**实施位置**：`crates/adapters/memory/src/memory_repo.rs` 第 417-455 行

将 Read + Write 合并为单个 WriteLock 临界区，消除 TOCTOU 竞态。原有测试全部通过（23 tests）。

## 影响范围

- `crates/adapters/memory/src/memory_repo.rs` — `ExternalTaskStore::fetch_and_lock`
- 所有使用 MemoryRepo 的外部任务 worker

## 企业内控补充

N/A — 开源项目，无企业内控约束。

## 后续动作

- [ ] 确认是否需要修复（方案 A）或接受限制（方案 C）
- [ ] 如修复，编写并发测试验证：N 个 worker 同时 fetch 同一 task_type，验证同一 task 永远只被一个 worker 获得
- [ ] 更新 MemoryRepo 文档说明 external task 语义
