# Session: 2026-04-04 — bpm-engine-evolution-plan

**日期**: 2026-04-04
**任务**: bpm-engine-evolution-plan（演进规划与 Sprint 1-4 执行）
**状态**: closed

---

## 链路起止

- **开始**: intake + team-plan + team-execute（Sprint 1-4）
- **结束**: /team-closeout 收口完成

---

## 任务

对 bpm-engine 项目审查后（bpm-engine-review）的遗留项进行演进规划，并执行 Sprint 1-4。

---

## 产出

| 类别 | 产出 |
|------|------|
| 规划 | PRD, Delivery Plan, Arch Evolution Roadmap, Test Enhancement Roadmap, P0 Decisions |
| 实现 | PostgreSQL 适配器, EL 表达式修复, ParallelJoin 方案 B, 并发测试, Doc-tests |
| 测试 | 104 tests（+81 新测试） |
| 文档 | API Contract, ADR-002 updated |
| 覆盖率 | ~27% → ~70% |

---

## 遗留项

| 项目 | 优先级 | 触发条件 |
|------|--------|----------|
| PostgreSQL 适配器完整实现 | P1 | 中期 |
| src/legacy_engine.rs 移除 | P3 | 中期 |
| Python Worker SDK | P3 | 长期 |
| 开源发布准备 | P1 | 当前 |

---

## 后续跟踪触发条件

- 用户决定修复 H2 in-memory 限制时 → 重新评估 MemoryRepo 设计
- 需要 PostgreSQL 完整实现时 → 启动相关任务
- 开源发布准备 → 启动 release 流程

---

## 关键 lessons learned

1. **实现先于测试**：ParallelJoin 测试期望新方案 B，但实现尚未修改。今后应先完成实现再写测试。
2. **时间相关测试**：lock expiry 测试的 sleep 时长必须大于 lock duration。
3. **依赖冲突**：sqlx 与 rusqlite 冲突，选用 tokio-postgres + deadpool-postgres。
4. **并发测试价值**：关键 invariant（TOCTOU）应优先补充并发测试验证。
