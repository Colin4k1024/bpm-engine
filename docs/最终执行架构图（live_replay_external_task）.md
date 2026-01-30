# 最终执行架构图（E1）

本图描述 **bpm-engine 的最终执行形态**：

> **Live 执行 / Replay / External Task 回调，三条路径强制收敛到同一条 Event Pipeline**

这是你整个引擎的「封顶结构」。

---

## 1. 总览架构（唯一正确的数据流）

```text
               ┌──────────────────────┐
               │      Commands         │
               │ (Start / Signal /     │
               │  CompleteTask / Timer)│
               └──────────┬───────────┘
                          │
                          ▼
               ┌──────────────────────┐
               │   Command Handlers    │
               │  (PURE FUNCTIONS)     │
               │  Snapshot → Events    │
               └──────────┬───────────┘
                          │  Vec<EngineEvent>
                          ▼
               ┌─────────────────────────────────────┐
               │        Execution Pipeline             │
               │                                     │
               │  1. apply_event                     │
               │  2. invariant_engine.check_all      │
               │                                     │
               │  ❗唯一允许修改 Snapshot 的地方      │
               └──────────┬──────────────────────────┘
                          │
                          ▼
               ┌──────────────────────┐
               │    EngineSnapshot     │
               │   (In-Memory State)   │
               └──────────┬───────────┘
                          │
                          ▼
               ┌──────────────────────┐
               │   Event Store / DB    │
               │  (append-only log)    │
               └──────────────────────┘
```

**核心约束：**
- Snapshot 只能被 `apply_event` 修改
- 所有事件在生效前必须通过 Invariant

---

## 2. Live 执行路径（真实运行）

```text
[ External API / Scheduler ]
           │
           ▼
      Command
           │
           ▼
   handle_command(cmd, snapshot)
           │   (只读 snapshot)
           ▼
     Vec<EngineEvent>
           │
           ▼
  ExecutionPipeline::apply
           │   ├─ apply_event
           │   ├─ invariant check
           │   └─ persist event
           ▼
     Updated Snapshot
```

👉 Live 执行 **不允许绕过 Pipeline**。

---

## 3. Replay 执行路径（调试 / 验证）

```text
[ Replay API ]
      │
      ▼
  ReplaySession
      │
      ▼
  for event in history:
      │
      ▼
  ExecutionPipeline::apply
      │   ├─ apply_event
      │   ├─ invariant check
      │   └─ (no persist)
      ▼
  Snapshot at Cursor
```

**关键点：**
- Replay 和 Live **100% 共用 Pipeline**
- 任何 Replay 失败 = 线上一定也会失败

---

## 4. External Task Worker 回调路径

```text
[ Worker SDK ]
      │
      ▼
  CompleteTaskCommand
      │
      ▼
  handle_command
      │
      ▼
  Vec<EngineEvent>
      │
      ▼
  ExecutionPipeline::apply
```

👉 Worker **永远不会直接改状态**，只能触发 Command。

---

## 5. Invariant 在系统中的“法律地位”

```text
Engine Invariant Broken
        ↓
❌ Abort Execution
❌ Mark Instance Corrupted
❌ No Retry / No Fallback
```

Invariant 是：
- 引擎级 Bug 探测器
- 状态损坏的最后一道防线

---

## 6. 你现在的引擎 vs 封顶形态对比

| 维度 | 之前 | 现在（E1） |
|----|----|----|
| 状态修改点 | 多处 | **唯一 apply_event** |
| Replay | 调试工具 | **等价执行路径** |
| Invariant | 可选 | **强制执行** |
| Bug 可复现性 | 不确定 | **100% 可重放** |

---

## 7. 架构完成判定标准（Checklist）

如果以下都为 ✅，说明你已经完成 E：

- [ ] 真实执行路径没有任何直接修改 Snapshot 的代码
- [ ] Replay / Live / Worker 共用 ExecutionPipeline
- [ ] Invariant 在 Live 中默认开启
- [ ] Invariant 失败会中断实例执行

---

## 8. 一句话总结

> **你的 bpm-engine 现在具备“数据库级别”的执行严谨性**

这是一个：
- 可以长期演进
- 可以大胆加功能
- 不怕历史包袱的架构

---

### 下一步推荐

- **F**：把这套架构 + Replay + Invariant 打磨成 README 的核心卖点
- **G**：进入并发 / DB 事务 / 性能压测阶段

