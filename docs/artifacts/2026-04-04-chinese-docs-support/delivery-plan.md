---
artifact: delivery-plan
task: chinese-docs-support
date: 2026-04-04
role: tech-lead
status: draft
---

# BPM Engine 中文文档支持 — 交付计划

## 1. 版本目标与范围

**本次规划范围**：中文文档体系建设，覆盖 README、API 参考、设计文档。

**版本目标**：
- P0：中文 README + Quick Start 指南
- P1：中文 API 参考（关键部分）
- P2：中文设计文档（BPMN 用户指南）

**放行标准**：
- 中文 README 覆盖全部英文 README 内容
- 关键 API 有中文说明
- 文档结构清晰，中英文可对照

---

## 2. 工作拆解

### Phase 1：中文快速开始（P0）

| # | 工作项 | 产出 | 依赖 |
|---|--------|------|------|
| 1 | 翻译 README → README_zh.md | 完整中文版 README | — |
| 2 | 编写 quick-start_zh.md | 5 分钟快速开始示例 | — |
| 3 | 翻译 CLAUDE.md → CLAUDE_zh.md | 中文项目说明 | — |

### Phase 2：中文 API 参考（P1）

| # | 工作项 | 产出 | 依赖 |
|---|--------|------|------|
| 4 | 翻译 api-contract.md → api-reference_zh.md | API 中文参考 | Phase 1 完成 |
| 5 | 翻译 ADR → ADR_zh.md | 决策记录中文版 | Phase 1 完成 |

### Phase 3：中文设计文档（P2）

| # | 工作项 | 产出 | 依赖 |
|---|--------|------|------|
| 6 | 翻译 architecture.md → architecture_zh.md | 架构设计文档 | Phase 2 完成 |
| 7 | 编写 bpmn-guide_zh.md | BPMN 用户指南 | Phase 2 完成 |

---

## 3. 文档同步机制

**方案**：基于文件名的双语对照

| 英文文档 | 中文文档 | 同步方式 |
|----------|----------|----------|
| README.md | README_zh.md | 同步修改 |
| CLAUDE.md | CLAUDE_zh.md | 同步修改 |
| docs/architecture.md | docs/architecture_zh.md | 同步修改 |
| docs/artifacts/.../api-contract.md | docs/artifacts/.../api-reference_zh.md | 同步修改 |

**CI 验证**：添加 `ci_docs_sync_check` 脚本，检测英文文档修改时提醒对应中文文档需同步。

---

## 4. 角色分工

| 角色 | 主责 |
|------|------|
| `tech-lead` | 审核文档范围和质量 |
| `doc-writer` | 中文翻译和文档编写 |

---

## 5. 检查节点

| 节点 | 验收标准 |
|------|----------|
| Phase 1 | README_zh.md + quick-start_zh.md 可读性检查 |
| Phase 2 | API 参考完整性（关键端点全覆盖） |
| Phase 3 | 设计文档与代码一致性 |

---

## 6. 产出物清单

| 文件 | 说明 |
|------|------|
| `README_zh.md` | 中文 README |
| `CLAUDE_zh.md` | 中文 CLAUDE |
| `docs/quick-start_zh.md` | 快速开始指南 |
| `docs/architecture_zh.md` | 架构文档 |
| `docs/api-reference_zh.md` | API 参考 |
| `docs/bpmn-guide_zh.md` | BPMN 用户指南 |
