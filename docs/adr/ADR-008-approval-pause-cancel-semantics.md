# ADR-008: 审批、暂停与取消语义（引擎自有审批记录、任务创建后置、实例级暂停/恢复/取消、仅一次生效）

## 决策信息

- **编号**: ADR-008
- **标题**: 高风险步骤的可持久化审批（拒绝）、实例级暂停/恢复/取消，以及并发与重试下的仅一次生效
- **状态**: Accepted
- **日期**: 2026-10-28
- **Owner**: tech-lead
- **关联**: Roadmap E5-2（Issue #24）、依赖 E5-1（Issue #23，身份/RBAC/租户）、E3-3（Issue #17，worker 侧 `__approval` 约定与 §3.1 可伪造性警示）、E3-2（Issue #16，permanent 失败 → DLQ + `TokenFailed` 路径）、ADR-001（fetch-and-lock 单租约持有者）、ADR-005（前向 SQL 迁移）

## 背景与约束

E3-3 把"副作用工具执行前必须审批"落在 worker 侧：审批输入是任务变量
`__approval = "true"`。这是一个**可伪造的约定**——引擎像保存任何普通变量一样
保存并重投它，任务的变量写入方可以伪造（`docs/tool-security.md` §3.1 明确以
E5-2 为前向引用记录了该洞）。同时，引擎缺少实例级操作语义：没有暂停/恢复
（TokenStatus::Suspended 存在但从未被使用），没有操作员取消（只有 BPMN
TerminateEnd 内部终止路径）。

硬性约束：

1. `crates/core` 保持纯逻辑（无 I/O、无 async、无存储 trait）。
2. 交付要求：审批与拒绝可持久化；审批/拒绝/取消可从历史解释；并发审批与
   重试下**仅一次生效**；先写本 ADR。
3. E2-4 的 openapi.yaml 与能力目录是规范契约，任何新端点/错误码必须双侧同步
   并由一致性套件锁定。
4. 既有套件（E3-4/E6-3 ai-reference、scheduler_workers、api_contract、
   rbac 等）必须保持绿色：`__approval` 变量路径的既有行为不能破坏。
5. 存储层双适配器（memory + postgres），迁移版本从 0003 之后递增（0004）。

## 备选方案

### 审批记录的归属

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 引擎自有的 ApprovalRecord 存储（新 ApprovalStore trait），任务创建后置** | 审批成为引擎状态，不可由变量通道伪造；worker 侧门读引擎写下的真值；无 fetch 过滤竞态 | 需要新存储 trait + 迁移 + 决策路由 |
| B: 任务先创建、fetch 过滤"未批准不可取" | 立即有 task_id 可挂审批 | fetch 过滤是 TOCTOU 陷阱：取任务与加锁在同一原子段，过滤要下沉到 store 层的每个变体；未批准任务占 lease 语义 |
| C: 继续只用 `__approval` 变量 + REST 保留键 | 零存储改动 | 无法持久化审批状态、无审计、伪造洞不闭合——正是本 issue 要解决的 |

**采用方案 A。**

### 暂停的实现层

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 暂停 = 挂起全部非终态 token（Suspended）+ 实例状态保持 Running** | 复用既有 TokenStatus::Suspended；不动 core 的 InstanceState；历史可解释 | "已暂停"需从 token 状态推导（存在 Suspended token 即视为暂停） |
| B: 新增 InstanceState::Paused | 状态显式 | InstanceState 是 core 类型且被适配器/历史/测试广泛消费，涟漪大 |

**采用方案 A。**

## 决策结果

### 1. 引擎自有的审批记录（不可伪造）

新核心类型（纯逻辑，模式对齐 saga.rs 的 CompensationRecord）：

- `ApprovalStatus`：`Pending` / `Approved` / `Rejected`。
- `ApprovalDecision`：`Approve` / `Reject`（决策路由的请求词汇）。
- `ApprovalEffect`：`Pending` / `Applied`（§7.1 决策/效果可重驱单元的恢复状态；
  REST 线上契约不含它——它是内部恢复状态）。
