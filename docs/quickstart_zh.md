---
layout: default
lang: zh
---

# 15 分钟快速开始

端到端跑通 bpm-engine：启动引擎、部署流程、启动实例、用真实的 worker
完成外部任务、看到实例 `COMPLETED`——然后用 5 分钟浏览静态校验、步骤
时间线、Agent 工具目录和 MCP 接线。

**本指南中的每一条命令都在撰写时实际执行并记录了输出**（macOS、Apple
Silicon、Rust 1.96）。`scripts/verify-quickstart.sh` 脚本在 CI 中按断言
重跑同样的命令，因此文档不会与实际行为脱节。

## 时间预算（诚实数字）

| 环节 | 预算 | 实测 |
|---|---|---|
| 0. 前置准备 | 2 分钟 | — |
| 1. 克隆与构建 | *（计时时钟之前）* | M 系列笔记本冷构建 27 秒；其他机器预算 2–5 分钟（首次构建需下载并编译约 300 个 crate） |
| 2. 启动引擎 | 1 分钟 | 不到 2 秒即健康 |
| 3. 健康检查 | 1 分钟 | 每次请求约 1 毫秒 |
| 4. 通过 REST 部署流程 | 2 分钟 | 22 毫秒 |
| 5. 启动实例 | 1 分钟 | 41 毫秒 |
| 6. 运行 worker | 2 分钟 | worker 启动到任务完成约 110 毫秒 |
| 7. 看到 COMPLETED + 步骤 | 2 分钟 | 约 1 秒 |
| 8. 扩展环节 | 5 分钟 | 每项约 1 分钟 |

**首次构建之后 15 分钟。** 第一次 `cargo build` 是唯一慢的步骤；其后
每一步都是秒级。

---

## 0. 前置准备（2 分钟）

