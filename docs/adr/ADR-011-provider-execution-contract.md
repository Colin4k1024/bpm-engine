# ADR-011: 真实 AI Provider/工具执行契约（预算、超时、取消、重试分类、幂等、敏感数据）

## 决策信息

- **编号**: ADR-011
- **标题**: 真实模型/工具调用的每步预算、双层超时、取消传播、重试分类细化、幂等作用域与敏感数据策略；锁定"外部模型/工具至少一次调用，token 至多一次完成"不变式
- **状态**: Accepted
- **日期**: 2026-11-02
- **Owner**: tech-lead
- **关联**: Roadmap N3-1（Issue #66）、依赖 N3-2（Issue #67，结构化输出验证与受控路由）、N3-3（Issue #68，预算与故障保护）、N3-4（Issue #69，生产 worker 模板）；ADR-008（审批门、暂停/取消、`__approval` 引擎真值）、ADR-007（变量/artifact 契约）、`docs/ai-task-envelope.md`（E3-1 信封）、`docs/tool-security.md`（E3-3 工具边界）、`docs/worker-context.md`（E3-2 worker 执行上下文）

## 背景与约束

### 现行 AI 步骤执行模型

AI 步骤是 `ExternalTask` 节点（`crates/core/src/node.rs:92-100`：
`{ task_type, retries: i32, timeout_secs: u64 }`）。token 到达时
（`token_arrived_handler.rs:142-256`）引擎创建 `ExternalTask` 行（Ready），经
`fetch_and_lock`（`external_task_store.rs:112-118`，Postgres `FOR UPDATE SKIP LOCKED`
原子加锁）交付 worker。

worker（`crates/worker-sdk/src/worker.rs`）poll → fetch → 在独立 `tokio::spawn` 中
执行 `TaskHandler::handle`（`worker.rs:456`；trait 定义 `handler.rs:235-240`）→
`complete`/`fail`。
`TaskResult`（`types.rs:26-43`）：`Complete { variables }` 或
`Fail { error, retry_after, category: Option<RetryCategory> }`。

### 现行的预算、超时、取消、幂等（均不完整）

**预算**：**无任何预算执行**。`TaskCost`（`crates/core/src/ai_task.rs:226-245`：
`currency: Option<String>` + `amount_micros/tokens_in/tokens_out/duration_ms` 全
`Option<u64>`）由 worker 报告，经信封 → history → `/steps` trace 端到端**报告但从不
检查**。无 `spend_limit`/`max_spend`/`cost_limit`/`budget`。`max_tokens: Option<u32>`
在 `ModelRequest`（`provider.rs:66`）上是输出 token 上限**提示**（mock 夹住报告数，
`provider.rs:271-274`），非预算、非引擎强制。

**超时**：双层但未对齐。
- 引擎侧 `timeout_secs`（节点定义）= **租约时长**（`lock_expire_at = now +
  timeout_secs`，`external_task_store.rs:57`）。租约过期 → 任务回收 Ready → 重投
  （at-least-once）。这是引擎的唯一超时——租约边界，非执行 wall-clock。
- worker 侧 `with_timeout`（`types.rs:90-102`）= 策略性 wall-clock 执行超时，返回
  `TaskResult::Fail { error: "task_timeout: ...", category: None }`（transient）。
- 信封 `timeout_secs`（`ai_task.rs:312`）= **"Informational in v1"**（envelope doc §2），
  不被引擎或 worker 读取为执行超时。

**取消**：`CancellationToken`（`handler.rs:31-37`）是 worker-run 作用域（Ctrl-C/
SIGTERM/shutdown）。`drain()`（`worker.rs:325-348`）停止 poll → 信号 in-flight → 等
`shutdown_grace` → abort 残留。**未传播到 `ModelProvider::generate`**——挂起的
provider 调用仅由 worker 的 `with_timeout` 与租约过期约束。实例取消（ADR-008 §6）
拒绝 complete/fail（409 `instance_cancelled`），但不中断飞行中的模型调用。

**重试分类**：仅 `RetryCategory::{Transient, Permanent}`（`ai_task.rs:153-168`）。
`Permanent` → 跳过重试（Failed + DLQ + TokenFailed）；`Transient`（或缺省）→
减计数重投。`fail` 的 `retry_after: Option<Duration>`（`external_task_store.rs:154`）
是**单次可选延迟**（可见性延迟，非指数退避）。无 rate-limit vs timeout vs bad-input
区分。

