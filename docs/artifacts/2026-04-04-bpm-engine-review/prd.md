---
artifact: prd
task: bpm-engine-review
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 项目审查 — 需求简报

## 1. 背景

对 `bpm-engine` 项目进行全方面审查，识别架构一致性、代码质量、测试覆盖、文档完整性、技术债等方面的现状与问题。

**项目性质：** Rust 实现的自研 BPM 引擎，token 驱动，持久化优先，支持外部任务、Saga 补偿、崩溃恢复。

## 2. 目标与优先级

| 优先级 | 目标 |
|--------|------|
| P0 | 审查核心架构与 `crates/core`、`crates/runtime`、`crates/storage` 的设计一致性 |
| P0 | 审查 `crates/bpmn` BPMN 2.0 解析器与规范映射的正确性 |
| P0 | 审查测试覆盖率和 invariant 保护完整性 |
| P1 | 审查文档（`docs/`）与代码实现的一致性，识别过时或缺失文档 |
| P1 | 审查技术债：`.bak` 残留文件、重复文档、过时示例 |
| P2 | 审查 `crates/worker-sdk` 与 REST server 的 API 契约 |
| P2 | 审查 CI/CD 流程和构建质量 |

**成功标准：** 产出结构化审查报告，列出：Critical/High 问题清单、建议优先级排序、不改代码前提下的改进方向。

## 3. 关键约束

- **不改前后端代码** — 只读审查，不提交任何代码修改
- 不修改 CI/CD 配置
- 不修改任何 crate 的 `Cargo.toml` 依赖

## 4. 参与角色清单

| 角色 | 主责 |
|------|------|
| `tech-lead` | 统筹审查范围、仲裁方案、收口结论 |
| `rust-reviewer` | 深度审查 Rust 代码质量、所有权模式、错误处理 |
| `code-reviewer` | 跨 crate 一致性、模块边界、依赖方向 |
| `bpmn-flow-engine` (skill) | BPMN 规范映射、token 语义、invariant 验证 |
| `qa-engineer` | 测试覆盖率分析、测试质量评估 |

## 5. 待确认项

| # | 问题 | 状态 |
|---|------|------|
| 1 | 测试覆盖率目标是否设定（当前约 23 个测试，分布在 5 个 crate）？ | 待确认 |
| 2 | 是否需要审查 `design/` 目录与 `docs/` 目录的重复文档合并策略？ | 待确认 |
| 3 | `.bak` 文件是否需要清理（13 个 .bak 散落在 `tests/` 和 `examples/`）？ | 待确认 |
| 4 | `crates/worker-sdk` 的 Python SDK 计划（`docs/sdk-python.md`）是否仍有效？ | 待确认 |
| 5 | `docs/artifacts/` 是否需要建立项目审查结果的持久化入口？ | 待确认 |

## 6. 企业治理（不适用）

本项目为开源 BPM 引擎，不涉及：
- 企业内部应用等级（T1-T4）
- 集团组件约束
- 数据/合规风险
- 跨境数据传输

## 7. 领域技能包启用建议

| 技能 | 触发原因 |
|------|----------|
| `bpmn-flow-engine` | BPMN 规范映射审查、token 生命周期验证 |
| `rust-review` | Rust 所有权、生命周期、错误处理模式审查 |
| `code-reviewer` | 跨 crate 依赖方向、模块边界、API 契约审查 |
| `doc-architecture` | `docs/` vs `design/` 文档结构审查与合并建议 |

## 8. UI 范围

**不涉及 UI** — 本次为纯代码和文档审查。

## 9. 需求挑战会候选分组

由于本任务为审查而非功能实现，建议跳过需求挑战会，直接进入 `/team-plan` 阶段。

**如需挑战会，分组建议：**
- **分组 A（架构）**：`architect` + `rust-reviewer` — 审查 `crates/core` 和 `crates/runtime` 的设计一致性
- **分组 B（BPMN）**：`bpmn-flow-engine` + `code-reviewer` — 审查 BPMN 解析和 token 语义
- **分组 C（质量）**：`qa-engineer` + `rust-reviewer` — 测试覆盖率和技术债

## 10. 当前阶段与下一步

- **当前阶段：** `intake`
- **目标阶段：** `/team-plan` — 产出分组的审查执行计划
- **阻塞项：** 无硬阻塞

## 11. CI/CD 现状（参考）

```
cargo fmt      ✓ 通过
cargo clippy   ✓ 通过（-D warnings）
cargo test     ✓ 通过（23 tests，0 failed）
```
