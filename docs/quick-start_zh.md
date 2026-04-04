# 快速开始指南（5 分钟）

本指南帮助你在 5 分钟内启动并运行第一个 BPMN 流程。

## 前置要求

- Rust（stable）
- Cargo
- 默认内存后端，无需 Docker 或数据库

## 步骤 1：克隆并构建

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

## 步骤 2：启动引擎服务器

```bash
cargo run -p bpm-server-rest
```

服务器将在 **http://127.0.0.1:3000** 启动。

内置流程定义：
- `minimal`：Start → End（最简单流程）
- `payment-flow`：Start → ExternalTask `payment` → End

## 步骤 3：运行最小流程（Start → End）

打开另一个终端，运行：

```bash
cargo run --example simple_process
```

或使用 curl：

```bash
# 启动流程实例
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'

# 检查流程状态（等待 status 变为 COMPLETED）
curl http://127.0.0.1:3000/api/v1/process-instances/{instance_id}
```

## 步骤 4：运行带有外部任务的流程

### 3.1 启动流程实例

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
```

### 3.2 启动 Payment Worker

打开第三个终端：

```bash
cargo run -p bpm-worker-sdk --example payment
```

Worker 将：
1. 轮询引擎获取 `payment` 任务
2. 锁定任务
3.执行业务逻辑（处理支付）
4. 完成任务
5. 流程继续到 End

### 完整的 Payment Worker 代码示例

```rust
use bpm_worker_sdk::{EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig};

struct PaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str { "payment" }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // 获取流程变量
        let amount = task.variables.get("amount")
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        println!("Processing payment: amount={}", amount);

        // 业务逻辑：处理支付...

        // 返回结果，更新变量
        let mut variables = std::collections::HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        variables.insert("transaction_id".to_string(), format!("tx_{}", uuid::Uuid::new_v4()));

        TaskResult::Complete { variables }
    }
}

// 创建 Worker
let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(PaymentHandler)
    .config(WorkerConfig::new("worker-1")
        .poll_interval(std::time::Duration::from_secs(1)))
    .build();

// 启动 Worker
worker.start().await;
```

## API 快速参考

| 方法 | 路径 | 描述 |
|------|------|------|
| POST | `/api/v1/process-instances` | 启动新流程实例 |
| GET | `/api/v1/process-instances/:id` | 获取实例状态 |
| GET | `/api/v1/process-instances/:id/history` | 获取执行历史 |
| POST | `/api/v1/external-tasks/fetch-and-lock` | Worker 获取并锁定任务 |
| POST | `/api/v1/external-tasks/:id/complete` | Worker 完成任务 |
| POST | `/api/v1/external-tasks/:id/fail` | Worker 标记任务失败 |

## 常见问题

### Q: 流程卡住不动怎么办？

检查：
1. 外部任务是否有 Worker 在运行？
2. Worker 是否正确注册了任务类型？
3. 查看历史事件：`GET /api/v1/process-instances/:id/history`

### Q: 如何查看流程定义？

```bash
curl http://127.0.0.1:3000/api/v1/process-definitions
```

### Q: 如何部署自定义 BPMN 流程？

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-definitions/deploy \
  -H "Content-Type: text/plain" \
  --data-binary @your-process.bpmn
```

## 下一步

- 阅读 [架构文档](architecture_zh.md) 深入了解引擎设计
- 查看 [API 文档](api-spec.md) 完整 API 参考
- 阅读 [不变式文档](invariants.md) 了解引擎保证

## 需要帮助？

- 查看 [常见问题](faq.md)
- 提交 Issue：https://github.com/fanjia1024/bpm-engine/issues
