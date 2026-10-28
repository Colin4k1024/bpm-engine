# ADR-006: 最小声明式 DSL 的 JSON Schema 与编译器

## 决策信息

- **编号**: ADR-006
- **标题**: 最小声明式 DSL 的 JSON Schema 与编译器
- **状态**: Accepted
- **日期**: 2026-10-28
- **Owner**: tech-lead
- **关联**: Roadmap E2-1（Issue #11）、ADR-004（Timer Scheduler）

## 背景与约束

- Roadmap E2 阶段要求 BPM 引擎面向 AI Agent 开放：Agent 生成流程定义时应直接产出
  结构化 JSON，而不是手写 BPMN 2.0 XML（命名空间、extensionElements、
  conditionExpression 文本协议都容易生成出错）。
- 生成的定义经常是非法的（悬空引用、缺字段、类型错误）。入口层必须返回
  **稳定的、字段级**的诊断（机器可读错误码 + JSON Pointer 路径），否则 Agent 无法
  自我纠错。
- 硬性约束：
  1. BPMN XML 入口继续可用，两个入口**共享编译后模型与同一套语义校验**
     （Issue #11 验收标准），不允许出现第二套执行语义。
  2. `crates/bpmn` 编译器已有全量收集（非 fail-fast）的错误协议
     （`CompilerError { code, message, node_id, flow_id, hint }`），DSL 不能绕过它。
  3. `crates/core` 是纯逻辑 crate，不得为 DSL 引入 I/O 或 serde 之外的耦合。
  4. 平行分支正在新增 ADR-005（前向迁移），本 ADR 编号固定为 ADR-006。

## 备选方案

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: DSL JSON → 降低（lower）到 `BpmnProcess` 中间模型 → 复用 `bpm_engine_bpmn::compile`** | 单一编译/校验路径；与 BPMN 入口产物天然逐字段一致 | 需要 lower 层严格对齐 BPMN 解析器的默认值 |
| B: DSL 直接构造 `ProcessDefinition` | 少一层转换 | 出现第二条校验/语义路径，必然与 BPMN 语义漂移；违反验收标准 |
| C: DSL 序列化为 BPMN XML 文本再走 XML 解析 | 零新代码 | 错误定位退化为 XML 行列号，丢失 JSON 路径；双重语法、双重转义，Agent 更难生成 |

**采用方案 A。**

## 决策结果

### 1. DSL v1 是受限子集

节点类型仅允许：`start`、`end`、`serviceTask`、`userTask`、`exclusiveGateway`、
`parallelGateway`、`timerIntermediateCatch`。边（edges）支持可选条件
（`variableEq` / `expression`）与 `default` 标记。

以下能力**明确排除**在 v1 之外（未来版本可通过 `schema_version` 门控逐步加入）：
boundary events、call activities、subProcess、message/signal 事件、terminate end。

### 2. 单一语义路径

`bpm-engine-dsl::compile(json)` 的内部流程：

```
JSON 文本 --serde(+serde_path_to_error)--> DslDocument（结构校验）
         --lower--> BpmnProcess + wire_flows
         --bpm_engine_bpmn::compile（原样复用）--> ProcessDefinition
```

lower 层必须与 BPMN 解析器**逐字段一致**（含默认值），保证"等价的 DSL 文档与
BPMN 文档编译出结构完全相同的 `ProcessDefinition`"（以单元测试固化）：

| DSL 字段 | BPMN 解析器行为 | DSL lower 结果 |
|----------|----------------|----------------|
| `serviceTask.taskType` | 缺省 `"default"` | DSL 层**必填**（不静默默认），错误码 `missing_field` |
| `serviceTask.retries` | 缺省 3 | 缺省 3 |
| `serviceTask.timeoutSecs` | 恒为 60（XML 无法配置） | 缺省 60 |
| `userTask.formFields[].label` | 缺省 = 字段 id | 缺省 = 字段 id |
| `userTask.formFields[].type` | 缺省 string | 缺省 string（仅接受 `string/number/boolean/enum`） |
| `formFields[].required` | 缺省 false | 缺省 false |
| `timer.duration` | `timeDuration` 原文透传 | `TimerType::TimeDuration(原文)` |
| edge `default: true` | gateway `default` 属性把对应 flow 标记 `is_default` | 直接置 `is_default = true`（等价终态） |

节点 id 规则与 BPMN 一致：非空、唯一；`process.id` 即编译后的 definition id
（与 BPMN `process id` 行为一致）。

### 3. 错误契约：三层，路径统一为 JSON Pointer

| 层 | 覆盖 | 错误数 |
|----|------|--------|
| JSON 形状层 | 语法错误、未知字段（`deny_unknown_fields`）、缺字段、类型错误、未知枚举值 | 至多 1 条（流式反序列化的固有属性，如实记录） |
| 结构层（DSL 独有） | 重复节点/边 id、悬空 source/target 引用、空节点表、按类型必填/禁用字段、非法条件值 | **全量收集** |
| 语义层（透传） | start/end 数量、孤儿节点、网关形态、死路等 | **全量收集**（即 BPMN 编译器行为） |

