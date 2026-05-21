# ADR-003: EngineContext Decoupling via Trait Object

## 决策信息

- **编号**: ADR-003
- **状态**: Accepted
- **日期**: 2026-05-21
- **Owner**: tech-lead
- **关联**: PRD engine-quality-hardening Q1

## 背景与约束

EngineContext 当前使用 `Option<Arc<dyn Trait>>` 持有各 store 引用。所有 handler 需要在运行时 unwrap 或判断 `if let Some(...)`，增加了 panic 风险和代码冗余。

需要在保持 handler Vec 可存异构对象的前提下消除 `Option`。

## 备选方案

| 方案 | 优点 | 风险 |
|------|------|------|
| **A: trait object（维持 `Arc<dyn T>`，移除 `Option`）** | 改动最小，handler 注册灵活 | 小量性能开销（vtable dispatch） |
| B: generic EngineContext<S: StorageBackend> | 零开销抽象 | handler Vec 无法存异构对象，API 复杂 |
| C: 超级 trait `StorageRepo` | 单一注入点 | 违反 ISP，适配器实现负担大 |

## 决策结果

**采用方案 A**：保持 `Arc<dyn Trait>` 模式，移除 `Option` 包装，增加 `EngineContextBuilder` 在构造时校验必填 store。

**原因**：
- 当前代码已在使用 trait object，改动量最小
- handler 注册保持灵活（不需要类型参数传播）
- builder 模式在编译期或构造期捕获缺失依赖

**影响范围**：`crates/runtime` 所有 handler + `crates/server/rest` + examples

## 后续动作

- backend-engineer 实现 EngineContextBuilder
- 移除所有 handler 中的 Option unwrap
