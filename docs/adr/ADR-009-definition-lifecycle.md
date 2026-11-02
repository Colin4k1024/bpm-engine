# ADR-009: 流程定义生命周期状态机、并发发布与版本兼容契约

## 决策信息

- **编号**: ADR-009
- **标题**: 流程定义从草稿到废弃的五态生命周期、同 key 并发发布/激活的并发规则、不可变版本绑定，以及与现行两态部署契约的兼容迁移
- **状态**: Accepted
- **日期**: 2026-11-02
- **Owner**: tech-lead
- **关联**: Roadmap N1-1（Issue #58）、依赖 N1-2（Issue #59，候选校验报告）、N1-3（Issue #60，租户命名空间与原子激活）、N1-4（Issue #61，发布审计与可回退）；ADR-005（前向 SQL 迁移）、ADR-006（DSL，`process.id` 即 definition id）、ADR-007（结构化变量与 artifact URI）

## 背景与约束

现行定义生命周期只有两态（`crates/storage/src/process_store.rs:42-49`）：

```rust
pub enum DefinitionStatus { Active, Deprecated }
```

部署语义（`crates/server/rest/src/routes.rs:4495-4541` `deploy_bpmn`）：接收原始 BPMN
XML → `parse_and_compile` → `deployer.deploy(&def, &body)`。两个适配器在 deploy 时
把新记录置为 `Active`，并把同 `def_key` 的既有 `Active` 记录翻转为 `Deprecated`
（memory `process_def_store.rs:46-53`；postgres `process_def_store.rs:24-38`）。
因此 **每个 key 至多一个 Active 版本**，但部署即激活——没有草稿、验证、发布中间态。

版本号由作者控制：解析自 definition id 的 `key:version` 后缀
（memory `parse_id_and_version:74-81`、postgres `:44-51`、REST
`parse_definition_id_version:1927-1934`，无冒号或非数字后缀时默认 1）。服务端不
自增版本；对同一 id 重复 deploy，postgres 走 `ON CONFLICT (id) DO UPDATE SET
bpmn_xml = $4`（静默覆盖 XML），memory 直接替换编译图。**没有版本单调性校验、没有
不可变性保证、没有"已发布版本不可覆盖"的约束。**

`validate_definition`（`crates/bpmn/src/validation.rs:81`）与 `dry_run`
（`crates/bpmn/src/simulation.rs:142`）是纯函数，仅在 `POST /validate`、
`POST /dry-run` 两条无状态端点被调用（`routes.rs:4688、4724`），结果从不持久化。
`docs/validation-and-dry-run.md:230-233` 明确："部署不隐式校验——调用方自行先调
validate/dry-run。"

实例与版本的绑定：`ProcessInstance.process_def_id`（`crates/core/src/instance.rs:21-42`）
记录带版本号的 id（如 `order-flow:3`）。加载运行中实例的定义时按 id 精确加载
（`load(id)`），不取"当前 active"——**旧实例在旧版本上继续运行今天已成立**。

定义为引擎全局，无租户隔离（`docs/capabilities.md:126`；`process_definition` 表
无 `tenant_id` 列，`0001_init.sql:12-19`）。生命周期操作无审计历史行（仅日志）。
这两项分别归 N1-3、N1-4，本 ADR 不变更。

硬性约束：

1. `crates/core` 保持纯逻辑（无 I/O、无 async、无存储 trait）。`DefinitionStatus`
   枚举在 `crates/storage`，可扩展；`ProcessDefinition`（core）不加版本/状态字段。
2. 现有 `POST /process-definitions/deploy`（原始 BPMN XML）的调用方不能被破坏——
   E 系列 22 个 PR 的套件（ai-reference、scheduler_workers、api_contract、rbac
   等）必须保持绿色。
3. E2-4 的 `openapi.yaml` 与路由注册双向锁定（`openapi_contract.rs`），任何
   status 枚举值变更或新端点必须双侧同步。
4. 迁移前向 only（ADR-005），`process_definition.status` 是 TEXT 列（非枚举类型），
   新状态值可加性引入。
