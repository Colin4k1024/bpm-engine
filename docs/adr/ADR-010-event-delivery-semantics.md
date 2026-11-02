# ADR-010: 消息/信号投递语义、持久收件箱与等待节点契约

## 决策信息

- **编号**: ADR-010
- **标题**: 消息定向投递与信号广播的语义区分、持久事件收件箱（correlation key、租户范围、TTL、去重键、先于等待到达）、暂停/取消/超时冲突优先级
- **状态**: Accepted
- **日期**: 2026-11-02
- **Owner**: tech-lead
- **关联**: Roadmap N2-1（Issue #62）、依赖 N2-2（Issue #63，PG/内存持久 inbox）、N2-3（Issue #64，REST/SDK/MCP 投递契约）、N2-4（Issue #65，故障矩阵）；ADR-004（timer 调度器嵌入）、ADR-008（§5 暂停期间 message/signal 丢弃——本 ADR 升级为缓冲）、ADR-007（变量契约，correlation 匹配基础）

## 背景与约束

### 现行的消息/信号子系统（纯内存、内部-only）

`EngineEvent`（`crates/core/src/event.rs:216-251`）只有 throw 侧变体
`MessageSent`（:244）和 `SignalSent`（:246），payload 仅携带
`{ instance_id（发送方）, message_name/signal_name }`（:152-168）——**没有目标
实例、没有 correlation key、没有 delivery id**。

catch 侧（`crates/runtime/src/token_arrived_handler.rs:435-439`）：token 到达
`MessageIntermediateCatch`/`SignalIntermediateCatch` 时置 `Waiting`，**不插入任何
持久记录**——与 timer catch 的对比是核心不对称（timer catch 在
`token_arrived_handler.rs:362-387` 插入 `TimerRecord` 到 `TimerStore`，token 有持久
支撑；message/signal catch 仅靠内存中的 `Waiting` token）。

投递是**同 pump 周期内的即时内存消费**：throw 节点 emit `MessageSent`/`SignalSent`
→ `MessageCatchHandler`（`message_handler.rs:14-91`）/`SignalCatchHandler`
（`signal_handler.rs:14-91`）在同 `VecDeque` drain 中扫描全部 running 实例的
`Waiting` token，按 `message_name`/`signal_name` 精确匹配后推进。**无匹配则事件
永久丢弃**（`pump.rs:42-51`）。

### 无外部触发面

无 `/signals`、`/messages`、`/deliver`、`/correlate` 路由（`routes.rs` 路由注册
:5309-5481，与 `openapi.yaml` 双向锁定）。`docs/api-spec.md:267-283` 的
`POST /signals` 是未实现的草图，不在锁定 `openapi.yaml` 中。`BpmEngine` 只暴露
`run_async(initial, ctx)`（`engine.rs:22-42`），无 `deliver_message`/`deliver_signal`
方法。**消息/信号只能由运行中实例的 throw 节点产生，外部无法投递。**

### 暂停期间的取舍（ADR-008 §5，待升级）

ADR-008 §5（:182-187）与 `docs/approval-semantics.md:159-165` 明确：**暂停期间
送达的 message/signal 被丢弃（v1）**——catch token 处于 `Suspended` 时不匹配
（`MessageCatchHandler` 只匹配 `Waiting`），事件不消费、不排队。timer 的对比：
`TimerFiredHandler`（`timer_fired_handler.rs:69-102`）遇到 Suspended 宿主 token 时
**改期重触发**（以原 id 重写 `TimerRecord`，`due_at = now + 1s`），截止时间在暂停
中存活。message/signal 没有对应机制。ADR-008 §5 将"按 (instance, node, name)
持久化 pending delivery、恢复时重投"记为 **v2 记录项**——本 ADR 即该 v2 的契约
锁定。

### correlation 现状

无 correlation key。匹配靠 `message_name`/`signal_name` 精确相等 + 扫描全部 running
实例（`message_handler.rs:43-57`），唯一分区是 tenant（`list_running(tenant_id)`，
`process_store.rs:90`）。`MessageSent`/`SignalSent` payload 不携带目标实例或业务键。

### 存储trait 清单（无 inbox trait）

`crates/storage/src/lib.rs:9-33` 的 12 个 trait 中**无** `EventInboxStore`/
`MessageDeliveryStore`/`PendingEventStore`。`EngineContext`（`handler.rs:27-60`）接线
10 个可选 store，无 inbox 槽位。结构上最接近的先例：
`TimerStore`（`timer_store.rs:62-76`：`insert` + `list_due(now, limit)` +
`mark_fired`，memory + postgres 双实现，`spawn_timer_scheduler` 后台轮询
`timer_scheduler.rs:46`）、`OutboxRepo`（出站事件，pending→claim→mark_published）、
`DeadLetterStore`（insert + list + requeue）。