- 每条 `DslError { code, path, message, hint, sourceCode? }`：`code` 为稳定
  snake_case 枚举；`path` 为 JSON Pointer（如 `/nodes/3/type`、`/edges/1/source`）。
- 语义层错误以 `compile` 码透传，`sourceCode` 字段携带上游
  `bpm_engine_bpmn::ErrorCode`（如 `NoEndEvent`），`path` 指回 DSL 文档中对应
  节点/边的位置。
- **结构层与语义层的分工**：DSL 层只做"形状"检查（JSON 结构、重复 id、悬空引用）；
  start/end/网关等语义检查**留在共享编译器**，不在 DSL 层重复实现
  （因此 DSL 错误码中没有 `MultipleStartNodes` 之类的语义码）。
- `deny_unknown_fields` + `serde_path_to_error` 共同提供字段级路径；错误分类从
  serde_json 的 Data 错误消息中稳定解析（`unknown field \`x\`` / `missing field \`x\``
  / `invalid type` / `unknown variant`）。

### 4. 条件（condition）的降级规则

核心事实：共享编译器 `parse_condition` 只产出 `EdgeCondition::Expression` /
`EdgeCondition::Default`（`VariableEq` 是 core 层为程序化构造保留的变体，
当前任何入口都无法经编译产生它）。

因此：

- `expression` 条件：表达式字符串**原样**写入 `condition_expression`（与 BPMN
  `<conditionExpression>` 文本行为一致，`${...}` 包裹由用户决定，编译时同规则剥离）。
- `variableEq { variable, value }` 条件：降级为规范表达式
  `${variable == "value"}`，再经共享编译器得到
  `EdgeCondition::Expression(variable == "value")`。
  - 运行时等价性：EL 引擎 `eval_eq` 对 `x == "value"` 做去引号后的字符串精确比较，
    与 `VariableEq` 的语义一致。前提是 EL 的布尔切分（` and `/` or `）不进入
    引号字面量——本 ADR 落地时修复了 EL 引擎的这一缺陷（`split_outside_quotes`，
    含回归测试 `test_boolean_split_ignores_quoted_literals`），因此含
    ` and `/` or ` 子串的 `value`（如 `"salt and pepper"`）也能正确精确匹配，
    且该修复同时惠及 BPMN `<conditionExpression>` 入口；
  - 该决策保住了"等价 BPMN 文档编译结果逐字段相同"的验收标准（等价 BPMN 即
    `<conditionExpression>${x == "value"}</conditionExpression>`）；
  - 代价：`variable`/`value` 不允许包含双引号（EL 不支持转义），结构层以
    `invalid_type` 拒绝并提示改用 `expression`。
- 同一边同时给出 `condition` 与 `default: true`：DSL 层拒绝
  （BPMN 容忍但条件是死代码；对 Agent 显式报错更安全）。

### 5. JSON Schema 与版本门控

- 使用 `schemars`（随类型派生 `JsonSchema`）生成 `schema_json()`，root 标题
  "BPM DSL v1"，`schemaVersion` 在 schema 中为 `const: 1`（后处理注入），并与
  `deny_unknown_fields` 一致地输出 `additionalProperties: false`。
- 生成物提交为 `crates/dsl/schema/bpm-dsl-v1.json`；单元测试断言
  **提交文件 == 现场生成**（防止类型与 schema 漂移），用
  `examples/dump_schema.rs` 重新生成。
- 运行时校验 `schema_version != 1` → `schema_version_unsupported`。未来演进
  （新增节点类型等）必须提升 `schema_version`，旧文档行为不变。

## 影响范围

- **新增** `crates/dsl`（package `bpm-engine-dsl`）：类型 + 校验 + lower + schema；
  不改 `crates/core`、`crates/bpmn`、`crates/runtime` 的任何行为
  （`bpmn` 的 `compile`/`parse`/model 类型本就已从 crate root 导出）。
- 根 workspace：members 增加 `crates/dsl`；根 crate facade 增加
  `pub use bpm_engine_dsl;`。
- 文档：`docs/dsl-guide.md`（用户指南 + 错误码表）、本 ADR、CHANGELOG。
- REST 部署端点**不在**本决策范围（随 API 契约 Issue 落地）。

## 企业内控补充

N/A — 开源项目，无企业内控约束。

## 后续动作

- [x] DSL v1 类型 + 结构校验 + lower + 共享编译（E2-1 交付）
- [x] `schema/bpm-dsl-v1.json` 提交并加 lockstep 测试
- [x] 与 BPMN 等价文档的编译一致性（parity）单元测试
- [ ] REST 部署端点 `/process-definitions`（dsl payload）（E2-x API 契约 Issue）
- [ ] v2 节点集（boundary events 等）按 `schema_version` 门控演进
