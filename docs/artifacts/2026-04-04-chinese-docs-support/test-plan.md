---
artifact: test-plan
task: chinese-docs-support
date: 2026-04-04
role: qa-engineer
status: draft
---

# BPM Engine 中文文档支持 — 测试计划

## 评审范围

本次为文档评审任务，无代码变更。

## 文档评审矩阵

| 文档 | 英文源 | 中文版 | 评审要点 | 状态 |
|------|--------|--------|----------|------|
| README_zh.md | README.md | README_zh.md | 内容完整性、结构一致性、技术术语保留 | ✅ |
| CLAUDE_zh.md | CLAUDE.md | CLAUDE_zh.md | 命令格式保留、路径准确性 | ✅ |
| quick-start_zh.md | README.md (快速开始章节) | docs/quick-start_zh.md | 示例可运行性、API 准确性 | ✅ |
| api-reference_zh.md | api-contract.md | docs/api-reference_zh.md | 端点完整性、JSON 示例准确性 | ✅ |
| architecture_zh.md | architecture.md | docs/architecture_zh.md | 架构概念准确性、引用链接 | ✅ |
| bpmn-guide_zh.md | bpmn-spec-mapping.md | docs/bpmn-guide_zh.md | BPMN 映射准确性、示例完整性 | ✅ |

## 评审检查项

### 1. 内容完整性检查

- [x] README_zh.md：345行，完整覆盖英文 README 所有章节
- [x] CLAUDE_zh.md：111行，完整覆盖英文 CLAUDE.md 所有章节
- [x] api-reference_zh.md：16个 API 端点全部翻译
- [x] 快速开始指南包含完整运行示例

### 2. 技术术语一致性

- [x] Token、Engine、Worker、Lease 等核心术语保留英文
- [x] API 端点路径、HTTP 方法保留英文
- [x] JSON 字段名和结构保留英文
- [x] 文件路径和代码示例保留原始格式

### 3. 文档结构对应

- [x] 中文文档与英文文档结构一一对应
- [x] 便于后续同步更新
- [x] 链接指向正确的英文/中文文档

### 4. 可读性检查

- [x] 中文表达流畅专业
- [x] 技术概念解释清晰
- [x] 代码示例格式正确

## 风险评估

| 风险 | 影响 | 级别 | 缓解措施 |
|------|------|------|----------|
| 英文文档更新时中文未同步 | 低 | 低 | 已建立文档同步机制记录 |
| 技术术语翻译不一致 | 低 | 低 | 统一保留核心术语英文 |

## 评审结论

**通过** — 所有文档内容完整、技术术语一致、结构对应清晰。
