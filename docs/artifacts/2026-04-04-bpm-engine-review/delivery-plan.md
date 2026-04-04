---
artifact: delivery-plan
task: bpm-engine-review
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 项目审查 — 交付计划

## 1. 需求挑战会结论

### 质疑汇总

**分组 A（架构）— `arch-challenger`**

| # | 核心质疑 | 替代路径 | 阻断条件 | 结论 |
|---|----------|----------|----------|------|
| A1 | TokenArrivedHandler 的 in-memory join_state 是无害 fallback？ | 测试 crash-restart + parallel join 恢复 | crash 后 parallel fork-join 可能永远 hung | **未决** — 需要并发 crash recovery 测试 |
| A2 | EventPump handler 遍历顺序不影响正确性？ | 验证 HistoryHandler 记录时机 | 无法保证 history 与 instance 状态的 atomic transaction | **未决** — 需要确认 handler 顺序约束文档化 |
| A3 | async-trait 在存储层无毒性问题？ | 基准测试 async-trait vs native async fn | 性能瓶颈无法在设计阶段发现 | **接受** — 当前用 2021 edition，async-trait 0.1 可接受 |

**分组 B（BPMN）— `bpmn-challenger`**

| # | 核心质疑 | 替代路径 | 阻断条件 | 结论 |
|---|----------|----------|----------|------|
| B1 | ParallelGateway fork/join 角色在编译期静态确定，与 BPMN 语义一致？ | 测试 inc==1 && out==1 的 ParallelGateway 运行时行为 | 并行分支同步语义完全失效 | **未决** — 需要 runtime join 行为测试 |
| B2 | parallel_group_id 在 ParallelJoin 处语义验证充分？ | 测试多 fork 输出汇聚到同一 join | join 过早/过迟触发 | **未决** — 需要多 fork 交汇场景测试 |
| B3 | docs/bpmn-spec-mapping.md 与代码一致？ | 确认 NodeType::ServiceTask 是否死代码 | ServiceTask 已映射为 ExternalTask，文档误导 | **确认** — 文档需更新，ServiceTask 变体为死代码 |

**分组 C（质量）— `qa-challenger`**

| # | 核心质疑 | 替代路径 | 阻断条件 | 结论 |
|---|----------|----------|----------|------|
| C1 | 23 tests 足够覆盖三大 invariant？ | llvm-cov 分支覆盖 + 并发 fetch_and_lock 测试 | fetch_and_lock TOCTOU 竞态存在 | **确认存在 bug** — P0 |
| C2 | integration_recovery.rs 覆盖了崩溃恢复？ | 模拟 kill -9 + 幂等性测试 | 幽灵 token 导致重复执行 | **未决** — 需要真正的 crash simulation |
| C3 | 0 doc-tests 可接受？ | 为核心 API 补 doc-examples | .bak 文件使用已废弃 API，误导新人 | **确认** — 需要清理 .bak 和补 doc-tests |

### 跨分组阻断性问题

1. **P0 — fetch_and_lock TOCTOU 竞态**（A1 + C1）
   - `MemoryRepo::fetch_and_lock` 的 select(ReadLock) 和 lock(WriteLock) 非原子
   - 多 worker 同时调用时，同一 task 可能被两个 worker 获得
   - 影响：外部任务单一 owner invariant 被违背

2. **P1 — parallel join 的 group_id 语义混淆**（B2）
   - 多个 ParallelFork 的 token 可能汇聚到同一 join，无法区分来源
   - `expected` = 所有 incoming flows 总数，而非单个 fork 的分支数

3. **P1 — in-memory fallback 的 parallel join 状态丢失**（A1）
   - TokenArrivedHandler 的 join_state 在 crash 后无法恢复
   - 与 "Persistence over memory" 承诺矛盾

4. **P2 — .bak 文件使用已废弃 API**（C3）
   - `integration_saga.rs.bak`、`integration_outbox.rs.bak` 引用不存在的 `bpm_engine::persistence`
   - `NodeType::ServiceTask(fn(...))` 从未被赋值，是死代码

---

## 2. 交付范围

