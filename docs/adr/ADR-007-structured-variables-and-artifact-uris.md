# ADR-007: 结构化变量与产物 URI 契约

## 决策信息

- **编号**: ADR-007
- **标题**: 结构化变量值与外部产物 URI 契约（版本化信封、大小上限、敏感字段、历史脱敏）
- **状态**: Accepted
- **日期**: 2026-10-28
- **Owner**: tech-lead
- **关联**: Roadmap E2-3（Issue #13）、ADR-006（最小声明式 DSL，平行分支 E2-1 已占用 006 编号，本 ADR 固定为 007）

## 背景与约束

E2 阶段面向 AI Agent 与外部 worker 开放变量通道后，三类问题必须解决：

1. **结构化值**：变量存储形态是 `HashMap<String, String>`（实例、事件 payload、外部任务变量、适配器、DB 列全部如此）。Agent 与 worker 需要传递 JSON 对象，而不是把 JSON 当不透明字符串手工拼接。
2. **大文本**：LLM 产物（长报告、代码块）若直接写入事件历史，历史表会膨胀且审计/回放负载不可控。大负载应存放外部对象存储，引擎只持有引用。
3. **敏感字段**：明文敏感值会随事件历史与日志落盘，泄漏面过大。

硬性约束：

1. **存储/线路表示不变**：`HashMap<String, String>`、既有 DB 列（TEXT）、既有事件 payload 结构全部保持；不允许 core 结构体改造或 SQL 迁移（变量契约是"值格式"问题，不是"schema"问题）。
2. `crates/core` 保持纯逻辑（无 I/O、无 async），契约的解析/校验/脱敏是纯函数。
3. 旧字符串变量永久有效：网关条件与 EL 只看到纯字符串或不透明信封字符串，语义不变。
4. 依赖 E1-2（配置与部署契约已就绪：env/TOML/log_effective 模式）。

## 备选方案

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 自描述版本化信封（`__var:1:<compact JSON>`）叠加在现有字符串之上** | 零 schema 变更、零迁移；读写方渐进采用；信封对不认识它的旧代码只是一个普通字符串 | 前缀碰撞需保留 `__var:`/`__encrypted:`/`__truncated:`/`__opaque:` 的 `__` 命名空间 |
| B: 变量表 + 类型列（结构化 schema） | 强类型、可索引 | 违反约束 1：需要 core 结构体与 SQL 迁移，所有适配器/事件 payload 连锁改造 |
| C: 约定 JSON 字符串（无前缀，尝试解析即信封） | 无前缀 | 无法区分"恰好是 JSON 的普通字符串"与结构化值；网关条件等纯字符串语义被破坏 |

**采用方案 A。**

## 决策结果

### 1. 版本化信封（`crates/core/src/variable_contract.rs`）

- 信封存储形态：`__var:1:<compact JSON>`。JSON 两种：
  - `{"t":"json","val":<any JSON>}`（含 null/标量/容器）
  - `{"t":"artifact","uri":"...","size":<u64 可选>,"sha256":"<64 hex 可选>","media_type":"<可选>"}`
- 读取 API：`parse_envelope(&str) -> Option<Result<VariableEnvelope, VariableContractError>>`
  —— `None` 表示非信封（纯字符串或 `__encrypted:` 密文）；未来版本（如 `__var:2:`）返回 `UnsupportedVersion`，历史与输出必须按不透明处理（脱敏，绝不解释）。
- v1 读取对未知字段宽容（前向兼容），序列化字段序固定 `t, val, uri, size, sha256, media_type`。
- 错误类型化并带稳定 code：`variable_envelope_unsupported_version`、`variable_invalid_envelope`、`variable_invalid_artifact_uri`。

### 2. 产物 URI 规则（`validate_artifact_uri`）

- 绝对 URI（scheme 非空且语法合法：`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`）
- 禁 userinfo（authority 中的 `@`）、禁 fragment（任何 `#`）
- 总长度 ≤ 2048 字节
- `size` ≥ 0（u64 类型级保证，负值在信封解析期即失败）
- `sha256` 出现时必须恰为 64 个 hex 字符
- **引擎永不解引用任何 URI**：解析归 worker/客户端所有
- `artifact://` scheme 为未来内建对象存储**保留**：合法但不透明（本轮不实现存储本体）

