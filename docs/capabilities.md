---
layout: default
title: 能力范围清单
description: bpm-engine 当前能力全景与边界（Capability Inventory）
lang: zh
---

# 能力范围清单（Capability Inventory）

> 基线：master `125d5a3`（E1–E6 全部 28 个 issue 关闭后）。一句话总览：
> **一个持久化优先、token 驱动的 Rust 流程引擎，为"嵌入式 Rust 开发者 + REST/MCP Agent"双入口服务，
> 覆盖从定义校验、部署、执行、租约 worker、审批控制到审计追溯的完整闭环。**
> 894 个测试全绿；每个能力均经独立代码审查合并（PR #29–#50）。

## 1. 代码组成

| 组件 | 位置 | 角色 |
|---|---|---|
| `crates/core` | 纯逻辑 | Token/事件/节点语义、变量信封、AI envelope、审批记录类型 |
| `crates/storage` | trait 定义 | 11 个存储 trait（实例/令牌/定义/外部任务/定时器/历史/补偿/审批/DLQ…） |
| `crates/adapters/{memory,postgres}` | 双实现 | 开发内存版 + 生产 PostgreSQL 版 |
| `crates/runtime` | 引擎 | 事件泵、11 个 handler、网关求值、EL、timer 调度器 |
| `crates/bpmn` | BPMN 2.0 | XML 解析→编译→校验 + 类型化 builder + 静态验证/dry-run |
| `crates/dsl` | JSON DSL | 受限声明式定义 + schemars JSON Schema（lockstep） |
| `crates/server/rest` | REST 服务 | 41 操作规范契约 + 能力目录 + RBAC/审计 |
| `crates/worker-sdk` | worker 运行时 | 轮询/租约/取消/续租/幂等 + 工具安全边界 |
| `crates/ai-reference` | AI 参考流 | mock provider + 5 案例 + 回归评测 harness |
| `crates/mcp-server` | MCP 适配 | stdio JSON-RPC 工具服务器 |
| `ui/` | 运维控制台 | React 19 + Vite，9 个页面 + Playwright e2e |
| `templates/` | 可复制模板 | 独立编译 worker crate + 3 个 DSL 流 + MCP 配置 |

## 2. 能力清单（按域）

### 2.1 流程定义 —— 三个入口，一套语义

- BPMN 2.0 XML（REST 部署端点）；受限 JSON DSL（节点/边/超时/重试，
  `schema_version` 门控）；Rust `ProcessBuilder` 类型化构建器
- 三入口全部汇入同一 `BpmnProcess` 中间模型 + 同一编译器（parity 测试锁定）；
  错误含稳定 code + JSON-pointer 路径
- 部署前静态检查：9 项（不可达节点、缺默认分支、环/join 危险、任务配置、变量输入契约）
  + **dry-run**（纯模拟器，零副作用，REST 实证）
- 节点类型：start/end、service(external task)、userTask、两种网关、timer 捕获、
  call activity、message/signal、boundary event、terminate end

### 2.2 执行语义

- token/event 驱动：崩溃即重放恢复；token 恰好一次到终态；join 按组到达计数
- 外部任务租约：fetch-and-lock 原子交付、续租、过期回收重投
  （at-least-once，见 `docs/recovery.md` §8）、DLQ
- 定时器：持久化，重启后续跑，暂停期间冻结重调度
- Saga：补偿记录持久化 + 应用侧协调器（引擎不执行循环——诚实边界）
- 结构化失败：transient 重试 / **permanent 直入 DLQ 不重试**（E3-2）

### 2.3 持久化与恢复

- `BPM_DATABASE__DRIVER = memory | postgres`；postgres 启动即跑版本化迁移
  （0001–0004，baseline stamping，forward-only，ADR-005）
- 进程 SIGKILL 后重启：实例/租约/timer/完成态全部存活（真实二进制级 CI 测试）
- 单服务模型（双实例部署明确不在承诺内）

### 2.4 REST 契约（对 Agent 的核心承诺）

- **41 个操作**（22 只读 / 19 变更），OpenAPI 3.1 规范（`docs/openapi.yaml`）
- 31 个稳定 snake_case 错误码 + 每码示例体；统一错误形
  （`error`/`code`/`invariant_violation`）
- `Idempotency-Key` 启动幂等重放；分页（limit/offset）；`/api/v1` 冻结只做增量
- `GET /capabilities` 未认证发现端点：目录由 OpenAPI 确定性派生，
  lockstep 测试防漂移