硬性约束：

1. `crates/core` 保持纯逻辑。新 `EngineEvent` 变体可加（与 ADR-008 §8 的"不新增事件
   变体"不冲突——那是针对路由层审批/生命周期操作；消息/信号投递是引擎事件，
   与 `TimerFired` 同构）。新 core 类型（delivery record 纯数据）放 `crates/storage`
   或 `crates/core`（纯数据，无 trait）。
2. 现有 throw→catch 同 pump 周期的即时语义不能被破坏（E 系列套件中
   message/signal throw→catch 的测试必须保持绿色）。
3. `openapi.yaml` 与路由双向锁定；新投递端点在 N2-3 落地时双侧同步（本 ADR 仅定义
   契约）。
4. Mimosa 约束：新增 SQL 参数绑定。
5. 单服务进程承诺（ADR-008 §7）；多副本 inbox 不在本 ADR 范围。

## 备选方案

### 投递持久化的范围

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 双写——throw/外部触发同时 insert inbox 记录 + emit 内存事件** | 即时消费保留（同 pump 周期）；inbox 记录是崩溃/暂停的持久后备；启动清扫补投 | 双写的"已消费但未 mark_consumed"窄窗需幂等收敛 |
| B: 仅外部触发走 inbox；throw 保持纯内存事件 | 改动小 | throw→暂停中 catch 仍丢弃（ADR-008 §5 的洞未闭合）；throw 后崩溃在 catch 消费前也丢 |
| C: 全部走 inbox 轮询（像 timer） | 单一路径 | 同实例 throw→catch 引入轮询延迟（~1s），破坏即时语义 |

**采用方案 A。** throw 与外部触发双写 inbox + 内存事件：正常路径即时消费（内存
事件驱动），异常路径（崩溃、暂停、catch 未到达）由 inbox 记录兜底。`mark_consumed`
幂等——内存事件已消费但未 mark 时，启动清扫发现 Pending 记录，重投时 catch token
已不在 catch 节点（已推进），记录标 Expired（无副作用）。

### correlation 机制

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 双模式——target_instance_id（直达）或 correlation 谓词（变量匹配）** | 直达免扫描；correlation 支持业务键匹配；确定性（0 或 1 匹配） | correlation 需扫描（有 tenant 分区 + 索引优化空间） |
| B: 仅 message_name 匹配（现行） | 零改动 | 无法定向到特定实例；同 name 多实例时歧义 |
| C: 仅 target_instance_id | 确定性 | 调用方必须知道 instance_id；不支持业务键 correlation |

**采用方案 A。** message 投递携带可选 `target_instance_id`（直达，免扫描）和可选
`correlation`（`{variable_key: expected_value}` 谓词集）。匹配规则见 §3。

## 决策结果

### 1. 消息（定向）与信号（广播）的语义区分

| 维度 | Message | Signal |
|------|---------|--------|
| 投递目标 | 单一实例（correlation 或 target_instance_id 确定） | 广播——同 tenant 全部有匹配 catch 的实例 |
| catch 节点 | `MessageIntermediateCatch { message_name }` | `SignalIntermediateCatch { signal_name }` |
| correlation | 必须（target_instance_id 或谓词）精确匹配 1 个实例 | 无 correlation——按 signal_name 匹配全部 |
| 多实例匹配 | >1 → 409 `message_correlation_ambiguous`；0 → 缓冲 | 全部匹配实例各得一份投递 |
| payload | 注入目标实例变量 | 注入每个匹配实例变量 |
| inbox 记录 | 每条 message 1 条 PendingDelivery | 每个匹配实例 1 条 PendingDelivery（广播展开为 N 条） |

### 2. 持久收件箱记录契约

新存储类型 `PendingDelivery`（纯数据，放 `crates/storage/src/event_inbox.rs`）：

```
PendingDelivery {
    id: String,                      // 投递 id（调用方提供或生成）
    tenant_id: Option<String>,       // 租户范围（与实例 tenant 一致）
    target_instance_id: Option<String>,  // None = 信号广播的匹配实例（展开后非 None）
    delivery_type: DeliveryType,     // Message | Signal
    name: String,                    // message_name 或 signal_name
    correlation: Option<HashMap<String,String>>,  // 谓词（仅 Message）
    payload: HashMap<String,String>, // 消费时注入实例的变量
    status: DeliveryStatus,          // Pending | Consumed | Expired
    dedup_key: String,               // 幂等键（见 §5）
    created_at: String,
    expires_at: Option<String>,      // TTL（见 §4）；None = 永不过期
}
```

