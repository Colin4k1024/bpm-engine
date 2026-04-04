# BPM Engine 架构演进路线图

**讨论组**: 架构演进组 (architect + tech-lead)
**日期**: 2026-04-04
**状态**: 已完成

---

## Q1 PostgreSQL 适配优先级

**结论：中期规划，非短期 P0**

**分析**：
1. `docs/database-schema.md` 已存在，schema 设计完整
2. `crates/storage` trait 层已抽象，PostgreSQL 适配器只需实现 traits
3. 但当前 P0/H1 问题尚未完全收口

**决策**：
- **短期（1-2 sprint）**：聚焦 P0/H1 决策和修复
- **中期（3-6 month）**：在 invariants 验证通过后启动 PostgreSQL 适配器
- **并行策略**：可同步进行 PostgreSQL 适配器的 schema 迁移工具开发

**依赖关系**：`P0/H1 修复完成 → 验证 invariants → PostgreSQL 适配器开发`

---

## Q2 API 版本化策略

**结论：当前已是 v1，引入版本化时机正确**

- 代码注释标注 `//! REST API v1`
- 当前版本化是 URL path 方式（`/api/v1/`），符合开源项目惯例
- **1.0 发布时**：引入 `/api/v2/` 时需同时保持 v1 至少 6 个月维护窗口

**待办**：
- 文档化 API contract（当前缺失）
- 补充 idempotency-key 实现

---

## Q3 legacy_engine.rs 移除

**结论：P3 技术债，短期内不需要移除**

- 可放在 3-6 month 规划中处理
- 建议在 `crates/runtime/src/engine.rs` 中添加注释说明 legacy_engine 的用途和移除计划
- 移除影响范围：`src/legacy_engine.rs` + `src/lib.rs` 中对应的 `pub mod legacy_engine`

---

## Q4 开源策略

**结论：代码基本 ready，但需先解决 H1 并补充文档**

**开源准备度**：~70%

**开源前必须解决**：

| 问题 | 状态 | 阻塞开源 |
|------|------|----------|
| P0 fetch_and_lock TOCTOU | 代码已修复 | 否 |
| H1 ParallelJoin group_id | ADR-002 proposed | **是** |
| API contract 文档 | 缺失 | **是** |
| 至少 1 个可运行 example | 已有多例 | 否 |

**许可证**：MIT 适合开源 BPM 引擎，当前选择正确

---

## 架构演进路线图

### 短期（1-2 sprint，4-8 周）

| 工作项 | 优先级 | Owner |
|--------|--------|-------|
| ADR-002 ParallelJoin 语义决策 | P0 | tech-lead |
| 补充 ParallelJoin 并发测试 | P1 | qa-engineer |
| 文档化 API contract | P1 | architect |
| 修复 H3 EL 表达式负数支持 | P1 | backend-engineer |
| 补充 doc-tests | P2 | backend-engineer |
| 清理 13 个 .bak 文件 | P3 | backend-engineer |

### 中期（3-6 个月）

| 工作项 | 优先级 | Owner |
|--------|--------|-------|
| PostgreSQL 适配器实现 | P1 | backend-engineer |
| Schema 迁移工具 | P2 | backend-engineer |
| 完善 observability | P2 | backend-engineer |
| Auth & multi-tenant | P3 | backend-engineer |
| 移除 `src/legacy_engine.rs` | P3 | backend-engineer |
| BPMN 测试集验证 | P2 | qa-engineer |

### 长期（6-12 个月）

| 工作项 | 优先级 |
|--------|--------|
| Dashboard / visualization | P2 |
| Python Worker SDK | P3 |
| Invariants tooling | P3 |
| 分布式/多节点支持 | Future |

---

## 关键路径

```
1. ADR-002 决策（P0 阻塞）
   ↓
2. 并发测试补全（P1）
   ↓
3. API contract 文档化（P1）
   ↓
4. PostgreSQL 适配器开发（P1）
   ↓
5. 开源发布（目标）
```

## 风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| H1 ParallelJoin 语义复杂需较大重构 | 高 | 方案 C 接受限制可快速收口 |
| PostgreSQL 适配器工作量超出预期 | 中 | 先实现核心 traits，逐步完善 |
| 过度工程化 | 中 | 聚焦核心问题，开源后根据社区反馈迭代 |