- `ApprovalRecord`：`{ id, instance_id, node_id, token_id, task_type, status,
  effect, decided_by, decided_at, note, created_at }`。

新存储 trait `ApprovalStore`（crates/storage，memory + postgres 双实现，迁移
`0004_approval_records.sql`）：

- `create(&record)`：**按主体 (instance_id, node_id, token_id) 幂等插入**——
  主体已存在时保留原记录（postgres 用 `ON CONFLICT DO NOTHING` + 主体唯一索引；
  memory 在写锁内查重）。这使 token 重复到达（崩溃恢复、事件重放）不会产生
  重复审批要求。
- `get(id)`、`list_by_instance(instance_id)`。
- `decide(id, decision, actor, note) -> Result<ApprovalDecideOutcome,
  ApprovalError>`：**原子仅一次**——只有第一条 decide 能改写状态；第二条返回
  `AlreadyDecided(原记录)`（带着**最初的**结论），`NotFound` 单独成错。
  postgres 用 `UPDATE ... WHERE id = $1 AND status = 'Pending' RETURNING ...`，
  0 行时回读分类；memory 在单写锁临界区内完成同一判定。两个适配器因此对
  "并发双 decide / decide 与网络重试竞争"给出同一结果：**状态只翻转一次，
  胜者的结论生效，败者得到原结论的回放或冲突码**。
- `mark_effect_applied(id)`（幂等）：效果应用成功后确认（§7.1）。
- `list_decided_with_pending_effect()`：启动清扫的工作清单（§7.1）。

### 2. 审批先于任务创建（task-creation-after-approval）

哪些步骤需要审批（v1，DSL/BPMN 尚无审批标记）：

- external task 的 `task_type` 出现在配置集合
  `engine.approval_required_task_types`（TOML 数组或逗号分隔字符串，
  env `BPM_ENGINE__APPROVAL_REQUIRED_TASK_TYPES`，默认空），**或**
- 该 (instance, node, token) 已存在 ApprovalRecord（崩溃恢复/重放到达时
  记录已在，等待或消费它，而不是再插一条）。

引擎侧（`TokenArrivedHandler` 的 `NodeType::ExternalTask` 分支，审批门在
创建外部任务**之前**）：

| 到达时记录状态 | 行为 |
|----------------|------|
| 无记录且类型需要审批 | 创建 `Pending` 记录；token 转 `Waiting`；**不创建外部任务**（worker 永远看不到未批准的工作） |
| `Pending` | token 维持 `Waiting`，等待决策 |
| `Approved` | 创建外部任务（可 fetch），变量注入 `__approval="true"`（见 §3）；token 转 `Waiting` |
| `Rejected` | 不创建任务；token 维持 `Waiting`（拒绝路径由路由层终结，见 §4） |

为什么先审批后建任务（而不是建了再过滤 fetch）：审批等待完全复用 user task
的"token 等待"模式；未批准的工作**根本不存在任务行**，fetch-and-lock 无需
任何过滤改动，也就没有"过滤在路由层是 TOCTOU、下沉 store 层要改每个变体"
的泥潭。代价与边界（诚实记录）：

- 决策前 token 的 `task_id` 不存在，REST 决策端点使用**审批记录 id**
  （`POST /api/v1/approvals/:approval_id/decide`）；操作员通过
  `GET /api/v1/process-instances/:id/approvals` 发现待审清单。
- v1 审批等待步骤不调度 boundary timer（门在原 handler 的 timer 调度之前
  short-circuit）；批准后任务由路由层直接创建，同样不补挂 timer。
  审批必需步骤挂 boundary timer 的组合推迟到后续迭代。

### 3. `__approval` 变量成为引擎写下的派生真值

