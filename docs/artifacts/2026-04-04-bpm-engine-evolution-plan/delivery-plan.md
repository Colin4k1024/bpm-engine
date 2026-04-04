---
artifact: delivery-plan
task: bpm-engine-evolution-plan
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 演进规划 — 交付计划

## 1. 版本目标与范围

**本次规划范围**：bpm-engine 项目在审查后（bpm-engine-review）的演进方向与实施计划。

**版本目标**：
- 短期（1-2 sprint）：消除 P0/H1/H3 风险，建立稳定基线
- 中期（3-6 month）：达到生产可用性门槛（70% 覆盖 + PostgreSQL 适配）
- 长期（6-12 month）：生态建设，准备开源发布

**放行标准**：
- P0/H1/H3 问题有明确修复方案并通过测试验证
- 覆盖率 >= 50%（短期）/ >= 70%（中期）
- API contract 文档化
- 无 blocking issue 阻塞开源

---

## 2. 需求挑战会结论

### 核心质疑与收敛

| # | 质疑 | 质疑目标 | 结论 |
|---|------|----------|------|
| 1 | P0-2 ParallelJoin 方案 B 的 `fork_instance_counter` 在 crash recovery 后是否连续？ | arch-evolution-roadmap | 需要在 PostgreSQL 适配时确保 fork_counter 持久化 |
| 2 | 方案 B 的 `fork_counter` 是否需要全局唯一性保证？ | p0-decisions | 是，但仅在同一 process instance 内唯一即可 |
| 3 | 短期 Sprint 1 是否应并行处理多个工作项？ | test-enhancement-roadmap | 是，Sprint 1 可并行（测试和修复可分离） |
| 4 | PostgreSQL 适配器是否需要先完成 API contract 文档化？ | arch-evolution-roadmap | 否，但 API contract 文档化应在 Sprint 1 完成 |

### 未收敛项

| # | 未决问题 | 阻塞项 | 负责人 |
|---|----------|--------|--------|
| 1 | ADR-002 ParallelJoin 方案 B 的 crash recovery 细节 | Sprint 2 开始前 | tech-lead |
| 2 | Sprint 1 具体时间box（2 周还是 4 周） | 资源确认 | tech-lead |

---

## 3. 角色分工

| 角色 | 主责工作项 |
|------|-----------|
| `tech-lead` | ADR-002 最终决策、Sprint 里程碑验收、升级仲裁 |
| `backend-engineer` | H3 EL 表达式修复、Sprint 1/2 实现工作、PostgreSQL 适配器开发 |
| `qa-engineer` | Sprint 1-2 测试补强、覆盖率工具集成、测试策略执行 |
| `architect` | API contract 文档化、架构设计评审 |
| `rust-reviewer` | P0-2 方案 B 实现评审、代码审查 |

---

## 4. 工作拆解

### Sprint 1（2 周，~15h）

| # | 工作项 | 优先级 | 类型 | Owner | 依赖 | 产出 |
|---|--------|--------|------|-------|------|------|
| 1 | ADR-002 决策落盘（方案 B） | P0 | 决策 | tech-lead | — | ADR-002 updated |
| 2 | fetch_and_lock 并发测试 (N=16) | P0 | 集成测试 | qa-engineer | ADR-002 | `tests/concurrent_fetch_and_lock.rs` |
| 3 | try_join expected=0/1 边界测试 | H3 | 单元测试 | qa-engineer | — | 2 个新测试 |
| 4 | EL 表达式负数解析修复 | H3 | 实现 | backend-engineer | — | `crates/runtime/src/el.rs` 修复 |
| 5 | API contract 文档化 | P1 | 文档 | architect | — | `docs/artifacts/{slug}/api-contract.md` |

### Sprint 2（2 周，~11h）

| # | 工作项 | 优先级 | 类型 | Owner | 依赖 | 产出 |
|---|--------|--------|------|-------|------|------|
| 6 | ParallelJoin 方案 B 实现 | P0 | 实现 | backend-engineer | ADR-002 决策 | `token_arrived_handler.rs` 修改 |
| 7 | ParallelJoin 语义测试（4 场景） | H1 | 集成测试 | qa-engineer | #6 | 4 个新测试 |
| 8 | Token 状态机单元测试 (core) | M | 单元测试 | qa-engineer | — | `crates/core/src/` 测试模块 |
| 9 | Saga 补偿顺序测试 | M | 单元测试 | qa-engineer | — | 1 个新测试 |

### Sprint 3-4（4-8 周，中期目标）

| # | 工作项 | 优先级 | 类型 | Owner | 依赖 |
|---|--------|--------|------|-------|------|
| 10 | Crash recovery + outbox 测试 | P1 | 集成 | qa-engineer | Sprint 2 完成 |
| 11 | External task 多 worker 竞争测试 | P1 | 集成 | qa-engineer | Sprint 1 完成 |
| 12 | PostgreSQL 适配器开发 | P1 | 实现 | backend-engineer | P0/H1 验证通过 |
| 13 | BPMN 测试集验证 | P2 | 测试 | qa-engineer | — |
| 14 | Doc-tests 启用 (storage + runtime) | P2 | 文档测试 | backend-engineer | — |
| 15 | Token exactly-once 幂等性测试 | M | 集成 | qa-engineer | — |
| 16 | Timer 持久化 + 重启恢复测试 | M | 集成 | qa-engineer | — |

### Sprint 5+（长期）

