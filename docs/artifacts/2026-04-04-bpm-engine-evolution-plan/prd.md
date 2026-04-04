---
artifact: prd
task: bpm-engine-evolution-plan
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 演进方向规划 — 需求简报

## 1. 背景

`bpm-engine` 项目于 2026-04-04 完成首次全面审查（bpm-engine-review），识别出以下关键遗留项：

| 优先级 | 问题 | 状态 |
|--------|------|------|
| **P0** | `fetch_and_lock` TOCTOU 竞态 | ADR-001 accepted，**代码未修复** |
| **H1** | ParallelJoin group_id 语义错误 | ADR-002 proposed，**未决策** |
| **H2** | in-memory fallback 状态丢失 | 接受限制 |
| **H3** | EL 表达式不支持负数 | 未修复 |
| **Low** | 13 个 .bak 文件 | 未清理 |
| **Low** | 0 doc-tests | 未补充 |
| **架构** | `src/legacy_engine.rs` 未移除 | 未处理 |

**项目当前阶段**：原型/研究阶段，适合生产使用前需要解决 P0 和 H1 问题。

## 2. 目标与优先级

| 优先级 | 目标 |
|--------|------|
| **P0** | 决策并修复 `fetch_and_lock` TOCTOU（方案 A：原子 WriteLock） |
| **P0** | 决策 ParallelJoin group_id 语义（方案 A/B/C 之一） |
| **P1** | 修复 H3 EL 表达式负数支持 |
| **P1** | 补充 `try_join` 边界测试 + 并发测试 |
| **P2** | 清理 13 个 .bak 文件 |
| **P2** | 补充 doc-tests（core/runtime/storage 公开 API） |
| **P2** | 文档化 Handler 顺序约束 |
| **P3** | 移除 `src/legacy_engine.rs` |
| **P3** | 更新 `docs/bpmn-spec-mapping.md` 与代码一致 |

**成功标准**：
- P0 问题得到明确决策和修复
- H1 问题得到明确决策（修复或接受限制并文档化）
- 所有 Medium/High 问题有明确的处理策略
- 产出下一阶段的 backlog 快照

## 3. 关键约束

- **只做计划，不实现代码**
- 演进方向需考虑：开源项目维护节奏、社区贡献可能性、生产可用性门槛
- 需要平衡：快速迭代 vs 架构稳定性

## 4. 参与角色清单

| 角色 | 主责 |
|------|------|
| `tech-lead` | 统筹演进方向、决策优先级、收口结论 |
| `architect` | 评估架构演进路径（PostgreSQL 适配、分布式扩展） |
| `rust-reviewer` | 评估 P0/H1 修复方案的 Rust 最佳实践 |
| `qa-engineer` | 评估测试增强策略（覆盖率目标、测试分层） |

## 5. 待确认项

| # | 问题 | 状态 |
|---|------|------|
| 1 | P0 `fetch_and_lock` 修复是否立即执行？方案 A（原子 WriteLock）还是其他？ | **待决策** |
| 2 | H1 ParallelJoin 语义采用方案 A/B/C 中的哪一个？ | **待决策** |
| 3 | 短期（1-3 月）内项目的主要使用场景是什么？（内部研究/对外开源/商业化） | **待确认** |
| 4 | 测试覆盖率目标是否设定？（当前 23 tests，建议目标？） | **待确认** |
| 5 | PostgreSQL 适配器是否纳入短期规划？（当前只有 in-memory） | **待确认** |
| 6 | 是否需要 API 版本化策略？（当前 REST API 无版本） | **待确认** |

## 6. 企业治理（不适用）

本项目为开源 BPM 引擎，不涉及企业内控约束。

## 7. 领域技能包启用建议

| 技能 | 触发原因 |
|------|----------|
| `bpmn-flow-engine` | H1 ParallelJoin 语义决策需要 BPMN 规范依据 |
| `rust-review` | P0/H1 修复方案评审 |
| `doc-architecture` | 文档体系演进规划（docs/ vs design/ 合并策略） |

## 8. UI 范围

**不涉及 UI** — 本次为纯规划和架构决策。

## 9. 需求挑战会候选分组

建议分三组进行需求挑战：

**分组 A（P0 决策组）**：
- `tech-lead` + `rust-reviewer`
- 议题：fetch_and_lock 修复方案、ParallelJoin 语义决策
- 决策产出：P0 修复执行计划

**分组 B（测试质量组）**：
- `qa-engineer` + `tech-lead`
- 议题：测试覆盖率目标、测试分层策略（单元/集成/E2E）
- 决策产出：测试增强路线图

**分组 C（架构演进组）**：
- `architect` + `tech-lead`
- 议题：PostgreSQL 适配优先级、API 版本化策略、开源策略
- 决策产出：架构演进路线图

## 10. 当前阶段与下一步

- **当前阶段**：`intake`
- **目标阶段**：`/team-plan` — 产出分组规划（演进方向、优先级排序、backlog 快照）

## 11. 参考产出物

- [bpm-engine-review/prd.md](../2026-04-04-bpm-engine-review/prd.md) — 审查需求简报
- [bpm-engine-review/final-report.md](../2026-04-04-bpm-engine-review/final-report.md) — 审查最终报告
- [bpm-engine-review/closeout-summary.md](../2026-04-04-bpm-engine-review/closeout-summary.md) — 审查收口报告
- [ADR-001-fetch-and-lock-race.md](../../adr/ADR-001-fetch-and-lock-race.md) — P0 bug 决策
- [ADR-002-parallel-join-semantics.md](../../adr/ADR-002-parallel-join-semantics.md) — H1 设计 ADR