5. Mimosa 约束：新增 SQL 必须参数绑定（`$1…`），禁止拼接。

## 备选方案

### 生命周期状态数

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 五态 Draft→Validated→Published→Activated→Deprecated** | 草稿可迭代不污染可启动集；验证结果可持久化供 Agent 修复；发布即冻结不可变版本；激活受 CAS 保护 | 状态多于现行两态，需迁移与枚举扩展 |
| B: 三态 Draft→Published→Deprecated（合并验证与发布） | 状态少 | 验证结果无处持久化（N1-2 的校验报告无载体）；"已校验但未发布"的中间态缺失，Agent 无法区分"可修复的草稿"与"已冻结版本" |
| C: 保持两态 Active/Deprecated，仅加并发护栏 | 零状态迁移 | 不满足"草稿、验证、发布、激活、废弃"的验收要求；校验报告（N1-2）与审计（N1-4）无状态依托 |

**采用方案 A。** 五态精确对应验收要求的"草稿、验证、发布、激活、废弃"。

### 版本号归属

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 作者声明（`key:version` id）+ 服务端单调性校验** | 与现行 id 契约完全兼容；`process_def_id` 格式不变；解析逻辑不变 | 作者可能跳号（3→5），需校验"严格大于同 key 已发布最大版本" |
| B: 服务端自增版本 | 严格连续无跳号 | 改变 deploy 契约（id 不再由作者控制）；`process_def_id` 格式变更，涟及实例/历史/审计/EL；破坏 E 系列套件 |

**采用方案 A。** 版本号继续由作者经 id 声明，服务端新增**单调性校验**：发布
（Published）时，该 key 的版本号必须严格大于同 key 所有已达到 Published/Activated/
Deprecated 的版本号；否则拒绝（`definition_version_not_monotonic`）。对仍在 Draft
的同 id 草稿，允许重新上传覆盖（草稿可变）。

### 部署快路径的归属

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 保留 `POST /deploy` 为快路径（validate→publish→activate 原子一步）** | 现有调用方零变化；幂等 deploy 语义保留 | 快路径无法停在"已发布未激活"中间态 |
| B: 废弃 `POST /deploy`，改为分步 API | 显式分离 publish 与 activate | 破坏所有现有调用方与套件 |

**采用方案 A。** `POST /deploy` 语义升级为"编译 → 校验 → 发布 → 激活"的原子
快路径，对调用方表现为与今天相同的"部署即可启动"。新增的细粒度端点（草稿上传、
持久化校验、发布、CAS 激活）在 N1-2/N1-3 作为**并行新增**端点引入，不替换快路径。

## 决策结果

### 1. 五态生命周期状态机

`DefinitionStatus`（`crates/storage/src/process_store.rs:42-49`）扩展为五态：

| 状态 | 含义 | 可启动新实例？ | 内容可变？ |
|------|------|:-:|:-:|
| `Draft` | 候选草稿，可反复上传覆盖 | 否 | 是 |
| `Validated` | 通过结构+语义校验，校验报告已持久化 | 否 | 否（冻结为可发布候选） |
| `Published` | 已发布不可变版本，可被激活 | 否（需先 Activated） | 否 |
| `Activated` | 该 key 的当前激活版本，唯一可启动 | 是 | 否 |
| `Deprecated` | 已退役（被新版本取代或手动废弃） | 否 | 否 |

合法跃迁（非法跃迁拒绝，返回 `definition_invalid_transition`）：

```
Draft ──validate──▶ Validated ──publish──▶ Published ──activate──▶ Activated
  │                    │                       │                       │
  └──(重新上传)──▶ Draft     └──(retire)──▶ Deprecated ◀──(retire)───┘
                                              ▲
                          Published ──(retire)──┘
                          Activated ──(deprecate/被新激活取代)──▶ Deprecated
```

跃迁规则：

- `Draft → Validated`：执行 `validate_definition` + `dry_run`，无 Critical finding
  时持久化校验报告并跃迁。有 Critical finding 时保持 Draft，报告仍持久化（N1-2 的
  修复建议载体）。