批准后创建任务时，引擎把 `__approval="true"` 注入**该任务的变量副本**
（instance 变量的克隆 + 注入，不回写 instance）。worker 侧 E3-3 门
（`TaskInput::approval_granted()`）读到的从此是引擎写下的真值——
`docs/tool-security.md` §3.1 的可伪造性警示就此闭合到"引擎真值"一侧。

与既有 `__approval` 用法的冲突裁决（ai-reference E3-4/E6-3 套件通过
start/user-task 变量传入它）：

- **不**把 `__approval` 加入 start-instance 的保留键拒绝清单（否则破坏既有
  套件）；任务变量通道照旧可携带它。
- 但当某任务类型进入审批必需集合时，**引擎真值获胜**：批准路径注入的
  `__approval="true"` 覆盖实例变量克隆中携带的任何值；未批准则任务根本不
  创建。未列入集合的任务类型行为与 E3-3 完全一致（零变化，既有套件保持
  绿色的机制保证）。
- 路径外的残余风险（诚实记录）：非审批必需任务仍靠 worker 侧约定，变量写
  入方仍可给它注入 `__approval`——把某类型加入
  `approval_required_task_types` 即可让该步骤升级为引擎门。

### 4. 拒绝 = token 终结 + 死信 + 历史可解释

`decide(reject)` 首次生效时（路由层）：

1. 追加历史 `ApprovalRejected`（含 actor、note、approval_id、node/token）。
2. 插入 DLQ 条目（error_message = `approval rejected by <actor>: <note>`，
   task_id 字段填审批记录 id——该步骤从未有外部任务行；条目 id 确定性地取
   `approval:<记录 id>`，插入因此幂等。重放语义：DLQ requeue 是操作员显式
   越过审批门的复活动作，requeue 创建的任务**不携带** `__approval`，
   worker 侧门保持 fail-closed）。
3. 该 token 置 `Terminated`（版本 +1，操作员强制跃迁）。**实例保持其当前
   状态**——与 E3-2 permanent 失败路径完全一致：一个死掉的步骤让实例继续跑
   它的其余分支，DLQ 条目是操作员的处置杠杆；历史里的 `TokenFailed`（与
   死信消息同名原因）解释了这一步被谁拒绝、为何死信。

**拒绝效果与取消的竞态（review F2 修复）**：整个拒绝效果（DLQ 插入、token
终结、实例保存、`TokenFailed` 事件）在 `instance_lifecycle_lock` 内执行，
并在锁内**重新加载**实例：并发取消在锁内写下的 `Terminated` 不可能被本路径
随后保存的旧副本改回 `Running`。若锁内加载到的实例已是终态（取消先到），
效果按"已应用、实例不变更"记录——DLQ 条目与 `ApprovalRejected` 历史解释
拒绝，终态实例不被复活，token 不被二次终结。版本观察哨兵：锁前观察到的
实例版本必须 ≤ 锁内加载的版本（进程互斥下新加载即权威——超出进程边界
写入者的防御性探测，见 §7 的单服务边界）。

### 5. 暂停 / 恢复（实例级）

- **暂停**（`POST /api/v1/process-instances/:id/pause`，operator）：实例
  状态保持 `Running`；全部非终态 token（Created/Ready/Waiting/Executing）
  置 `Suspended`（版本 +1）；追加历史 `InstancePaused`（含 actor）。
- **恢复**（`POST .../resume`）：按节点类型恢复**暂停前语义**（版本 +1）——
  稳态为 Waiting 的节点（timer/message/signal 中间捕获、user task、
  external task、boundary event、call activity）的 token 回到 `Waiting`
  （核心状态机新增合法跃迁 `Suspended → Waiting`：这些 token 暂停前就是
  Waiting，捕获推进按 `Waiting` 匹配，回到 Ready 就永远无法触发）；其余
  token 回到 `Ready`（既有的 `Suspended → Ready` 跃迁，引擎按 Ready
  claim 继续）；追加 `InstanceResumed`。