新存储 trait `EventInboxStore`（N2-2 实现 memory + postgres，迁移 `0006`）：

```
async fn insert(&self, record: &PendingDelivery) -> Result<()>        // 按 dedup_key 幂等
async fn list_pending(&self, tenant_id, name, target_instance_id?) -> Vec<PendingDelivery>
async fn list_pending_for_instance(&self, instance_id) -> Vec<PendingDelivery>  // 恢复清扫
async fn mark_consumed(&self, id: &str) -> Result<()>                  // 幂等
async fn mark_expired(&self, id: &str) -> Result<()>                   // 幂等
async fn list_expired(&self, now: &str, limit: u32) -> Vec<PendingDelivery>  // TTL 清扫
```

形状对齐 `TimerStore`（`insert` + `list_due` + `mark_fired`）：inbox 的
`list_pending` 对应 timer 的 `list_due`，`mark_consumed` 对应 `mark_fired`。
`EngineContext` 新增 `event_inbox: Option<Arc<dyn EventInboxStore>>` 槽位。

### 3. correlation key 与匹配规则

**Message 投递匹配**（按优先级）：

1. 若提供 `target_instance_id`：仅检查该实例。实例必须有 `Waiting`（或 `Suspended`）
   token 在 `MessageIntermediateCatch { message_name == name }`。变量须满足
   `correlation` 谓词（如有）。匹配 → 投递；实例无匹配 catch → 缓冲（Pending）。
2. 若无 `target_instance_id`：扫描同 tenant running 实例，找有匹配 catch 的
   `Waiting`/`Suspended` token 且 `correlation` 谓词全满足的实例。
   - 恰好 1 个 → 投递
   - >1 个 → 409 `message_correlation_ambiguous`（附匹配实例 id 列表）
   - 0 个 → 缓冲（Pending，等 catch token 到达或实例创建）

**Signal 投递匹配**：扫描同 tenant running 实例，全部有
`SignalIntermediateCatch { signal_name == name }` 的 `Waiting`/`Suspended` token
各产生一条 PendingDelivery（`target_instance_id = 该实例`）。0 个匹配 → 信号瞬时
丢弃（广播无目标时不缓冲——与 message 不同；理由：信号是广播，无特定等待者即
无意义）。

### 4. TTL（投递存活时间）

- 每条 PendingDelivery 有可选 `expires_at`（ISO-8601）。调用方提供或默认
  `created_at + inbox_default_ttl_secs`（配置项，默认 604800 = 7 天）。
- 过期记录由后台清扫（对齐 `reclaim_expired_locks` 的 sweeper 模式）：
  `list_expired(now, limit)` → `mark_expired`。过期记录永不消费。
- `expires_at = None` 表示永不过期（用于无超时的持久等待）。N2-3 的投递端点要求
  调用方显式选择 TTL 或 None。

### 5. 去重键（dedup key）

- 每条 PendingDelivery 有 `dedup_key`。`insert` 按 `dedup_key` 幂等
  （postgres `ON CONFLICT (dedup_key) DO NOTHING`；memory 写锁内查重）。
- `dedup_key` 来源（优先级）：
  1. 调用方提供的 `delivery_id`（外部触发的显式幂等键）
  2. throw 节点：`throw:{instance_id}:{node_id}:{name}:{attempt}`（确定性，同 token
     重放产生同 key → 不重复投递）
  3. 缺省：`sha256(tenant + name + target_instance_id + correlation + payload)`
- **相同投递只推进一次**（验收核心）：重复 insert → 0 行（已存在），原 PendingDelivery
  不受影响；catch 消费时 `mark_consumed` 幂等，重复消费尝试读到 `Consumed` → no-op。

### 6. 事件先于等待节点到达（early-arrival）

**现行行为**：catch token 不存在时事件丢弃。**本 ADR 升级为缓冲**：

- Message：投递时无匹配 catch token → PendingDelivery 入库（`target_instance_id`
  已知或 correlation 确定）。后续 token 到达 `MessageIntermediateCatch` 时，
  `TokenArrivedHandler` 先查 inbox（`list_pending(tenant, name, instance_id)`）：
  有匹配 → 立即消费（`mark_consumed`）+ 注入 payload + 推进 token；无匹配 → token
  置 `Waiting`（现行行为）。
- Signal：投递时无匹配 catch → 瞬时丢弃（§3，广播无等待者无意义）。token 已在
  catch 且 Suspended → 缓冲（Pending，恢复时消费）。