- `Validated → Published`：分配不可变版本号（单调性校验，§2），冻结 XML 与编译图。
  `Validated` 是可选中间态——草稿可直接走快路径发布，不必停留。
- `Published → Activated`：CAS 激活（§3）。
- `Activated → Deprecated`：手动 deprecate，或被同 key 新版本激活时自动翻转。
- 任何非终态 → `Deprecated`：手动 retire。
- `Deprecated` 是终态，不可回退到任何活跃态。"回退"（N1-4）= 重新激活一个已
  Published 的旧版本（§4）。

DB 迁移 `0005_definition_lifecycle.sql`（前向 only，ADR-005）：

```sql
-- status 是 TEXT 列，新值加性引入，无需 ALTER 类型
UPDATE process_definition SET status = 'activated' WHERE status = 'active';
-- 'deprecated' 值不变
-- Draft/Validated/Published 由新端点写入，迁移不回填（旧数据只有 active/deprecated 两态）
```

不变式：迁移后表中不存在 `status = 'active'` 的行（全部 renamed 为 `activated`）。
`DefinitionStatus` 枚举的 `Active` 变体更名为 `Activated`（serde `rename` 保持
线缆值 `activated`；`active` 作为兼容别名在读路径接受、写路径产出 `activated`）。

### 2. 版本单调性与不可变性

- 版本号继续解析自 definition id 的 `key:version` 后缀（逻辑不变）。
- **单调性校验**（publish 时）：同 `def_key` 下，新发布版本的 `version` 必须严格
  大于该 key 所有已达到 `Published`/`Activated`/`Deprecated` 的版本号的最大值。
  违反 → `definition_version_not_monotonic`（409）。
- **不可变性**：一旦版本达到 `Published`（含 `Activated`、`Deprecated`），其
  BPMN XML 与编译图不可修改。对已发布版本 id 重新 deploy →
  `definition_version_immutable`（409），不再静默覆盖（关闭现行
  `ON CONFLICT DO UPDATE SET bpmn_xml` 的覆盖路径）。
- 仅 `Draft` 状态的 id 可重新上传（覆盖 XML 与编译图，不触发单调性校验）。
- 快路径 `POST /deploy` 对已存在的 id：若该 id 当前为 `Draft` → 覆盖草稿后走完
  快路径；若已 `Published`+ → `definition_version_immutable`（409，要求新版本号）。

### 3. 同 key 并发发布与激活（CAS）

**至多一个 Activated per key**——激活是 CAS 操作：

- **Postgres**（N1-3 实现）：单事务内
  `UPDATE process_definition SET status='deprecated' WHERE def_key=$1 AND status='activated' AND id<>$2`
  再 `UPDATE process_definition SET status='activated' WHERE id=$2 AND status='published'`
  （`RETURNING` 验证行数；0 行 = 已被并发改变，返回当前状态）。参数绑定，符合
  Mimosa 约束。
- **Memory**：写锁内完成同一段 check-then-act（现行 `register` 已在写锁内做
  "翻转旧 active"，扩展为仅从 `Published` 跃迁到 `Activated`）。

并发场景预期（验收）：

| 场景 | 结果 |
|------|------|
| 同 key 两个不同版本并发 activate | CAS 串行化：先到者激活、另一者被翻转为 Deprecated；后到者的 `UPDATE ... WHERE status='published'` 命中 0 行 → 409 `definition_activation_conflict`（附当前激活版本 id） |
| 同版本并发 activate（重复请求） | 幂等：已 Activated → 200 回放当前状态 |
| 同 key 并发 publish 两个不同版本号 | 单调性校验串行化：版本号大的成功；版本号小的若不大于既有最大值 → 409 `definition_version_not_monotonic` |
| 同 id（同版本号）并发 publish | 第二个命中 `definition_version_immutable`（409） |

**多副本部署**（同 DB 多进程）的 CAS 安全性依赖 DB 行级锁（`FOR UPDATE`），但
v1 产品承诺单服务进程（ADR-008 §7 已记录）。多副本显式推迟。

### 4. 旧实例继续运行（不可变版本绑定）

- `ProcessInstance.process_def_id` 在 `ProcessStarted` 时记录带版本号的 id
  （现行行为，不变）。