- **冻结的机制依据**（已核对代码）：message/signal/timer-catch 的推进都按
  `t.status == Waiting` 匹配 token——Suspended token 不匹配，天然冻结；
  `claim_token` 只接受 Ready——Suspended 不可被引擎继续推进。
- **暂停期间触发的 timer：改期，不吞掉（review F3 修复）**。调度器在派发
  `TimerFired` 前把 timer 记录标成 `Fired`；`TimerFiredHandler` 遇到宿主
  token 处于 `Suspended` 时**不消费这次触发**，而是以原 id 重写 TimerRecord
  （`due_at = now + 1s`，status = `Scheduled`——两个适配器的 insert 都是
  id 幂等的 upsert）使其反复重触发，直到恢复把 token 拉回 `Waiting`，
  timer 随之正常触发推进。**截止时间在暂停中存活**：暂停只是延后，不是
  丢弃（boundary-timer 分支同样不会在暂停期间点火；恢复后按同一机制触发）。
- **暂停期间送达的 message/signal：丢弃（v1 明示取舍，review F4）**。它们
  是瞬时事件，v1 无缓冲：捕获 token 处于 `Suspended` 时送达的
  message/signal 不被消费、也不排队——发送方重试（再次 throw），或操作员
  先恢复再送达。该行为由测试钉住（暂停时送达 → 未消费；恢复后送达 →
  消费）。**缓冲重投是 v2 记录项**（未来：按 (instance, node, name) 持久化
  pending delivery，恢复时重投）。
- **飞行中锁的诚实选择：租约继续。** 暂停不回收、不冻结任何外部任务租约：
  worker 仍持有锁、可以 extend-lock。但 `complete`/`fail`（以及 user task
  complete）在实例存在 Suspended token 时被 REST 边界拒绝（409
  `instance_paused`），任务保持 Locked；恢复后 worker 重试 complete 即可
  推进，若租约先过期则任务重投（at-least-once 既有语义）。这是"新步骤在
  转换处被阻断、飞行中的执行不被半途撕毁"的最诚实落点。
- 暂停的幂等：对已暂停实例再暂停 → 200 回放（不追加第二条
  `InstancePaused`）；对终态实例暂停 → 409 `instance_not_pausable`。

### 6. 取消（实例级）

`POST /api/v1/process-instances/:id/cancel`（operator）：复用 TerminateEnd
语义——全部非终态 token 置 `Terminated`，实例置 `Terminated`，发
`ProcessTerminated` 事件，追加历史 `InstanceCancelled`（含 actor）。

- 取消后对该实例的外部任务操作（complete/fail/extend-lock）以稳定码 409
  `instance_cancelled` 拒绝；user task complete 同样拒绝。
- 幂等：对 Terminated 实例取消 → 200 回放当前状态；对 Completed 实例取消
  → 200 回放（no-op，取消只对未终态实例有效）。
- 诚实边界：fetch-and-lock **不过滤**已取消实例的任务（store 层无状态联
  join 的 v1 取舍）——worker 可能取到已取消实例的任务，但其 complete/fail
  被 409 拒绝，工作被丢弃；副作用幂等性本来就是工具作者责任（§8 of
  tool-security.md）。

### 7. 仅一次生效（关键验收）

| 操作 | 竞争场景 | 结果 |
|------|----------|------|
| approve | 并发/重试双 decide | `ApprovalStore::decide` 原子翻转一次；同向第二条 → 200 回放原记录（若效果未落地则先重驱，§7.1）；反向第二条 → 409 `approval_already_decided`。任务只创建一次（效果应用在 §7.1 的锁 + 幂等护栏下） |
| reject | 并发/重试双 decide | 同上；token 恰好终结一次 |
| reject vs. cancel | 并发取消 + 拒绝 | 两者都在 `instance_lifecycle_lock` 内串行执行：实例终态为 `Terminated`（绝不复活为 Running），拒绝恰好记录一次（一条 DLQ + 一条 `ApprovalRejected`），token 恰好终结一次（`TokenFailed` 至多一条，取决于谁先执行终结） |
| pause | 并发/重试双 pause | 200 回放；实例级操作经进程内互斥锁串行化（见下），Suspended 只写一次 |
| resume | 双 resume | 200 回放（无 Suspended token 时为 no-op） |
| cancel | 并发双 cancel | 恰好一次生效：先到者执行终止并发出 `ProcessTerminated`，后到者观察到 Terminated → 200 回放，不再发事件 |