**幂等**：**引擎不强制幂等**。引擎交付 at-least-once（租约过期⇒重投）。worker 侧
`IdempotencyGuard`（`idempotency.rs:44-124`）= 进程内、有界（默认 10000 键、FIFO
驱逐）、`check_and_mark`、失败 `unmark`，**不跨重启、不跨进程**。`derive_key =
"<task_id>:sha256:<input>"`（:133-144）。信封 `idempotency_key`（:300）= **仅审计**
（记入 history，不强制；envelope doc §4）。

**结构化输出验证**：**无**。`input_schema`/`output_schema`（`ai_task.rs:336,340`）=
`Option<serde_json::Value>`，`validate()`（:441-452）仅校验是 JSON 对象，**不解释
内容**。模型输出作为**纯字符串变量**写入（如 `verdict = "approved"`），网关按
`EdgeCondition::VariableEq`（`node.rs:52-65`）或 EL（`el.rs:13-60`）比较纯字符串。
**无 schema 校验模型输出是否符合预期枚举/结构**——off-schema 输出落入 Default 边。

**工具安全**（E3-3，已实现）：allowlist（deny by default，`tool_not_allowed`）、
审批门（`__approval`，`tool_requires_approval`）、env-only 凭据
（`CredentialSource::Env`，`credential_missing`）、输出限制+脱敏
（`OutputLimits.max_bytes` = 65536，`redact_history_variable_value` 标记）。

**provider 适配**（`crates/ai-reference`）：`ModelProvider::generate(ModelRequest) ->
ModelResponse`（`provider.rs:114-118`）。`MockModelProvider`（离线确定性，CI 默认）、
`UnconfiguredProvider`（编译期 stub，`provider_unconfigured`）、`OpenAiCompatProvider`
（feature `http`，默认 off；env 配置读一次 `OPENAI_BASE_URL`/`OPENAI_API_KEY`/
`OPENAI_MODEL`，无轮换）。

硬性约束：

1. `crates/core` 保持纯逻辑。预算执行、超时包装、取消传播是 worker 侧策略，
   不进 core。`RetryCategory` 枚举可扩展（在 core，纯数据）。
2. at-least-once 交付语义不变（租约过期⇒重投是引擎的崩溃恢复基石）。
3. 现有 `ExternalTask` 节点的 `retries`/`timeout_secs` 字段与 `fail` 契约不破坏
   （E 系列套件绿色）。
4. `docs/ai-task-envelope.md` 的信封 schema_version=1 不变（加性可选字段，不升版本）。
5. Mimosa 约束：无新 SQL（本 ADR 无存储变更）。

## 备选方案

### 预算执行的位置

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: worker 侧执行（引擎记录声明，不强制）** | core 保持纯逻辑；预算是调用方策略；worker 有 cost 信息 | 依赖 worker 正确实现；恶意/错误 worker 可超预算 |
| B: 引擎侧执行（complete 时检查累计 cost） | 引擎强制 | core 需读 cost 并比较（违反纯逻辑）；跨重试的累计 cost 需存储；complete 时检查无法阻止"已花超" |

**采用方案 A。** 预算声明放节点定义（N3-3 加可选字段）或经任务变量传入；worker 在
每次 model 调用前检查累计 cost，超预算 → `Fail(Permanent, "budget_exceeded")`。
引擎记录预算声明与 cost 报告（信封），不强制。诚实边界：一个错误 worker 可超预算
调用——预算是 best-effort 防护，不是硬保证（与 at-least-once 一致：引擎不限制
worker 行为，只保证 token 语义）。

### 重试分类的粒度

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 保持 Transient/Permanent 二态 + 错误码前缀指导** | 枚举不膨胀（信封兼容）；错误码已是稳定字符串；N3-2 可按码分类 | 精细分类靠字符串约定，非类型安全 |
| B: 扩展枚举为 Transient/Permanent/RateLimited/BadInput/BudgetExceeded | 类型安全 | 信封 schema 变更（升 version 或破坏 deny_unknown_fields）；涟及 worker SDK/REST/测试 |