### 3. 大小上限：REST 写边界强制

- 配置：`engine.variable_max_bytes`（默认 65_536，env `BPM_ENGINE__VARIABLE_MAX_BYTES`）与 `engine.history_variable_max_bytes`（默认 1024，env `BPM_ENGINE__HISTORY_VARIABLE_MAX_BYTES`），TOML `[engine]`，纳入 `log_effective`。
- 三个写边界强制（start-instance、user-task complete、external-task complete）：
  - 每个变量值（**最终存储字符串**，含信封）≤ `variable_max_bytes`，否则 4xx `variable_too_large`（错误体含变量名、上限与提示"store large payloads externally and reference them via an artifact envelope"）。start 边界在加密后复查一次——密文膨胀可能超过上限。
  - 信封必须解析合法，否则 4xx `variable_invalid_envelope`（含变量名+原因；不支持版本同样归入该 code，细节留在 message）
  - artifact 信封必须通过 URI 校验，否则 4xx `variable_invalid_artifact_uri`
- 错误体沿用 `ErrorResponse` 形态，新增可选 `code` 字段（None 时序列化时省略，向后兼容）。

### 4. 敏感字段 ⇒ 必须加密

- `StartInstanceRequest` 新增 `sensitive: Option<Vec<String>>`：每个敏感名必须 (a) 存在于提交变量中且 (b) 列在 `encrypted` 中，否则 4xx `sensitive_requires_encryption`。
- **理由**：加密值落盘即密文（既有 `__encrypted:` 格式本就不透明），因此事件历史与日志永不包含明文——这是结构上保证，不靠每个日志点自觉。GET 解密行为不变（持钥的 API 调用方可读；展示掩码是 UI 关注点，推迟到 E6）。
- 与信封组合：加密作用于最终字符串（信封可以整体被加密）。

### 5. 大文本不进事件历史（默认开启的脱敏）

- 核心纯函数 `redact_history_variable_value(value, policy)`：
  - 长度 > `history_variable_max_bytes` → `__truncated:<orig_len>:<sha256-first-12-hex>:`
  - 未知信封版本 → `__opaque:`
  - 其余（纯字符串、合法信封、`__encrypted:` 密文）→ 原样
- 运行时（`history_handler`）对序列化 payload 后处理，**只重写**已知的变量承载字段（`variables`、`initial_variables`，递归查找这两键但不碰其它字段）。
- 策略来自 `EngineContext`（新 builder 字段 `.history_redaction_policy(...)`，缺省即 1024 上限）——**脱敏默认开启**，小值不受影响。Server 从配置构建。
- 判定顺序：先长度后版本（超长的未知版本值得到更紧凑的 `__truncated:` 标记）。

### 6. 旧数据与历史兼容

- 纯字符串在所有路径（条件求值、比较、适配器）行为不变；旧 `__encrypted:` 值不动。
- **既有历史事件不回写**：脱敏只作用于新写入。旧历史中的大值保持原样（回写违反 append-only 审计语义）。
- 实例变量读取方看到信封字符串原样返回；fetch-and-lock 把信封字符串原样交付 worker——worker 按 `docs/variables-and-artifacts.md` 契约自行解析。
- 信封值经过网关条件/EL 时只是不透明字符串：条件应使用纯字符串变量（或先在客户端解包）。

## 非目标（明确不做）

- **不内建对象存储 adapter**（`artifact://` 仅保留 scheme；产物存取归调用方/未来迭代）
- 不做每变量元数据存储（无 variables 表、无索引）
- 展示掩码（UI 层脱敏显示）推迟到 E6
- 不回写/清洗既有历史数据

## 后续动作

- `docs/variables-and-artifacts.md` 作为 Agent/worker 的契约参考（信封格式、上限、错误码表），SDK 文档引用之
- worker-sdk 文档与示例按契约演示信封解析（后续 PR）
- 未来引入对象存储时新增 `__var:` 新版本或 `artifact://` 解引用器，另行 ADR