**串行化机制**：路由层的 pause/resume/cancel 与审批效果应用
（`drive_approval_effect`）共享 `AppState` 中的一个进程内异步互斥锁
（`instance_lifecycle_lock`），check-then-act 与实例保存在锁内完成——单服务
内关闭竞态窗口（含 review F2 的"拒绝的迟到保存复活已取消实例"）。多副本部署
（同一 DB 多进程）不在 v1 范围：需要 DB 条件更新（`UPDATE ... WHERE state
<> 'Terminated'` 检查行数），明确推迟并记录于此。

**审批跨重试的存续**：批准按"主体 (instance, node, token)"存续——任务创建
后的 transient 重试（requeue 为 Ready、再 fetch）不会重新进入 token 到达门，
**不要求重新审批**；批准在首次成功转换处被消费（任务创建即消费；记录保留
Approved 供审计）。新的审批要求只在**新的 token 到达**（新的重放/重投产生新
token）时产生。

### 7.1 决策与效果：可重驱单元（review blocker 修复）

decide 与其效果**不是**一个事务，但它们是一个**可重驱单元**——决策是持久
真值，效果幂等且可重驱：

1. `ApprovalStore::decide` 提交决策（status → Approved/Rejected）并把
   `effect` 置为 `Pending`：**决策从此是持久真值**。
2. 路由层随即应用效果（grant → 创建可 fetch 的外部任务；reject → 死信 +
   token 终结 + `TokenFailed`），成功后 `mark_effect_applied(id)`（幂等）。
   效果应用整体在 `instance_lifecycle_lock` 内执行（见 §7 的串行化）。
3. **错误传播**：效果应用失败（如任务插入失败）返回 500 `internal`，报文
   指名审批 id 并说明恢复契约——决策已记录、效果待落地，**操作员重试同一条
   decide 调用**即可。
4. **自愈回放**：对已决记录的**同向** decide 回放 200 前先检查 `effect`——
   仍为 `Pending` 就先重驱效果再作答；反向决策依旧 409。
5. **启动清扫**：服务启动（`build_app` 完成存储装配后、开始服务前）加载
   `list_decided_with_pending_effect()` 的全部记录并逐条重驱（逐条记录日志；
   失败记日志不致命——决策保持持久，重试 decide 仍可重驱）。这是单服务
   恢复故事；postgres 驱动下有实际意义，memory 存储是进程生命周期的
   dev/test 状态（启动即空），清扫天然为 no-op。

**效果幂等性**（重驱永不重复效果）：

- grant：创建前先查 `ExternalTaskStore::get_by_token`——token 已有任务行
  （任何状态）即证明此前（部分）应用已创建任务（任务插入与
  mark_effect_applied 之间崩溃的窗口），重驱不再建第二条。
- reject：DLQ 条目 id 确定性取 `approval:<记录 id>`（两适配器按 id 幂等）；
  token 终结是条件式的——已终结（此前部分应用）则不动、不保存、不再发
  `TokenFailed`（`ApprovalRejected` + DLQ 已是持久审计；崩溃于"保存后、
  事件前"的窄窗可能缺 `TokenFailed`，`ApprovalRejected` 与 DLQ 仍完整解释
  该步）。
- 审计修复：决策历史事件（`ApprovalGranted`/`Rejected`）在 decide 与效果
  之间由路由层追加——该窗口崩溃会丢事件；重驱先按 (event_type,
  approval_id) 检查，缺失则以**记录中的** actor/note 恰好补一条。