**采用方案 A。** `RetryCategory` 保持二态。定义错误码前缀 → 分类映射（§5），N3-2/N3-3
按码实现精细路由（如 `rate_limited` → Transient + 指数退避；`output_validation_failed`
→ Permanent）。错误码是字符串前缀（`<code>: <message>`，现行格式），不破坏信封
schema。

## 决策结果

### 1. 核心不变式：至少一次调用，token 至多一次完成

| 维度 | 保证方 | 机制 |
|------|--------|------|
| 模型/工具调用 | **at-least-once**（引擎） | 租约过期⇒任务回收 Ready⇒重投⇒worker 可能再次调用模型 |
| token 完成 | **at-most-once**（引擎） | `ExternalTaskCompleted`/`TokenFailed` 驱动 token 跃迁，CAS 保证 token 到达终态恰好一次（ADR invariants） |
| 副作用幂等 | **worker 责任** | worker 用 `execution_id`（task_id:attempt）+ `idempotency_key` 去重模型调用与工具副作用 |

**窗口分析**（诚实记录）：
1. worker 调用模型 → 得到结果 → **崩溃在 complete 之前**。租约过期 → 任务重投 →
   另一 worker 再次调用模型。模型被调用 ≥2 次，token 完成 1 次。worker 的
   `idempotency_key`（同 task + 同 input → 同 key）使第二次调用命中缓存（若
   durable store）——若无 durable store，模型被重复调用，副作用幂等是工具作者
   责任（`tool-security.md` §8）。
2. worker 调用工具（有副作用）→ 工具执行成功 → 崩溃在 complete 之前。工具副作用
   已发生。重投后 worker 再次执行——**工具副作用必须幂等**（工具作者责任，引擎
   不回滚副作用，ADR invariants：补偿归 N4）。
3. worker complete → 引擎处理 `ExternalTaskCompleted` → token 跃迁。若引擎崩溃在
   token 跃迁后、ack 之前 → worker 重试 complete → 引擎的 `complete` 检查 state：
   已 Completed（state != Locked）→ `TaskNotLocked` → REST 400 `task_not_locked`
   （`routes.rs:450`）。重复 complete 被拒绝，token 不二次跃迁——at-most-once 由
   条件 UPDATE（`WHERE state='Locked'`）保证。

**不变式表述**：外部模型/工具调用至少一次（at-least-once）；token 到达终态至多一次
（at-most-once，CAS 保证）；副作用幂等是 worker/工具作者责任，引擎不保证
exactly-once 调用。这是"可信的长时运行执行层"与"不确定性模型调用"的边界。

### 2. 每步预算（worker 侧执行）

预算声明（节点定义加可选字段，N3-3 落地；或经任务变量传入）：

```
Budget {
    max_cost_micros: Option<u64>,    // 累计成本上限（与 TaskCost.currency 对齐）
    max_total_tokens: Option<u64>,   // 累计 input+output token 上限
    max_attempts: Option<u32>,       // 最大尝试次数（独立于 retries 倒计数）
}
```

worker 执行流程（N3-3 实现）：
1. 每次 `ModelProvider::generate` 前：累计 cost（从历史 attempt 的 `TaskCost` +
   当前预估）与 `max_cost_micros`/`max_total_tokens` 比较；超限 → 不调用模型 →
   `Fail(Permanent, "budget_exceeded: spent=<X> limit=<Y>")`。
2. 每次 `Tool::run` 前：同上检查（工具调用也消耗预算）。
3. `max_attempts`：独立于引擎的 `retries` 倒计数。worker 跟踪 attempt 计数
   （`execution_id` 的 attempt 部分），超限 → `Fail(Permanent, "max_attempts_exceeded")`。

引擎记录预算声明（写入任务变量或节点定义）与 cost 报告（信封 `TaskCost`），不强制。
`/steps` trace 展示预算与累计 cost（N6-1 观测）。

**诚实边界**：预算是 best-effort——worker 在调用前检查，但"检查后、调用中"的 cost
（如流式 token）可能超限。超限在下次检查时发现并 Fail。不回滚已发生的调用。

### 3. 双层超时