- CI 一致性套件：路由双向对账 + 错误码全量对账 + 样本 schema 校验

### 2.5 身份、租户与控制

- JWT（HS256，四角色：deployer/operator/worker/auditor）fail-closed 权限矩阵
  + 全路由覆盖守卫；API-key 作 bootstrap；无凭证=开放模式
- 租户从已验证声明派生；跨租户读返回与未知 id **同形 404**（零存在性泄露）；
  worker fetch 租户过滤在原子锁区段内（**租约窃取不可能**）
- **引擎持有审批**（ADR-008）：审批前不建任务（worker 见不到未批准工作）；
  approve/reject/pause/resume/cancel 并发下仅生效一次
  （effect 状态机 + 重试重驱动 + 启动清扫）；`__approval` 由引擎写入
- 审计：边界事件带已验证 actor；`/audit` 端点关联操作人 + 定义版本 + 决策
  + artifact 引用；保留期清扫（history+DLQ）；回放响应**构造级不泄密**

### 2.6 数据与 AI 契约

- 变量：`HashMap<String,String>` 形态不变，`__var:1:` 信封
  （json / artifact URI：无 userinfo/fragment、≤2048、可选 sha256）
  + 64KiB 上限 + sensitive 必须加密 + 历史自动脱敏（截断/opaque）
- AI task envelope：execution_id、幂等键、超时、重试类别、模型/工具版本、
  产物引用、成本（currency/amount_micros/tokens）、输入输出 schema
- Worker 工具安全边界：deny-by-default allowlist、副作用工具审批门 fail-closed、
  凭证仅 env 注入（Debug 不泄值）、输出限额（与 core 截断标记 parity）
- Worker SDK：`WorkerHandle` 优雅关停（取消传播+排空）、自动续租、超时辅助、
  进程内幂等守卫

### 2.7 Agent/机器接口

- **MCP server**（`crates/mcp-server`，stdio JSON-RPC 2.0）：工具面目录驱动
  （默认只读 12，角色预设 + allowlist 纯下收，拼错即拒启）；8MiB 读层硬界；
  引擎侧租户/RBAC 实证
- 5 个 AI 参考流（检索/推理/审批/工具/补偿）+ mock provider（离线确定性）
  + `http` feature 后可换真实 OpenAI 兼容 provider

### 2.8 可解释性与评测

- `GET /steps`：因果步骤时间线（尝试/outcome/worker/envelope/成本/artifact），
  敏感值**构造级零暴露**
- 诊断 UI（`/diagnosis/:id`）：attempt 徽章、错误→历史深链、DLQ 链接
  （Playwright 15/15 无 mock）
- 离线回归评测：13 案例 × 2 定义版本，成功率/恢复正确性/介入率/成本比较；
  **统计输出与确定性断言严格分离**

### 2.9 运维与质量

- 运维控制台 9 页面（定义/实例/任务/worker/DLQ/诊断/追踪/不变量）；
  Prometheus 指标（feature 门控）；`/ready` 池/迁移/保留期健康
- CI 五个 job：fmt+clippy(-D warnings)+全测、覆盖、PG 迁移、PG 恢复
  （kill/restart）、quickstart 新环境验证
- 8 个 ADR、15 分钟 quickstart（每命令实测）+ 13 条排错手册

## 3. 明确**不在**当前范围内（诚实边界）

- PostgreSQL **双服务实例**并发运行（单服务承诺）
- 内建对象存储 adapter（artifact URI 仅契约，引擎永不解引用）
- JWT 无 `iss`/`aud`/`iat` 校验（共享密钥单服务边界，见 `docs/rbac.md`）；
  token 签发/吊销在带外
- 定义的部署/激活/废弃无历史审计行（仅日志）；定义为引擎全局（非租户隔离）
- 暂停期间到达的 message/signal **丢弃**（无缓冲，v2 项，ADR-008）；
  memory 驱动 DLQ requeue 不重建任务
- `TokenCompleted` 事件已定义但 v1 不发射；回放仅数据模型级；
  modern-era MCP 扩展（resources/prompts/sampling、HTTP transport）为 v2
- env 覆盖形为 `BPM__SERVER__PORT` 双下划线（config crate 前缀剥离怪癖，
  见 `docs/troubleshooting.md` §6）