### 审查范围（不变）

| 优先级 | 目标 | 状态 |
|--------|------|------|
| P0 | 核心架构设计一致性审查 | 进行中 |
| P0 | BPMN 2.0 解析器正确性审查 | 进行中 |
| P0 | invariant 保护完整性审查 | 进行中 |
| P1 | 文档与代码一致性审查 | 进行中 |
| P1 | 技术债识别（.bak 文件、重复文档） | 进行中 |
| P2 | API 契约审查 | 待开始 |
| P2 | CI/CD 流程审查 | 待开始 |

### 不在范围内

- 修改前后端代码
- 修改 CI/CD 配置
- 修改 Cargo.toml 依赖

---

## 3. 审查执行计划

### 阶段 1：产出结构化审查报告

| 工作项 | 主责角色 | 输出 |
|--------|----------|------|
| 架构一致性报告（Critical/High 问题清单） | `rust-reviewer` + `code-reviewer` | `review-report.md` |
| BPMN 解析和 token 语义报告 | `bpmn-flow-engine` skill | `bpmn-review.md` |
| 测试覆盖率和质量报告 | `qa-engineer` | `quality-report.md` |
| 技术债清单（.bak、重复文档、死代码） | `code-reviewer` | `tech-debt.md` |

### 阶段 2：综合汇总

| 工作项 | 主责角色 | 输出 |
|--------|----------|------|
| 最终审查报告（含优先级排序） | `tech-lead` | `final-report.md` |
| ADR（需要记录的架构决策） | `architect` | `docs/adr/` |

---

## 4. 风险与依赖

| 风险 | 影响 | 缓解措施 | Owner |
|------|------|----------|-------|
| fetch_and_lock TOCTOU 是真实 bug | P0 数据一致性 | 识别后记录为 P0，在 review report 中标记 | tech-lead |
| parallel join 语义在复杂拓扑下错误 | P1 正确性 | 补并发测试覆盖 | qa-engineer |
| .bak 文件误导后续开发者 | P2 文档 | 建议直接删除，不保留注释式代码 | tech-lead |
| 文档与代码不一致 | P1 文档质量 | 更新 bpmn-spec-mapping.md | code-reviewer |

---

## 5. 应用等级 / 技术架构等级（不适用）

开源 BPM 引擎项目，不涉及企业内部应用等级、T1-T4 约束、集团组件合规。

---

## 6. ADR 需求

| 是否需要 | 主题 | 状态 |
|----------|-------|------|
| **需要** | ParallelJoin group_id 语义澄清：当前实现是否满足 BPMN 规范？ | 待创建 |
| **需要** | ExternalTask fetch_and_lock 竞态：接受当前 MemoryRepo 实现还是需要修复？ | 待创建 |
| **建议** | async-trait vs native async fn in traits 的未来迁移规划 | 待讨论 |

---

## 7. 技能装配清单

| 技能 | 用途 | 状态 |
|------|------|------|
| `bpmn-flow-engine` | BPMN 规范映射、token 语义质疑 | 已启用 |
| `rust-review` | Rust 所有权、生命周期、错误处理审查 | 已启用 |
| `code-reviewer` | 跨 crate 依赖方向、模块边界审查 | 已启用 |
| `qa-engineer` | 测试覆盖率分析 | 已启用 |

---

## 8. 前端交付物（不涉及）

本次为纯代码和文档审查，无 UI 变更。

---

## 9. 当前阶段与下一步

- **当前阶段：** `/team-plan` 完成，需求挑战会结论已落盘
- **目标阶段：** `/team-execute` — 执行分组审查，产出 `review-report.md`、`bpmn-review.md`、`quality-report.md`、`tech-debt.md`
- **阻塞项：** 无硬阻塞

---

## 10. 审查分组执行命令

```bash
# 架构和 Rust 质量审查
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | grep -E "error|warning" | head -50

# 测试覆盖率
cargo llvm-cov --workspace --html 2>&1 | tail -20

# BPMN 解析正确性（运行集成测试）
cargo test --package bpm_engine_bpmn -- --nocapture
```