| 层 | 归属 | 字段 | 机制 | 超时后 |
|----|------|------|------|--------|
| **租约超时** | 引擎 | 节点 `timeout_secs` | `lock_expire_at = now + timeout_secs`；`reclaim_expired_locks` 回收 | 任务 Ready → 重投（at-least-once） |
| **执行超时** | worker | 信封 `timeout_secs`（升级为执行超时提示）或 worker 默认 | `with_timeout(generate, execution_timeout)` | `Fail(Transient, "execution_timeout: ...")` → 减计数重投 |

**信封 `timeout_secs` 升级**：从"Informational in v1"升级为**执行超时提示**——worker
SHOULD 用它作为 `with_timeout` 值。若 absent，worker 用默认（如 120s）。envelope doc
§2 的"Informational"标注由本 ADR 更新为"执行超时提示，worker 侧强制"。

**租约与执行超时的关系**：执行超时 < 租约超时（否则租约先过期，worker 被 reclaim）。
约定：`execution_timeout = min(envelope.timeout_secs, timeout_secs * 0.8)`。worker
在执行超时前 Fail(transient) + `retry_after`（重投），优于租约过期被动回收（后者
无法记录失败原因）。`extend_lock` 用于长时间任务主动续租（现行
`run_with_renewal`，`handler.rs:145-232`）。

### 4. 取消传播

**worker 侧**（N3-3 实现）：`ModelProvider::generate` 调用用 `tokio::select!` 包裹
`ctx.cancelled()`：
- 取消信号到达 → 中断 `generate` → `Fail(Transient, "cancelled")`（或 drain 场景下
  no-op：不 fail、不消耗 retry，租约过期被动回收，`worker.rs:516-528` 现行行为）。
- 正常完成 → 返回结果。

**`ModelProvider` trait 演进**（N3-3，可选）：`generate` 签名增加
`&CancellationToken` 参数，或在 worker 侧 `select!` 包裹（不改 trait）。本 ADR 不强制
改 trait——`select!` 包裹是最低侵入实现。

**实例取消**（ADR-008 §6）：实例 Terminated 后，in-flight 任务的 complete/fail 被
409 `instance_cancelled` 拒绝。worker 的租约过期后任务回收，但实例已终态 → 任务
不重投（或重投后 complete 被 409 拒绝，工作丢弃）。worker 的 `CancellationToken`
在 drain 时中断模型调用，避免无意义的长时间调用。

### 5. 重试分类细化（错误码前缀 → 分类映射）

`RetryCategory` 保持二态 `{Transient, Permanent}`（信封 schema 不变）。定义错误码
前缀 → 分类映射（N3-2/N3-3 实现）：

| 错误码前缀 | 分类 | 退避 | 说明 |
|---|---|---|---|
| `execution_timeout` | Transient | 指数退避 `min(base * 2^attempt, cap)` | 执行超时，可重试 |
| `rate_limited` | Transient | `retry_after` 从 429/RATE_LIMIT 头提取 | provider 限流，可重试 |
| `provider_error` | Transient | 指数退避 | provider 5xx/网络错误 |
| `bad_input` | Permanent | — | 输入格式错误，不可重试 |
| `output_validation_failed` | Permanent | — | 模型输出不符合 schema（N3-2） |
| `budget_exceeded` | Permanent | — | 超预算 |
| `max_attempts_exceeded` | Permanent | — | 超最大尝试次数 |
| `tool_not_allowed` | Permanent | — | 工具不在 allowlist |
| `tool_requires_approval` | Permanent | — | 审批门未通过 |
| `credential_missing` | Permanent | — | 凭据配置缺失 |
| `tool_execution_failed` | Transient（现行） | 可配 | 工具执行失败，默认 transient |
| `cancelled` | Transient | — | 取消，重投或实例终态 |
| `handler_panic` | Transient（现行） | — | worker panic 隔离 |

**退避策略**（N3-3）：`retry_after = min(base * 2^attempt, cap)`，默认
`base=1s, cap=60s`（worker 配置）。worker 在 `Fail` 时计算并传入 `retry_after`。
引擎存储为 `lock_expire_at = now + retry_after`（可见性延迟，现行机制
`external_task_store.rs:441-463`）。

### 6. 幂等作用域