- 实例加载定义时按 id 精确加载（`load(id)`），不取"当前 active"——即使该版本已被
  Deprecated，运行中实例仍加载其绑定的版本继续执行。
- **回退操作**（N1-4）：将一个 `Deprecated` 的 `Published` 版本重新 `activate`
  （CAS）。这不改写任何已运行实例的 `process_def_id`——旧实例继续在原版本上跑，
  新实例从重新激活的版本启动。"回退 = 重新激活旧版本，不改写已运行实例。"

### 5. 失败回滚预期

| 操作 | 失败点 | 回滚 |
|------|--------|------|
| 快路径 deploy | 编译失败 | 不持久化（现行行为，不变） |
| 快路径 deploy | 校验失败（Critical finding） | 不持久化（快路径要求校验通过才发布） |
| 快路径 deploy | publish 成功、activate 失败（CAS 冲突） | 版本留在 `Published`，未 `Activated`；调用方收到 409 + 当前激活版本；重试 activate（单独端点）即可 |
| 细粒度 publish | 单调性校验失败 | 不持久化版本记录；草稿保持原态 |
| 细粒度 activate | CAS 冲突 | 版本保持 `Published`，不翻转 |

**诚实边界**：快路径的 publish+activate 在 Postgres 单事务内原子（要么都成功，
要么都回滚）；在 Memory 单写锁内原子。细粒度 publish 与 activate 是两次独立操作，
不原子——这是细粒度 API 的固有取舍，N1-3 的 CAS 激活闭合"激活一步"的并发窗。

### 6. 部署快路径语义（兼容桥）

`POST /process-definitions/deploy`（原始 BPMN XML，`application/xml`）升级为：

1. `parse_and_compile`（编译失败 → 400 `Compile`，现行行为）
2. `validate_definition`（Critical finding → 400，新增校验门——现行 deploy 不校验，
   本 ADR 将快路径升级为"必须通过校验才发布"）
3. publish（单调性校验 + 不可变冻结；版本冲突 → 409）
4. activate（CAS；冲突 → 409，版本留在 Published）

返回值不变（`DeployResponse { process_definition_id }`）。

**兼容性取舍**：步骤 2 是行为变更——现行 deploy 不校验，快路径新增校验门。任何
能通过编译但含 Critical finding（如 `unreachable_node`）的定义，今天能 deploy、
升级后不能。这是有意的质量门提升；`POST /validate` 仍可用于无副作用的预检。
若需"不校验直接 deploy"的旧行为，保留一个 opt-in 标志（`?skip_validation=true`，
默认 false）——但 N1-1 只定义契约，该标志的端点行为在 N1-2 落地。

### 7. API 兼容矩阵

| 现行端点（E 系列） | N1-1 后语义 | 线缆变化 |
|---|---|---|
| `POST /process-definitions/deploy` | 快路径：编译→校验→发布→激活（原子） | 请求不变；新增 409 版本冲突码；`?skip_validation` opt-in（N1-2） |
| `GET /process-definitions` | 列出全部版本记录 | `status` 枚举增加 `draft/validated/published/activated`（`active` 兼容别名读入） |
| `GET /process-definitions/versions/:key` | 同 key 版本列表 | 同上 status 扩展 |
| `GET /process-definitions/active/:key` | 返回当前 Activated 版本 | 语义不变；status 值 `activated`（兼容 `active` 读入） |
| `PUT /process-definitions/:id/activate` | CAS 激活 | 新增 409 `definition_activation_conflict` |
| `PUT /process-definitions/:id/deprecate` | 退役（同现行） | 无变化 |
| `POST /process-definitions/validate` | 无状态校验（现行） | 无变化（N1-2 增加持久化校验端点） |
| `POST /process-definitions/dry-run` | 无状态模拟（现行） | 无变化 |

新增端点（N1-2/N1-3 落地，本 ADR 仅定义契约）：

