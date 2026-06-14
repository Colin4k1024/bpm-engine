---
layout: default
lang: zh
---

# 架构概览

> 本文档描述 Rust BPM Engine 的**运行时架构**。
> 重点关注**执行语义**，不涉及建模工具或 BPMN 规范。

---

## 范围

本架构文档定义了：

- 工作流在运行时的执行方式
- 状态如何通过事件推进
- 如何处理并发、故障和恢复

本文档**不涉及**以下内容：

- BPMN XML 规范
- 可视化建模或低代码工具
- UI 相关内容

---

## 核心抽象

### Process Definition（流程定义）

描述工作流结构的静态图。

- Nodes（节点）：Start、Service、User、Gateway、End
- Sequence flows（顺序流）

### Process Instance（流程实例）

流程定义的一个运行实例。

- 持有 variables（变量）
- 拥有 tokens（令牌）
- 具有生命周期：Running / Completed / Terminated

### Token（令牌）

> **Token 是执行的最小单位。**

Token 代表在特定节点执行的权利。

- 多个 Token 可以实现并行执行
- Token 独立向前推进

---

## 执行模型

- Token 在流程图中移动
- 节点通过消费 Token 来执行
- 执行过程产生 events（事件）

所有状态转换都由**事件**驱动，而非直接调用。

参见：`execution-model.md`

---

## 事件驱动核心

- Engine 对不可变事件做出反应
- Event handlers 是确定性的且事务性的
- Handler 可以发射新的事件

这保证了：

- 可观测性（Observability）
- 可重放性（Replayability）
- 故障安全（Crash safety）

---

## 并发模型

- 并发是**Token 作用域内**的
- 一个 Token 在任一时刻只能被一个执行器执行
- 乐观锁（CAS）防止冲突

并行 Join 通过唯一性约束保护。

---

## 故障与补偿

- 故障是一等事件
- 长时运行一致性通过 Saga 模式实现
- 补偿按逆序执行

参见：`saga.md`

---

## 故障恢复

- 所有 Engine 状态都是持久化的
- Token 在故障后可以重新恢复
- 执行总是向前继续

参见：`recovery.md`

---

## 设计原则

- Token 优于 thread
- Event 优于 call stack
- Compensation 优于 rollback
- Persistence 优于 memory

---

本文档作为架构与实现之间的**契约**。
