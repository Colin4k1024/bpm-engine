# ADR-012: 补偿状态机——持久执行、逆序、重试与人工接管

## 决策信息

- **编号**: ADR-012
- **标题**: 补偿步骤的持久状态机、逆序执行、并行分支顺序、重试分类、人工接管与失败终态；与既有 Saga 记录兼容
- **状态**: Accepted
- **日期**: 2026-11-02
- **Owner**: tech-lead
- **关联**: Roadmap N4-1（Issue #70）、依赖 N4-2（Issue #71，持久补偿执行器）、N4-3（Issue #72，操作员恢复 API）；ADR-008（实例取消/终止语义）、ADR-011（at-least-once 调用，at-most-once 完成——补偿调用遵循同一不变式）、ADR-009（定义状态机的方法论）

## 背景与约束

### 现行补偿基础设施

`CompensationRecordRow`（`crates/storage/src/compensation.rs:32-47`）：
```
{ id, instance_id, node_id, handler_ref, order, status, created_at }
```
status 为 `"Pending"` / `"Completed"` / `"Failed"`（TEXT 字符串）。

`CompensationRecordRepo`（`:54-60`）：`add(record)` + `list_by_instance(instance_id)`（按 order 升序）。

`EngineEvent` 有 `SagaStarted` / `SagaCompleted`（`crates/core/src/event.rs`）。`EngineContext` 有可选 `compensation_repo`（`crates/runtime/src/handler.rs:38-39`）。Token 有 `ExecutionDirection`（forward / compensation，`crates/core/src/token.rs:65`）。

core 有测试验证补偿逆序（`crates/core/src/lib.rs:514`：C(3) → B(2) → A(1)）。

### 现行缺口（诚实记录）

1. **无持久补偿执行器**：补偿记录被登记（`add`），但执行由应用协调（非引擎驱动）。引擎不创建补偿外部任务、不分配租约、不重试。
2. **无重试分类**：status 只有 Pending/Completed/Failed，无 transient/permanent 区分。失败的补偿无退避、无 DLQ。
3. **无人工接管**：操作员无法重试、跳过或终止单个补偿步骤。
4. **无并行分支顺序定义**：当 fork 的两个分支各自完成可补偿步骤后，补偿顺序未明确定义（哪个分支先补偿？）。
5. **无失败终态处理**：Failed 补偿无 DLQ、无操作员视图、无审计。

硬性约束：

1. `crates/core` 保持纯逻辑。补偿状态机执行是 runtime + storage 层（worker 驱动），不进 core。
2. 既有 `CompensationRecordRow` 结构不破坏（加性扩展 status 值 + 新字段）。
3. 既有 `SagaStarted`/`SagaCompleted` 事件不变（补偿执行是路由层操作，不新增 EngineEvent 变体，ADR-008 §8 先例）。
4. 补偿调用遵循 ADR-011 不变式：at-least-once 调用，at-most-once 完成。
5. Mimosa 约束：新增 SQL 参数绑定。

## 备选方案

### 补偿执行的归属

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: worker 驱动（补偿 = 外部任务 with lease）** | 复用 fetch-and-lock/retry/DLQ 基础设施；补偿是 side-effect（模型/工具调用），worker 执行是自然落点；与 ADR-011 一致 | 需新 task_type 约定 + 补偿任务创建逻辑 |
| B: 引擎内联执行（handler 直接调用补偿） | 简单 | core 违反（side-effect 调用不进 core）；无租约/重试/DLQ；worker 无法参与 |
| C: 应用协调（现行） | 零引擎改动 | 不满足"持久补偿执行器"验收；应用自维护循环；无人工接管 |

**采用方案 A。** 补偿步骤作为外部任务交付给 worker（`task_type = "compensate:{handler_ref}"`），复用 E3-3 的 fetch-and-lock / lease / retry / DLQ 基础设施。

## 决策结果

### 1. 补偿状态机

`CompensationRecordRow.status` 扩展为五态（TEXT 字符串，加性引入）：

| 状态 | 含义 | 可重试？ |
|------|------|:-:|
| `Pending` | 已登记，等待补偿执行 | — |
| `Running` | 补偿任务已创建（worker fetch-and-lock） | — |
| `Completed` | 补偿成功执行 | — |
| `Failed` | 补偿失败（permanent 或重试耗尽） | 操作员可重试 |
| `Skipped` | 操作员显式跳过（人工接管） | — |

合法跃迁：
```
Pending ──(compensation triggered)──▶ Running ──(worker complete)──▶ Completed
                                           │
                                           ├──(worker fail, transient, retries>0)──▶ Running (retry)
                                           ├──(worker fail, permanent / retries=0)──▶ Failed
                                           └──(operator skip)──▶ Skipped
Failed ──(operator retry)──▶ Pending ──▶ Running ──▶ Completed/Failed
```

**不变式**：`Completed` 和 `Skipped` 是终态（不可回退）。`Failed` 可被操作员重试（→ Pending）。

### 2. 步骤登记（compensatable step registration）

当一个可补偿步骤（ExternalTask with `compensatable: true`，或 BPMN compensation boundary event 的宿主）完成时：
1. 引擎在 `CompensationRecordRepo` 中 `add` 一条 `Pending` 记录：`{id, instance_id, node_id, handler_ref, order, status: "Pending", created_at}`。
2. `handler_ref` = 补偿任务的 `task_type`（如 `"compensate:undo-payment"`）。
3. `order` = 完成顺序（单调递增，用于逆序执行）。

