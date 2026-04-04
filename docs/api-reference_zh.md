---
artifact: api-reference
task: chinese-docs-support
date: 2026-04-04
role: backend-engineer
status: draft
---

# REST API 参考文档 v1

**Base URL**: `/api/v1`

所有接口均基于此 Base URL。服务器默认监听 `http://127.0.0.1:3000`。

## 认证与请求头

| Header | 必填 | 说明 |
|--------|------|------|
| `X-Tenant-Id` | 否 | 多租户部署场景下的租户标识符，会传递到 Engine Context。 |
| `Idempotency-Key` | 建议 | 用于 POST 操作幂等的保留请求头。**尚未实现** —— 详见缺失实现章节。 |

## 共享类型

### ErrorResponse

```json
{
  "error": "string",
  "invariant_violation": "string | null"
}
```

- `invariant_violation` 仅在错误类型为 `InvariantViolation` 时出现（例如 "EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER"）。当此字段存在时，响应还会包含 `X-Invariant-Violation` 请求头，值相同。

### CompleteTaskResponse

```json
{
  "status": "COMPLETED | FAILED"
}
```

---

## 流程定义

### `GET /process-definitions/:id`

获取流程定义图谱视图（供 Trace UI 使用）。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `id` | string | 流程定义 ID |

**响应** `200 OK`