| 作用域 | 键 | 存活 | 强制方 |
|--------|----|------|--------|
| 执行尝试 | `execution_id` = `<task_id>:<attempt>` | 单次 attempt | worker（模型调用去重） |
| 任务+输入 | `idempotency_key` = `derive_key(task_id, input)` 或调用方提供 | worker 决定（进程内 or durable） | worker |
| 审计 | 信封 `idempotency_key` | 持久（history） | 引擎记录，不强制 |

**worker 的幂等责任**（验收核心）：
- 同 `execution_id` 的重复调用（同 attempt 内的 select! race）→ 模型结果缓存，不
  重复调用。
- 跨 attempt 的重复调用（租约过期重投，新 attempt）→ `idempotency_key` 相同
  （同 task + 同 input）→ 若 worker 有 durable store，命中缓存；若无，模型被再次
  调用（at-least-once），副作用幂等是工具作者责任。
- **durable 幂等存储推荐**（N3-4 模板）：worker 用自有持久存储（如 Redis/SQLite）
  存 `idempotency_key → 结果`，跨重启命中。进程内 `IdempotencyGuard`（现行）仅适用
  于无副作用的只读模型调用。

引擎的 `idempotency_key`（信封字段）= **仅审计**（envelope doc §4 不变）。引擎不
查重、不缓存模型结果——那是 worker 的调用侧职责。

### 7. 敏感数据策略

| 数据 | 模型输入 | history | /steps trace | 信封 |
|------|:--------:|:-------:|:------------:|:----:|
| 任务变量（含敏感标记） | ✓ 全量 | ✗ 脱敏（`HistoryRedactionPolicy`） | ✗ 不展示变量值 | ✗ 不含变量值 |
| 模型输出（纯字符串） | — | ✗ 脱敏（size-capped + redaction） | ✗ 不展示 | ✓（经变量通道，同样脱敏） |
| 凭据（env-only） | ✗ **绝不**进 prompt | ✗ | ✗ | ✗ |
| prompt 内容 | — | ✗ 脱敏 | ✗ 仅摘要/引用 | ✗ |
| cost / model / tools | — | ✓ | ✓（`envelope_summary`） | ✓ |
| artifact URI + sha256 | — | ✓ | ✓ | ✓ |
| input_schema / output_schema | — | ✓ | ✗（`envelope_summary` 显式省略） | ✓（metadata） |
| idempotency_key | — | ✓ | ✗（显式省略） | ✓ |

**红线**：
1. **凭据绝不进 prompt/变量/history**。`CredentialSource::Env`（`tools.rs:584-594`）
   是唯一凭据来源；worker 注入到 `ToolContext`，不注入 `ModelRequest.prompt`。
   `ToolContext` Debug（`tools.rs:394-411`）只打印凭据**键名**，永不值。
2. **模型输入的全量 vs history 的脱敏**：模型 provider 收到全量输入（需要完整
   上下文推理）；history 只记录脱敏摘要（`HistoryRedactionPolicy`，
   `variables-and-artifacts.md` §6）。这是"模型需要看 / 历史不需要记"的边界。
3. **大输出用 artifact URI**：模型大输出（如长文本/JSON）经 artifact URI 引用
   （`ArtifactRef { uri, sha256, size, media_type }`，`ai_task.rs:200-215`），变量
   只存引用或摘要。引擎不解引用 URI（`ai_task.rs:26-28`）。
4. **`envelope_summary` 观测边界**（`step_trace.rs:645-684`）：展示 execution_id、
   retry_category、model、tools、cost、artifacts；**不展示** input_schema/
   output_schema/idempotency_key/任何变量值。这是"可解释但不泄密"的观测契约。

### 8. 模型版本契约

- `ModelRef { provider, name, version }`（`ai_task.rs:171-181`）由 worker 在信封中
  声明（实际使用的模型）。引擎记录用于审计/观测（`/steps` 展示 model）。
- 引擎**不校验**模型版本 allowlist（worker/deployer 责任）。N3-2 可在节点定义中
  声明期望 `ModelRef`，worker 自检不匹配时 `Fail(Permanent, "model_version_mismatch")`
  （worker 侧策略，非引擎强制）。