### 7. 暂停/取消/超时冲突优先级

| 到达时实例/token 状态 | 投递行为 | 优先级 |
|---|---|---|
| catch token `Waiting` | 立即消费（`mark_consumed` + 推进） | 正常 |
| catch token `Suspended`（暂停） | **缓冲**（PendingDelivery 入库，`mark_consumed` 推迟） | Pause < Buffer |
| 恢复（`resume`） | 恢复时扫描该实例 PendingDelivery，匹配 catch 的 → 消费 + 推进 | Resume 触发重投 |
| 实例 `Terminated`（取消/完成） | 投递标 Expired（`mark_expired`）；不消费、不推进 | Cancel > Delivery |
| boundary timer 先于 message 触发 | token 离开 catch 节点 → PendingDelivery 标 Expired（token 不再在此 catch） | Timeout > Delivery |
| 投递已过期（TTL） | `mark_expired`；不消费 | TTL > Delivery |

**冲突优先级总序**：Cancel/Completed（终态）> Boundary Timeout > TTL 过期 >
Pause（缓冲）> 正常消费。

**与 ADR-008 §5 的关系**：ADR-008 §5 的"暂停期间 message/signal 丢弃（v1）"被本
ADR 升级为"缓冲（Pending），恢复时重投"。ADR-008 §5 的 v2 记录项
（"按 (instance, node, name) 持久化 pending delivery、恢复时重投"）即本 ADR 的
§7。timer 的"暂停改期"（`timer_fired_handler.rs:69-102`）保持不变——timer 有独立
的 due_at 重排机制，不经过 inbox。

### 8. 双写与启动清扫（崩溃恢复）

**双写**（throw / 外部触发）：
1. `insert PendingDelivery`（inbox，持久）
2. emit `EngineEvent::MessageDelivered`/`SignalDelivered`（新变体，见 §9）或复用
   `MessageSent`/`SignalSent`（内存即时消费）

正常路径：内存事件在同 pump 周期被 catch handler 消费 → `mark_consumed`。
异常路径（崩溃在 emit 后、消费前）：inbox 记录仍 Pending。

**启动清扫**（对齐 timer scheduler 的 initial sweep，`timer_scheduler.rs:61`）：
服务启动时 `list_pending_for_instance` 全量扫描 Pending 记录，对每条：
- 实例存在且 catch token `Waiting` → emit 投递事件（重投）
- 实例存在且 catch token `Suspended` → 保持 Pending（恢复时消费）
- 实例终态/不存在 → `mark_expired`
- catch token 已不在该节点（已推进）→ `mark_expired`（无副作用）
- TTL 已过 → `mark_expired`

**幂等收敛**：`mark_consumed`/`mark_expired` 幂等。内存事件已消费但未 mark 的记录，
清扫时发现 catch token 已离开 → `mark_expired`（token 已推进，不再消费，记录无
副作用）。重复投递（同 dedup_key）→ insert 0 行，不影响原记录。

### 9. 新增 EngineEvent 变体

新增两个投递侧事件（与 `TimerFired` 同构——持久记录驱动的引擎事件）：

```
EngineEvent::MessageDelivered(payloads::MessageDelivered)  // { delivery_id, instance_id, message_name, payload }
EngineEvent::SignalDelivered(payloads::SignalDelivered)    // { delivery_id, instance_id, signal_name, payload }
```

- throw 节点：`MessageIntermediateThrow`/`SignalIntermediateThrow` 在 emit 现有
  `MessageSent`/`SignalSent`（保留，向后兼容）的同时 insert inbox + emit
  `MessageDelivered`/`SignalDelivered`（驱动 inbox 路径的 catch 消费）。
  `MessageSent`/`SignalSent` 保留用于 outbox 发布（出站通知），不驱动 catch 推进。
- 外部触发：直接 insert inbox + emit `MessageDelivered`/`SignalDelivered`。
- catch handler：监听 `MessageDelivered`/`SignalDelivered`，查 inbox 确认 Pending →
  消费 → 推进 token。

**向后兼容**：`MessageSent`/`SignalSent` 保留（outbox/replay/历史映射不变）。
现有 E 系列套件中 throw→catch 的即时语义由 `MessageDelivered` 在同 pump 周期驱动
（catch handler 在同 drain 消费），行为等价。

### 10. 投递端点契约（N2-3 落地，本 ADR 定义语义）