- **Rust（stable）**——用 [rustup](https://rustup.rs) 安装：
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- `git`、`curl`、`jq`（第 8 步用到）——多数系统已预装。
- **无需 Docker、无需数据库**——默认后端是内存模式，本指南用的正是它。

## 1. 克隆并构建 *（15 分钟时钟之前）*

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.90s
```

## 2. 启动引擎（1 分钟）

```bash
cargo run -p bpm-engine-server-rest
```

二进制名是 `api_server`；它打印生效配置（JSON 日志、密钥打码）后开始
监听：

```
{"message":"effective configuration","server.host":"0.0.0.0","server.port":3000,
 "database.driver":"memory", ...,"auth.jwt_secret":"<not set>","auth.api_key":"<not set>", ...}
{"message":"storage backend: memory (bootstrap definitions: minimal, payment-flow)"}
{"message":"timer scheduler started","poll_interval_ms":"1000"}
```

- 访问地址 **http://127.0.0.1:3000**（默认绑定 `0.0.0.0:3000`）。
- 内存模式预注册了两个**内置流程**：`minimal`（Start → End）和
  `payment-flow`（Start → 外部任务 `payment` → End）。
- **开放认证**（未配置凭据）——开发默认；生产模式见 `docs/rbac.md`。
- 自定义端口：`BPM__SERVER__PORT=3001 cargo run -p bpm-engine-server-rest`
  （注意 `BPM` 后是**双下划线**——如果你的环境变量似乎被忽略，见
  [故障排查](troubleshooting.md)）。

保持该终端打开；下面每条命令都在仓库根目录的**第二个终端**执行。

## 3. 健康检查（1 分钟）

```bash
curl -s http://127.0.0.1:3000/health
# {"status":"ok"}

curl -s http://127.0.0.1:3000/ready
# {"status":"ok","checks":{"engine":"ok","database":"ok"}}
```

`/health` 是存活探针（进程在）；`/ready` 检查引擎与存储后端。两者与
`/capabilities` 一样无需认证。

## 4. 通过 REST 部署流程（2 分钟）

`POST /api/v1/process-definitions/deploy` 接收 **BPMN 2.0 XML** 文档。
（JSON DSL 有自己的 REST 入口——第 8 步的 validate 与 dry-run——并通过
`bpm_engine_dsl::compile` 以编程方式部署；见
`templates/flows/README.md`。一个编译器、三个入口，`docs/dsl-guide.md`。）

部署一个两节点支付流程，其 service task 由你在第 6 步运行的 worker
处理：

```bash
cat > quickstart-payment.bpmn <<'EOF'
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="quickstart-payment" name="Quickstart Payment" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="charge" name="Charge Payment">
      <extensionElements>
        <taskDefinition type="payment"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end-ok"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="charge"/>
    <sequenceFlow id="f2" sourceRef="charge" targetRef="end-ok"/>
  </process>
</definitions>
EOF

curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/deploy \
  -H "Content-Type: application/xml" \
  --data-binary @quickstart-payment.bpmn
# {"process_definition_id":"quickstart-payment"}     ← HTTP 201
```

`<taskDefinition type="payment"/>` 扩展把这个 service task 变成类型为
`payment` 的外部任务——即 worker 订阅的"主题"。部署是不可变的版本；
同一 id 部署两次会生成版本 2。

## 5. 启动实例（1 分钟）

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"quickstart-payment","variables":{"amount":"100"}}'
# {"instance_id":"41028a07-1197-4ee7-90b1-38f7a89a0a1d","status":"RUNNING"}

export INSTANCE_ID=41028a07-1197-4ee7-90b1-38f7a89a0a1d   # ← 换成你自己的 id

curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID | jq '{status, current_nodes, variables}'
# {
#   "status": "RUNNING",
#   "current_nodes": ["charge", "start"],
#   "variables": { "amount": "100" }
# }
```

实例处于 `RUNNING`，一个 token **停在 `charge` 等待**：外部任务不在
引擎内执行——必须由 worker 获取、锁定并完成（基于租约，token 完成是
exactly-once 的）。

## 6. 运行 worker（2 分钟）

在第二个终端：

```bash
cargo run -p bpm-engine-worker-sdk --example payment
```

```
INFO payment worker starting; poll Engine at http://127.0.0.1:3000
INFO processing payment task_id=cfecf7dd-… amount=100
INFO task finished task_id=cfecf7dd-… task_type=payment worker_id=payment-worker-1 duration_ms=102 result="Complete"
```

worker 轮询 `fetch-and-lock`，拿到任务（它的 handler 服务
`task_type = "payment"`——与第 4 步的 `<taskDefinition type="payment"/>`
匹配），执行幂等的业务逻辑，然后带着
`{"status":"PAID","amount":"100"}` 完成任务，变量合并回实例。它会继续
轮询更多任务；用完后按 **Ctrl-C** 停止。

（首次运行会编译示例：第 1 步构建之后约 7 秒。）

## 7. 看到 COMPLETED（2 分钟）

```bash
curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID | jq '{status, variables}'
# {
#   "status": "COMPLETED",
#   "variables": { "status": "PAID", "amount": "100" }
# }
```

执行历史一步可达——**步骤时间线**（`docs/trace.md`）：

```bash
curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID/steps \
  | jq '.steps[] | {step_no, node_id, kind, events: [.events[].event_type]}'
# {"step_no":1,"node_id":"start","kind":"node","events":["TokenArrived"]}
# {"step_no":2,"node_id":"charge","kind":"external_task",
#  "events":["TokenArrived","ExternalTaskLocked","ExternalTaskCompleted"]}
# ...
```

你刚刚执行的是一个 crash-safe、可重放的工作流：上面的每一步都记录为
不可变的历史事件（`.../history`），同一次执行可以被确定性重放。

---

## 8. 五分钟扩展

### 8.1 校验并 dry-run 一个 DSL 流程（不部署、无副作用）

仓库自带机器可读的流程模板（`templates/flows/`）。部署前先检查——编译
失败以 **HTTP 200 数据**返回而不是报错，方便 Agent 读取并修复：

```bash
jq -Rs '{format: "dsl", source: .}' templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/validate \
    -H "Content-Type: application/json" -d @- | jq '{valid, findings}'
# {"valid":true,"findings":[{"severity":"Info","code":"condition_variable_scope",…}]}

jq -Rs '{format: "dsl", source: ., variables: {status: "PAID"}}' \
  templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/dry-run \
    -H "Content-Type: application/json" -d @- | jq '.dry_run.terminus'
# {"type":"completedAt","node_id":"end-paid"}
```

dry-run 是用你的假设变量做的无副作用走查——不创建实例、任务、定时器
或历史记录（`docs/validation-and-dry-run.md`）。流程编写：`docs/dsl-guide.md`。

### 8.2 端到端跑一个 AI 流程（自包含）

```bash
cargo run --example ai_flow
```

不需要服务器和密钥：它启动本地引擎、部署审批 **DSL** 流程、用离线
确定性 mock 模型提供者跑完，并打印每步的 AI 信封（模型 + token 成本、
使用的工具）：

```
state    : COMPLETED
steps:
  #2 propose    external_task  … envelope: "mock"/"mock-model-v1" 8 "USD" (16->8 tokens)
  #3 approve    node           ["TokenArrived", "UserTaskCompleted"]
  #4 act        external_task  … tools=[ledger_post]
```

真实提供者接线：`docs/ai-reference-flows.md`。这些任务背后的 worker
就是 `templates/ai-worker/` 的形态。

### 8.3 浏览机器可读的 Agent 工具目录

```bash
curl -s http://127.0.0.1:3000/capabilities | jq '.tools | length'
# 35
curl -s http://127.0.0.1:3000/capabilities | jq '.tools[] | select(.classification=="read_only") | .name'
```

每个 REST 操作一条：参数 schema、只读/可变更分类、以及各操作可能返回
的稳定错误码——这是 AI Agent 编码所依据的契约（`docs/agent-tools.md`，
OpenAPI：`docs/openapi.yaml`）。

### 8.4 接线 MCP 客户端

通过 stdio 服务器 `bpm-engine-mcp-server` 把引擎的工具目录暴露给 MCP
客户端（Claude Desktop/Code、Cursor 等）。把 `templates/mcp-config.json`
复制进你的客户端并替换铸造的 token：

```bash
BPM_JWT_SECRET=$(…引擎的 auth.jwt_secret…) \
cargo run -p bpm-engine-server-rest --example mint_jwt -- \
    --sub mcp-agent --tenant acme --role operator
```

完整接线与安全说明：`templates/README.md` 与 `docs/mcp-server.md`。

### 8.5 接下来去哪

- **遇到问题？** [troubleshooting.md](troubleshooting.md)——新用户真实
  会踩的失败模式（401/403、端口、Postgres、DSL 错误、任务卡住、DLQ、
  429、MCP），每条都是症状 → 原因 → 修复。
- **可复制的起点**——[templates/](../templates/)：流程 JSON、可编译的
  worker crate、MCP 配置。
- **所有可运行示例**——[examples/index.md](examples/index.md)。
- **JWT 身份、角色、租户**——`docs/rbac.md`（用 `mint_jwt` 示例铸造
  token，见 8.4）。
- **PostgreSQL 持久化**——`docs/recovery.md` + `docs/database-schema.md`；
  crash 恢复演示：`./deploy/verify-recovery.sh`。
- **架构与保证**——`docs/architecture.md`、`docs/invariants.md`、
  `docs/execution-model.md`。