- `OpenAiCompatProvider` 配置读一次（`from_env`，`openai.rs:74-82`），`OPENAI_MODEL`
  默认 `gpt-4o-mini`。模型版本在 `ModelRef` 中由 worker 报告实际使用值。

### 9. 可测样例（供 N3-2/N3-3/N3-4 实现与验收）

1. **至少一次调用**：worker 调用模型后崩溃（未 complete）→ 租约过期 → 重投 →
   模型被调用 ≥2 次；token 最终 Completed 一次。
2. **至多一次完成**：并发双 worker complete 同任务 → 一者 200（ Locked→Completed，
   token 跃迁）、一者 400 `task_not_locked`（state 已非 Locked，条件 UPDATE 命中 0 行）；
   token 跃迁恰好一次。
3. **预算超限**：累计 cost 超 `max_cost_micros` → worker 不调用模型 →
   `Fail(Permanent, "budget_exceeded")` → Failed + DLQ。
4. **执行超时**：`with_timeout` 触发 → `Fail(Transient, "execution_timeout")` →
   减计数重投 + `retry_after` 退避。
5. **取消传播**：drain 信号 → `generate` 中断 → 任务不 fail（drain no-op）或
   `Fail(Transient, "cancelled")`；租约过期被动回收。
6. **重试分类**：`rate_limited` → Transient + `retry_after` 从 429 头提取；
   `output_validation_failed` → Permanent（N3-2）。
7. **幂等**：同 `execution_id` 重复调用 → 缓存命中不重复；跨 attempt 同
   `idempotency_key` → durable store 命中（N3-4 模板）。
8. **敏感数据**：凭据不在 history/prompt；`envelope_summary` 不含变量值/schema；
   敏感变量在 history 脱敏。
9. **工具副作用幂等**：工具执行后崩溃 → 重投 → 工具再次执行 → 副作用幂等
   （工具作者验证，N3-4 模板示范）。

## 非目标（明确不做）

- **不做引擎侧预算强制**（core 纯逻辑；预算是 worker 策略，best-effort）。
- **不做 exactly-once 模型调用**（at-least-once 是引擎保证；exactly-once 需要
  durable 幂等存储，是 worker 责任，引擎不提供）。
- **不做副作用回滚**（工具副作用已发生不回滚；补偿归 N4 补偿状态机）。
- **不升信封 schema_version**（加性可选字段，不升 version；`deny_unknown_fields`
  下新增字段需 v2——本 ADR 不新增信封字段，预算声明在节点定义/任务变量）。
- **不做凭据轮换/vault 集成**（env-only 不变；`tool-security.md` §10 已记录为
  out-of-scope）。
- **不做流式 token 实时预算截断**（预算在调用前后检查，流式中途超限在下次检查
  收敛）。
- **不做 provider trait 签名强制变更**（取消传播用 `select!` 包裹，不改
  `ModelProvider::generate` 签名；若 N3-3 选择加 `CancellationToken` 参数是可选演进）。
- **不做模型输出 schema 强制校验在引擎侧**（N3-2 在 worker 侧校验；引擎不解释
  schema 内容，`ai_task.rs:26-28` 不变）。

## 后续动作

- **N3-2**（Issue #67）：worker 侧结构化输出验证（`output_schema` JSON Schema 校验
  模型输出）；不合格 → `output_validation_failed`（Permanent）；仅已声明字段映射到
  流程变量与网关；默认拒绝模型自选工具名/参数越权。
- **N3-3**（Issue #68）：预算执行（`Budget` 字段 + worker 检查）、退避策略
  （指数退避计算）、取消传播（`select!` 包裹）、错误码前缀 → 分类映射实现；
  mock 故障注入 + 可选真实 provider 集成测试。
- **N3-4**（Issue #69）：生产 worker 模板——审批、结构化输出、工具 allowlist、
  幂等键、artifact 引用串成单模板；durable 幂等存储示范；空环境文档命令 CI 执行。
- `docs/ai-task-envelope.md` §2：`timeout_secs` 从"Informational"更新为"执行超时提示，
  worker 侧强制"。
- `docs/worker-context.md`：补充预算、取消传播、退避策略、错误码分类映射。
- `docs/tool-security.md` §8：工具副作用幂等责任的本 ADR §1/§6 交叉引用。