**不变式**：记录的 `status` 永不回退（仅一次）；`effect` 只从 `Pending` 走向
`Applied`（`mark_effect_applied` 幂等）；终态/缺失实例上的效果按"已应用、
实例不变更"收敛（grant 不建任务，reject 只记 DLQ）。

### 8. 历史可解释（选择最简机制）

不新增 EngineEvent 变体（事件枚举牵动 outbox/replay/历史映射的全链路）。
审批与实例生命周期是**路由层操作**，与既有 `ExternalTaskLocked` /
`ExternalTaskFailed` 路由层历史事件完全同构：通过 HistoryRepo 以稳定
event_type 追加，payload 携带 actor：

- `ApprovalCreated`（引擎门创建 Pending 记录时，由 handler 经 ctx.history_repo
  追加，无 actor——引擎动作）
- `ApprovalGranted` / `ApprovalRejected`（决策，含 actor/note）
- `InstancePaused` / `InstanceResumed` / `InstanceCancelled`（含 actor）

这些 event_type 不进入 `event_from_outbox` 的可重放集合（replay 只重放
EngineEvent 名）；Trace/History API 的 category 映射新增 `approval` 与
`instance` 类别。审批、拒绝、取消因此**从历史可解释**（谁、何时、对哪个
步骤、批了还是拒了），而无需把操作建模成引擎事件。

### 9. 角色（E5-1 矩阵）

approve/reject（decide）、pause、resume、cancel 全部为 **operator** 动作
（实例生命周期操作）；`GET /api/v1/process-instances/:id/approvals` 是读，
全角色可读。RBAC 表、`docs/rbac.md` 矩阵同步新增行。

### 10. 错误码（E2-4 契约）

新增 5 个稳定码（routes::error_codes ↔ openapi.yaml ↔ 能力目录三方锁定）：

| Code | HTTP | When |
|------|------|------|
| `approval_not_found` | 404 | 未知审批记录 id（跨租户访问同样回此码——不泄漏存在性） |
| `approval_already_decided` | 409 | 对已决记录提交**相反**决策（同向为 200 回放） |
| `instance_paused` | 409 | 暂停期间的外部任务 complete/fail、user task complete |
| `instance_cancelled` | 409 | 已取消实例上的外部任务 complete/fail/extend-lock、user task complete |
| `instance_not_pausable` | 409 | 对终态实例 pause/resume |

## 非目标（明确不做）

- 不做 BPMN/DSL 层面的 `approval-required` 节点标记（配置集合是 v1 的
  唯一入口；定义级标记待 DSL 演进另行决策）。
- 不做多级/会签审批（单记录单决策）。
- 不做 fetch-and-lock 的实例状态过滤（§6 的诚实边界）。
- 不做审批等待期的 boundary timer（§2 的代价）。
- 不做暂停的租约冻结/回收（§5 的诚实选择）。
- 不做暂停期间 message/signal 的**缓冲重投**（§5：v1 明示丢弃，发送方
  重试或先恢复；缓冲按 (instance, node, name) 持久化 pending delivery、
  恢复时重投是 v2 记录项）。
- 不做 E5-3 的完整审计（actor 已随决策与历史事件落盘，正式审计模型归
  E5-3）。
- 不做多副本分布式仅一次（§7）。

## 后续动作

- `docs/approval-semantics.md`：端点、语义矩阵、仅一次表、任务创建后置、
  决策/效果重驱（§7.1）与暂停的诚实语义（本 ADR 的用户侧参考）。
- `docs/tool-security.md` §3.1：可伪造性警示改为"引擎真值"描述。
- `docs/rbac.md`：矩阵新行与错误码表。
- `deploy/schema.sql`：同步 0004 的 `approval_record` 表（含 `effect` 列）。
- E5-3：把 actor 从请求日志升级为正式审计模型时，本 ADR 的历史事件 payload
  已经携带 actor 字段可供消费。
- v2：暂停期间 message/signal 的缓冲重投（§5 的记录项）。