| 新端点 | 用途 | 落地 Issue |
|---|---|---|
| `POST /process-definitions/draft` | 上传/覆盖草稿（不校验、不发布） | N1-2 |
| `POST /process-definitions/:id/validate` | 持久化校验，产出结构化修复报告 | N1-2 |
| `POST /process-definitions/:id/publish` | 发布（单调性 + 不可变冻结） | N1-2 |
| `POST /process-definitions/:id/retire` | 退役（→ Deprecated） | N1-4 |

OpenAPI `DefinitionVersionView.status` 枚举（`docs/openapi.yaml:2712`）从
`[active, deprecated]` 扩展为 `[draft, validated, published, activated, deprecated]`
（`active` 作为兼容读入别名，写路径产出 `activated`）。openapi_contract 一致性套件
同步更新（N1-2/N1-3 落地时）。

### 8. 可测样例（供 N1-2/N1-3 实现与验收）

本 ADR 锁定以下可测断言（实现后进 CI）：

1. **五态跃迁合法性**：Draft→Validated→Published→Activated 全程合法；
   Published→Draft 非法（`definition_invalid_transition`）。
2. **单调性**：同 key 已有 version=3 Published，publish version=2 →
   `definition_version_not_monotonic`；publish version=4 → 成功。
3. **不可变性**：对已 Published 的 `order-flow:3` 重新 deploy →
   `definition_version_immutable`；对 Draft 的 `order-flow:4` 重新上传 → 成功覆盖。
4. **CAS 激活**：同 key version=3 已 Activated，activate version=2 → version=3
   自动 Deprecated，version=2 成为 Activated；并发 activate version=4 与 version=5
   → 一胜一负（负者 409 + 胜者 id）。
5. **旧实例继续运行**：version=3 的实例运行中，activate version=4 → 实例仍加载
   version=3 定义继续执行；新实例启动用 version=4。
6. **快路径兼容**：`POST /deploy` 合法定义 → 编译+校验+发布+激活一步完成，返回
   definition id（与现行回包同构）。
7. **迁移**：0005 迁移后，表中无 `status='active'` 行；`GET /active/:key` 返回
   `status='activated'`。

## 非目标（明确不做）

- **不做租户级定义命名空间**（归 N1-3：`tenant_id` 列、跨租户枚举/读取/部署/启动
  拒绝、旧全局定义迁移策略）。
- **不做发布审计与可回退操作的面板**（归 N1-4：actor/版本/时间/内容摘要/原因的
  审计记录、并发/重启集成测试）。
- **不做 DSL deploy 端点**（ADR-006 line 152 的 open TODO 仍为未实现；DSL 仍经
  validate/dry-run 预检，deploy 只收 BPMN XML）。
- **不做草稿的版本号分配**——草稿用作者声明的 id（含版本号），但版本号在 publish
  时才校验单调性。草稿阶段允许任意版本号的 id（便于 Agent 迭代）。
- **不做已运行实例的版本迁移**——实例绑定不可变版本，不"升级到新版本"。
- **不做多副本分布式 CAS**（v1 单服务进程承诺，ADR-008 §7 已记录）。
- **不改变 `ProcessDefinition`（core）结构**——不加版本/状态字段；版本与状态是
  存储层 `ProcessDefinitionRecord` 的属性，core 的 `ProcessDefinition` 仍是纯图。

## 后续动作

- **N1-2**（Issue #59）：实现草稿端点、持久化校验报告（结构化修复建议、风险级别、
  所需权限、模拟分支覆盖）、publish 端点；扩展 `DefinitionStatus` 枚举与 DB 迁移
  0005；同步 openapi.yaml status 枚举与新增端点。
- **N1-3**（Issue #60）：在 N1-2 的状态机上叠加 `tenant_id` 列与命名空间隔离、
  CAS 激活的事务实现（Postgres `FOR UPDATE` + 参数绑定）、跨租户拒绝。
- **N1-4**（Issue #61）：发布审计记录（actor/版本/时间/内容摘要/原因）、retire
  端点、重新激活旧版本的回退操作、并发/重启集成测试。
- `deploy/schema.sql`：同步 0005 的 status 值 rename（`active`→`activated`）。
- `docs/database-schema.md`：补 `process_definition` 表的 status 枚举说明
  （现行文档无该表专节）。