```json
{
  "id": "payment-flow",
  "start": "start",
  "nodes": [
    { "id": "start", "node_type": "Start" },
    { "id": "payment", "node_type": "ExternalTask" },
    { "id": "end", "node_type": "End" }
  ],
  "edges": [
    { "source": "start", "target": "payment" },
    { "source": "payment", "target": "end" }
  ]
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 流程定义未找到 |

---

### `POST /process-definitions/deploy`

从 BPMN 2.0 XML 部署流程定义。

**请求体**: 原始 BPMN 2.0 XML 字符串（Content-Type: `text/plain`）

**响应** `201 Created`

```json
{
  "process_definition_id": "my-process"
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `DeployErrorResponse` (Parse variant) | XML 解析错误 |
| `400` | `DeployErrorResponse` (Compile variant) | BPMN 编译错误（`CompilerError` 列表） |

---

## 流程实例

### `POST /process-instances`

启动新的流程实例。

**请求体**

```json
{
  "process_def_id": "string",
  "variables": {}
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `process_def_id` | string | 是 | 要启动的流程定义 ID |
| `variables` | object | 否 | 初始变量，键值对形式 |

**响应** `201 Created`

```json
{
  "instance_id": "uuid",
  "status": "RUNNING"
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `ErrorResponse` | 流程定义未找到或请求无效 |

---

### `GET /process-instances/:id`

获取流程实例的当前状态。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `id` | string | 流程实例 ID |

**响应** `200 OK`

```json
{
  "instance_id": "string",
  "process_def_id": "string",
  "status": "RUNNING | COMPLETED | TERMINATED",
  "current_nodes": ["node_id_1", "node_id_2"],
  "tokens": [Token, ...]
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 流程实例未找到 |

---

### `GET /process-instances/:id/trace`

聚合执行轨迹：包含实例状态、Token 时间线和外部任务历史。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `id` | string | 流程实例 ID |

**响应** `200 OK`

```json
{
  "instance": InstanceStateResponse,
  "token_timelines": [
    {
      "token_id": "string",
      "node_id": "string",
      "status": "CREATED | READY | EXECUTING | WAITING | SUSPENDED | COMPLETED | TERMINATED",
      "events": [
        {
          "event_type": "string",
          "occurred_at": "unix_timestamp",
          "payload": {}
        }
      ]
    }
  ],
  "external_task_history": [
    {
      "task_id": "string",
      "token_id": "string",
      "process_instance_id": "string",
      "events": [TraceEventView, ...]
    }
  ]
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 流程实例未找到 |
| `500` | `ErrorResponse` | 历史记录获取错误 |

---

### `GET /process-instances/:id/history`

执行历史记录，用于审计和 Trace UI。按因果顺序返回事件。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `id` | string | 流程实例 ID |

**查询参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `token_id` | string | 可选过滤器：仅返回此 Token 的事件 |
| `event_type` | string | 可选过滤器：仅返回此类型的事件 |

**响应** `200 OK`

```json
{
  "instance_id": "string",
  "events": [
    {
      "sequence": 0,
      "id": "string",
      "event_type": "ProcessStarted | TokenArrived | TokenCompleted | ...",
      "category": "instance | token | external",
      "occurred_at": "unix_timestamp",
      "payload": {}
    }
  ]
}
```

**事件分类**

| 分类 | 事件类型 |
|------|----------|
| `instance` | `ProcessStarted`, `ProcessCompleted` |
| `token` | `TokenArrived`, `TokenCompleted`, `TokenFailed`, `UserTaskCreated`, `UserTaskCompleted`, `TimerScheduled`, `TimerFired`, `SagaStarted`, `SagaCompleted` |
| `external` | `ExternalTaskLocked`, `ExternalTaskCompleted`, `ExternalTaskFailed` |

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `500` | `ErrorResponse` | 历史记录获取错误 |

---

## 任务

### `GET /tasks`

列出所有运行中流程实例的等待中任务（用户任务和外部任务）。

**查询参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `type` | string | 可选过滤器：`user` 或 `external` |

**响应** `200 OK`

```json
[
  {
    "task_id": "instance_id:node_id",
    "node_id": "string",
    "instance_id": "string",
    "task_type": "user | external"
  }
]
```

---

### `POST /tasks/:task_id/complete`

完成一个用户任务。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `task_id` | string | 任务 ID，格式为 `instance_id:node_id` |

**请求体**

```json
{
  "variables": {}
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `variables` | object | 否 | 任务完成的输出变量 |

**响应** `200 OK`

```json
{ "status": "COMPLETED" }
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `ErrorResponse` | 无效的 `task_id` 格式 |
| `500` | `ErrorResponse` | 引擎执行错误 |

---

## 外部任务

外部任务采用 Lease 模型：Worker 获取并锁定任务，然后完成或失败该任务。任意时刻只有一个 Worker 能持有锁。过期锁会被自动回收。

### `POST /external-tasks/fetch-and-lock`

为 Worker 获取并锁定可用的外部任务。

**请求体**

```json
{
  "worker_id": "string",
  "task_types": ["payment", "notification"],
  "max_tasks": 10,
  "lock_duration_ms": 60000
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `worker_id` | string | 是 | 请求方 Worker 的标识符 |
| `task_types` | array of string | 是 | 要获取的任务类型（例如 `["payment"]`） |
| `max_tasks` | integer | 否 | 最大锁定任务数（默认：10） |
| `lock_duration_ms` | integer | 是 | 锁持续时间（毫秒）；未在规定时间内完成的任务将被回收 |

**响应** `200 OK`

```json
[
  {
    "task_id": "string",
    "token_id": "string",
    "process_instance_id": "string",
    "task_type": "string",
    "variables": {}
  }
]
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `500` | `ErrorResponse` | 锁回收或获取错误 |

---

### `POST /external-tasks/:task_id/complete`

完成一个已锁定的外部任务。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `task_id` | string | 外部任务 ID |

**请求体**

```json
{
  "worker_id": "string",
  "variables": {}
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `worker_id` | string | 是 | Worker ID（必须与锁持有者匹配） |
| `variables` | object | 否 | 要合并到流程实例的输出变量 |

**响应** `200 OK`

```json
{ "status": "COMPLETED" }
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `ErrorResponse` with `X-Invariant-Violation: EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER` | 任务已被其他 Worker 锁定 |
| `400` | `ErrorResponse` with `X-Invariant-Violation: EXTERNAL_TASK_NOT_LOCKED` | 任务当前未被锁定 |
| `404` | `ErrorResponse` | 任务未找到 |
| `500` | `ErrorResponse` | 引擎执行错误 |

---

### `POST /external-tasks/:task_id/fail`

将一个已锁定的外部任务标记为失败。如果任务已耗尽重试次数，则对应的 Token 也会被标记为失败。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `task_id` | string | 外部任务 ID |

**请求体**

```json
{
  "worker_id": "string",
  "error": "string",
  "retry_after_ms": 30000
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `worker_id` | string | 是 | Worker ID（必须与锁持有者匹配） |
| `error` | string | 是 | 人类可读的错误描述 |
| `retry_after_ms` | integer | 否 | 如果存在此字段，任务将在此时间后可以重新获取 |

**响应** `200 OK`

```json
{ "status": "FAILED" }
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `ErrorResponse` with `X-Invariant-Violation` | 不变量违反（例如 Worker 不匹配） |
| `404` | `ErrorResponse` | 任务未找到 |
| `500` | `ErrorResponse` | 引擎执行错误 |

---

## 重放会话

重放会话是临时的（不持久化）。它们允许逐步执行流程实例的历史事件，以生成只读快照。

### `POST /process-instances/:id/replay`

为流程实例创建重放会话。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `id` | string | 流程实例 ID |

**响应** `201 Created`

```json
{
  "session_id": "uuid",
  "instance_id": "string",
  "total_events": 42
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 流程实例未找到 |
| `500` | `ErrorResponse` | 历史记录获取错误 |

---

### `GET /replay/:session_id/snapshot`

获取重放会话在当前游标位置的只读快照。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 重放会话 ID |

**响应** `200 OK`

```json
{
  "cursor": 10,
  "total_events": 42,
  "completed": false,
  "tokens": [
    { "token_id": "string", "node_id": "string", "state": "WAITING" }
  ]
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 会话未找到或已过期 |

---

### `POST /replay/:session_id/step`

将下一个事件应用到重放中，并将游标向前移动一位。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 重放会话 ID |

**响应** `200 OK`

```json
{
  "cursor": 11,
  "event": {
    "event_type": "TokenArrived",
    "occurred_at": "1700000000",
    "token_id": "string | null",
    "node_id": "string | null"
  },
  "snapshot": {
    "completed": false,
    "tokens": [{ "token_id": "string", "node_id": "string", "state": "WAITING" }]
  }
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `400` | `ErrorResponse` | 没有更多事件可以步进 |
| `404` | `ErrorResponse` | 会话未找到或已过期 |
| `500` | `ErrorResponse` | 重放应用失败 |

---

### `POST /replay/:session_id/seek`

通过重放从 `0` 到 `cursor` 的所有事件，跳转到指定的游标位置。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 重放会话 ID |

**请求体**

```json
{
  "cursor": 20
}
```

**响应** `200 OK`

```json
{
  "cursor": 20,
  "snapshot": {
    "completed": false,
    "tokens": [{ "token_id": "string", "node_id": "string", "state": "WAITING" }]
  }
}
```

**错误响应**

| 状态码 | Body | 说明 |
|--------|------|------|
| `404` | `ErrorResponse` | 会话未找到或已过期 |
| `500` | `ErrorResponse` | 重放应用失败 |

---

### `DELETE /replay/:session_id`

销毁重放会话并释放资源。

**路径参数**

| 名称 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 重放会话 ID |

**响应** `204 No Content`

---

## 错误码定义

所有错误都返回包含 `error` 字符串字段的 `ErrorResponse` JSON。

### HTTP 状态码

| 状态码 | 含义 |
|--------|------|
| `200` | 成功 |
| `201` | 已创建 |
| `204` | 无内容（成功的删除操作） |
| `400` | 错误请求（无效输入、不变量违反、解析/编译错误） |
| `404` | 未找到 |
| `500` | 内部服务器错误 |

### 不变量违反类型

当引擎检测到协议违反时，`invariant_violation` 字段会被填充，响应还会包含 `X-Invariant-Violation` 请求头。

| 类型 | 含义 | 受影响的操作 |
|------|------|-------------|
| `EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER` | 任务被其他 Worker 锁定 | `external-task-complete`, `external-task-fail` |
| `EXTERNAL_TASK_NOT_LOCKED` | 任务当前未被锁定 | `external-task-complete`, `external-task-fail` |
| `EXTERNAL_TASK_NOT_FOUND` | 任务不存在 | `external-task-complete`, `external-task-fail` |
| `TOKEN_NOT_FOUND` | Token 在实例中未找到 | `external-task-complete` |

---

## 缺失实现：幂等键

REST API 中预留了 `Idempotency-Key` 请求头，但**目前在任何端点处理器中都未实现**。该请求头位置尚未在路由处理器中接入。

**影响**：所有变更类端点（`POST /process-instances`、`POST /tasks/:task_id/complete`、`POST /external-tasks/fetch-and-lock`、`POST /external-tasks/:task_id/complete`、`POST /external-tasks/:task_id/fail`、`POST /process-definitions/deploy`、`POST /process-instances/:id/replay`）都不是幂等的。重试请求可能导致重复副作用。

**受影响的端点**：上述所有 POST 变更端点。

**实现指导**：添加幂等支持的方法：

1. 在每个变更处理器中使用 `headers.get("Idempotency-Key")` 提取 `Idempotency-Key` 请求头。
2. 在专用存储中存储 key → 响应 的映射（例如 Redis 或现有的 `MemoryRepo`）。
3. 对于带有相同 key 的后续请求：
   - 如果已缓存响应且操作成功，返回缓存响应并使用 `200 OK`。
   - 如果已缓存响应且操作仍在进行中，返回 `409 Conflict`。
4. 考虑为幂等记录设置 TTL（例如 24 小时）。
5. 至少应先为 `POST /external-tasks/:task_id/complete` 添加幂等支持，因为它是 Worker SDK 客户端最可能重试的操作。

---

## 端点汇总

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/process-definitions/:id` | 获取流程定义图谱视图 |
| `POST` | `/process-definitions/deploy` | 部署 BPMN 2.0 XML |
| `POST` | `/process-instances` | 启动流程实例 |
| `GET` | `/process-instances/:id` | 获取流程实例状态 |
| `GET` | `/process-instances/:id/trace` | 获取聚合执行轨迹 |
| `GET` | `/process-instances/:id/history` | 获取执行历史 |
| `POST` | `/process-instances/:id/replay` | 创建重放会话 |
| `GET` | `/tasks` | 列出等待中的任务 |
| `POST` | `/tasks/:task_id/complete` | 完成用户任务 |
| `POST` | `/external-tasks/fetch-and-lock` | 获取并锁定外部任务 |
| `POST` | `/external-tasks/:task_id/complete` | 完成已锁定的外部任务 |
| `POST` | `/external-tasks/:task_id/fail` | 标记已锁定的外部任务为失败 |
| `POST` | `/replay/:session_id/step` | 步进一个重放事件 |
| `POST` | `/replay/:session_id/seek` | 跳转到游标位置 |
| `GET` | `/replay/:session_id/snapshot` | 获取重放快照 |
| `DELETE` | `/replay/:session_id` | 删除重放会话 |

**共计：16 个端点**