| 端点 | 用途 | 语义 |
|---|---|---|
| `POST /api/v1/process-instances/:id/messages` | 向指定实例投递消息 | target_instance_id = :id；correlation 可选；payload 注入 |
| `POST /api/v1/messages` | 按 correlation 投递消息 | 无 target_instance_id；correlation 匹配；0→缓冲，>1→409 |
| `POST /api/v1/signals` | 广播信号 | signal_name + payload；匹配实例各得一份；0→瞬时丢弃（202） |

投递请求体（message）：
```
{ message_name: String, correlation?: {key: value}, payload?: {key: value},
  delivery_id?: String, ttl_secs?: u64 }
```
投递请求体（signal）：
```
{ signal_name: String, payload?: {key: value}, delivery_id?: String, ttl_secs?: u64 }
```

响应：202 `Accepted`（已入 inbox，不保证即时消费——缓冲场景）；附 `delivery_id`。
即时消费场景返回 200 `Consumed`（附推进的 token 信息）。N2-3 落地时 openapi.yaml
同步。

### 11. 可测样例（供 N2-2/N2-3/N2-4 实现与验收）

1. **定向投递**：message 带 target_instance_id → catch token Waiting → 即时消费，
   payload 注入，token 推进。
2. **early-arrival**：message 投递时 catch token 未到达 → PendingDelivery 入库；
   token 到达 catch → 查 inbox → 消费 + 推进。
3. **暂停缓冲**：catch token Suspended 时投递 message → PendingDelivery 入库
   （不消费）；resume → 扫描 inbox → 消费 + 推进。
4. **取消过期**：实例 Terminated 后投递 message → PendingDelivery 标 Expired；
   resume/cancel 后不消费。
5. **去重**：同 delivery_id 投递两次 → 第二次 insert 0 行；catch 只推进一次。
6. **TTL**：PendingDelivery 过 `expires_at` → sweeper 标 Expired；token 到达时
   不消费。
7. **correlation 歧义**：两个实例同 message_name 且 correlation 均匹配 →
   409 `message_correlation_ambiguous`。
8. **信号广播**：3 个实例有同 signal_name catch → 投递信号 → 3 条
   PendingDelivery，3 个 token 各推进一次。
9. **崩溃恢复**：投递后崩溃（未消费）→ 重启 → 启动清扫重投 → catch Waiting
   → 消费。
10. **throw 向后兼容**：现有 throw→catch 测试（E 系列套件）保持绿色。

## 非目标（明确不做）

- **不做多副本 inbox**（单服务进程承诺；多副本需分布式队列 + leader 选举，另立
  Epic）。
- **不做信号的无目标缓冲**（信号广播无匹配 catch 时瞬时丢弃，不缓冲——与 message
  不同；理由：广播无特定等待者，缓冲无消费方）。
- **不做 message 的"任意实例"投递**（必须有 target_instance_id 或 correlation；
  不支持"同 name 随便给一个"——歧义不可接受）。
- **不改变 throw 节点的即时语义**（throw 仍同 pump 周期驱动 catch；inbox 是持久
  后备，不引入轮询延迟）。
- **不做 inbox 的 outbox 发布**（inbox 是入站投递；出站事件发布走现有
  `OutboxRepo`，两者不混）。
- **不做 boundary message/signal**（现行无 `BoundaryMessage`/`BoundarySignal`
  节点类型，`node.rs:116-123` 仅有 `BoundaryTimer`/`BoundaryError`；新增 boundary
  类型归后续 BPMN 演进）。
- **不改变 timer 的暂停改期机制**（timer 有独立的 due_at 重排，不经 inbox；
  `timer_fired_handler.rs:69-102` 不变）。

## 后续动作

- **N2-2**（Issue #63）：实现 `EventInboxStore` trait + memory/postgres 双实现 +
  迁移 `0006_event_inbox.sql`；`PendingDelivery` 类型；`EngineContext` 新增
  inbox 槽位；双写 + 启动清扫 + TTL sweeper；`MessageDelivered`/`SignalDelivered`
  事件 + catch handler 改造。
- **N2-3**（Issue #64）：REST 投递端点（`:id/messages`、`/messages`、`/signals`）
  + worker SDK 投递方法 + MCP 投递工具；openapi.yaml + 能力目录 + SDK 示例对齐。
- **N2-4**（Issue #65）：真 PostgreSQL 故障矩阵——暂停前后、入库后崩溃、消费中
  崩溃、双投递、取消与过期交错。
- `docs/approval-semantics.md:159-165`：把"暂停期间 message/signal 丢弃（v1）"
  更新为"缓冲（本 ADR §7）"。
- ADR-008 §5：v2 记录项标记为"已由 ADR-010 实现"。
- `deploy/schema.sql`：同步 0006 的 `event_inbox` 表。