| # | 工作项 | 优先级 | 类型 |
|---|--------|--------|------|
| 17 | E2E smoke + chaos 测试 | P2 | E2E |
| 18 | Dashboard / visualization | P2 | 前端 |
| 19 | Python Worker SDK | P3 | SDK |
| 20 | API contract tests (REST) | P2 | 集成 |

---

## 5. 关键路径

```
Sprint 1        Sprint 2        Sprint 3-4       Sprint 5+
───────────────────────────────────────────────────────────►
[ADR-002 决策]──►[ParallelJoin B 实现]──►[PostgreSQL]──►[开源发布]
      │                                        │
      ├─[并发测试 P0]───────────────────────────┤
      ├─[EL 负数修复 H3]───────────────────────┤
      ├─[API contract 文档 P1]─────────────────┤
      └─[try_join 边界测试 H3]─────────────────┘

关键里程碑:
Sprint 1 末: P0/H3 消除，覆盖率 23 → 30%
Sprint 2 末: H1 消除，覆盖率 30 → 45%
Sprint 4 末: 70% 覆盖，PostgreSQL adapter 完成
Sprint 5+: 80%+ 覆盖，开源发布
```

---

## 6. 风险与缓解

| 风险 | 影响 | 概率 | 缓解措施 | Owner |
|------|------|------|----------|-------|
| P0-2 ParallelJoin 方案 B 实现复杂度超预期 | 高 | 中 | Sprint 1 结束时 review，如果复杂改用方案 C | tech-lead |
| PostgreSQL 适配器工作量超出预期 | 中 | 高 | 先实现核心 traits (TokenStore, ProcessStore)，逐步完善 | backend-engineer |
| 测试覆盖率目标未达成 | 中 | 低 | cargo-llvm-cov 门禁强制执行 | qa-engineer |
| Sprint 资源不足（2 人） | 中 | 中 | 聚焦 P0/H1，延后 P2/M | tech-lead |

---

## 7. 检查节点

| 节点 | 时间 | 验收标准 | 通过条件 |
|------|------|----------|----------|
| Sprint 1 Checkpoint | 第 2 周末 | ADR-002 决策、fetch_and_lock 并发测试通过、EL 修复通过 | tech-lead 签字 |
| Sprint 2 Checkpoint | 第 4 周末 | ParallelJoin 方案 B 实现、4 个语义测试通过 | tech-lead 签字 |
| Mid-term Review | Sprint 4 末 | 覆盖率 >= 50%、PostgreSQL adapter 核心 traits 完成 | tech-lead + architect |
| Long-term Review | Sprint 5+ | 覆盖率 >= 70%、开源准备就绪 | tech-lead |

---

## 8. 依赖清单

| 依赖项 | 类型 | 来源 | 影响 |
|--------|------|------|------|
| Rust toolchain (stable) | 环境 | CI | 无 |
| tokio (multi_thread) | 测试依赖 | Sprint 1 | 无 |
| cargo-llvm-cov | 覆盖率工具 | Sprint 1 | CI 需集成 |
| PostgreSQL schema | 文档 | `docs/database-schema.md` | 无变更 |
| ADR-001 | 决策 | 已完成 | 无 |
| ADR-002 | 决策 | Sprint 1 | **阻塞 Sprint 2** |

---

## 9. 技能装配清单

| 技能 | 触发场景 | 用途 |
|------|----------|------|
| `bpmn-flow-engine` | P0-2 ParallelJoin 实现 | BPMN 规范映射验证 |
| `rust-review` | 方案 B 实现评审 | 所有权/生命周期审查 |
| `rust-testing` | 测试增强执行 | 单元/集成测试设计 |
| `doc-architecture` | API contract 文档化 | 文档结构规划 |
| `code-reviewer` | Sprint 1/2 代码合并前 | 质量门禁 |

---

## 10. ADR 更新需求

| ADR | 当前状态 | 需更新内容 |
|-----|----------|-----------|
| ADR-001 | accepted | 更新为 **implemented**，补充并发测试验证结果 |
| ADR-002 | proposed | 更新为 **accepted（方案 B）**，补充实现要点和文档更新计划 |

---

## 11. 下一步动作

| # | 动作 | 负责人 | 截止时间 |
|---|------|--------|----------|
| 1 | 更新 ADR-002 状态为 accepted（方案 B） | tech-lead | Sprint 1 开始前 |
| 2 | 集成 cargo-llvm-cov 到 CI | devops/backend | Sprint 1 开始前 |
| 3 | Sprint 1 kickoff（并行启动 #1-#5） | tech-lead | 下周 |
| 4 | 创建 backlog snapshot | tech-lead | Sprint 1 结束后 |

---

## 12. 产出物清单

| 文件 | 说明 | 状态 |
|------|------|------|
| `prd.md` | 需求简报 | ✅ 已完成 |
| `p0-decisions.md` | P0 决策结论 | ✅ 已完成 |
| `arch-evolution-roadmap.md` | 架构演进路线图 | ✅ 已完成 |
| `test-enhancement-roadmap.md` | 测试增强路线图 | ✅ 已完成 |
| `delivery-plan.md` | 本文件 | ✅ 已完成 |
| `api-contract.md` | API 契约文档 | 📋 Sprint 1 待产出 |
| ADR-001 | P0 bug 决策 | 📋 更新状态 |
| ADR-002 | P1 设计决策 | 📋 更新状态为 accepted（方案 B） |
