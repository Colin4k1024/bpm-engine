# ADR-004: Timer Scheduler Embedded in Runtime Crate

## 决策信息

- **编号**: ADR-004
- **状态**: Accepted
- **日期**: 2026-05-21
- **Owner**: tech-lead
- **关联**: PRD engine-quality-hardening Q2

## 背景与约束

Timer 调度器需要周期性查询 `TimerStore::list_due()` 并将到期 timer 转化为 `EngineEvent::TimerFired`。需要决定其代码位置和生命周期管理方式。

## 备选方案

| 方案 | 优点 | 风险 |
|------|------|------|
| **A: 嵌入 `crates/runtime`，`tokio::spawn` 后台任务** | 与 BpmEngine 生命周期绑定，部署简单 | 引擎停止时需 graceful shutdown |
| B: 独立 crate `crates/timer-scheduler` | 可独立部署、独立测试 | 当前逻辑量 < 200 行，过度拆分 |

## 决策结果

**采用方案 A**：Timer scheduler 作为 `crates/runtime` 中的 `timer_scheduler.rs` 模块，通过 `tokio::spawn` 启动后台循环。

**设计要点**：
- Poll interval 默认 1s，可配置
- 启动时先执行一次 `list_due()` 处理 crash recovery 期间积压的 timer
- 使用 `CancellationToken` 实现 graceful shutdown
- Timer fired 后通过 shared channel 送入 EventPump

**精度承诺**：best-effort，±2s 内（非实时系统）

## 后续动作

- backend-engineer 实现 TimerScheduler
- 补充集成测试验证 timer 到期推进 token