**与既有 Saga 记录兼容**：`CompensationRecordRow` 结构不变（status 字段从 3 值扩展到 5 值，TEXT 列加性）。既有 `SagaStarted`/`SagaCompleted` 事件不变（路由层操作，不新增 EngineEvent，ADR-008 §8 先例）。旧实例的补偿记录（status 为 Pending/Completed/Failed）在新状态机中继续有效。

### 3. 逆序执行（reverse-order compensation）

当补偿被触发（实例取消、boundary error、或操作员手动触发）：
1. `list_by_instance(instance_id)` 获取全部记录（按 order 升序）。
2. 过滤 `status == "Pending"` 的记录。
3. 按 `order` **降序**排列（逆序：最后完成的先补偿）。
4. 逐条创建补偿外部任务（`task_type = handler_ref`），worker fetch-and-lock 执行。
5. 前一条 Completed 后才创建下一条（串行逆序，不并行）。

**并行分支顺序**：fork 的两个分支各自完成可补偿步骤后，`order` 反映完成时间顺序。逆序执行按 order 降序——最后完成的分支的步骤先补偿。这是确定性的（order 是单调递增的完成序号）。如果两个分支的步骤几乎同时完成（并行），order 的分配顺序由引擎的 token 完成顺序决定（确定性）。不保证跨分支的特定补偿顺序——只保证同一分支内逆序。

### 4. 重试与失败分类

补偿任务作为外部任务，遵循 ADR-011 的重试分类：
- `Transient`（timeout, rate_limited, provider_error）→ 减计数重投 + 退避（`compute_backoff`）
- `Permanent`（bad_input, tool_not_allowed, budget_exceeded）→ Failed + DLQ

补偿任务的 `retries` 从可补偿步骤的节点定义继承（或默认 3）。

**Failed 补偿的 DLQ**：失败的补偿进入 DeadLetterStore（`error_message = "compensation failed: {reason}"`），操作员可通过 DLQ API 重试或跳过。

### 5. 人工接管（operator intervention）

操作员可通过 REST API：
- `POST /api/v1/process-instances/:id/compensation/retry/:record_id` — 重试 Failed 补偿（→ Pending → Running）
- `POST /api/v1/process-instances/:id/compensation/skip/:record_id` — 跳过补偿（→ Skipped，终态）
- `GET /api/v1/process-instances/:id/compensation` — 查看补偿进度（全部记录 + 状态）

这些操作记录 actor（审计，复用 N1-4 的 DefinitionAuditStore 模式或 HistoryRepo）。

### 6. 失败终态

当补偿 Failed 且操作员不干预：
- 实例保持其当前状态（Terminated/Completed 不变——补偿是善后，不影响实例终态）
- DLQ 条目是操作员的处置杠杆
- 历史记录 `CompensationFailed`（含 record_id, node_id, reason, actor）

### 7. 与 ADR-011 的关系

补偿调用遵循 ADR-011 的不变式：
- **at-least-once 调用**：补偿任务的租约过期 ⇒ 重投 ⇒ worker 可能再次执行补偿
- **at-most-once 完成**：补偿任务 complete 驱动 CompensationRecordRow.status → Completed（CAS，恰好一次）
- **副作用幂等**：补偿的 side-effect（如退款）必须幂等——worker 责任（ADR-011 §1）

### 8. 可测样例（供 N4-2/N4-3 实现与验收）

1. **逆序执行**：步骤 A(order=1) → B(order=2) → C(order=3) 完成；补偿触发 → 执行顺序 C → B → A。
2. **并行分支**：fork 分支 1 完成 A(order=1)，分支 2 完成 B(order=2)；补偿 → B → A（按 order 降序）。
3. **重试**：补偿任务 transient 失败 → 减计数重投 → 重试成功 → Completed。
4. **permanent 失败**：补偿任务 permanent 失败 → Failed + DLQ；操作员 retry → Pending → Running → Completed。
5. **跳过**：操作员 skip → Skipped（终态）；下一条补偿继续执行。
6. **兼容**：旧实例的 Pending/Completed/Failed 记录在新状态机中有效。
7. **取消后补偿**：实例 Terminated → 补偿触发 → 全部 Pending 记录逆序执行。

## 非目标（明确不做）

- **不做并行补偿执行**（串行逆序，不并行——简单且可预测；并行补偿的顺序复杂度推迟）。
- **不做补偿的补偿**（补偿步骤本身不可补偿——一级补偿，不递归）。
- **不改变 core 的 Saga/Token 结构**（`ExecutionDirection` 不变；补偿状态机是 storage + runtime 层）。
- **不新增 EngineEvent 变体**（补偿执行是路由层操作，通过 HistoryRepo 记录审计，ADR-008 §8 先例）。
- **不做跨实例补偿**（补偿是实例内的，不跨实例）。
- **不做自动补偿触发**（补偿由取消/boundary error/操作员触发，不自动——何时补偿是业务决策）。

## 后续动作

- **N4-2**（Issue #71）：实现持久补偿执行器——`CompensationRecordRow.status` 扩展、补偿任务创建逻辑（runtime）、worker 驱动执行、重试/DLQ、启动清扫（Failed 记录的重驱）。
- **N4-3**（Issue #72）：操作员恢复 API——`GET /compensation`（进度视图）、`POST /retry`、`POST /skip`、RBAC + 审计。
- `deploy/schema.sql`：同步补偿表的 status 值扩展（如果新增列）。
- `docs/saga.md`（或新建 `docs/compensation.md`）：用户侧补偿参考。
