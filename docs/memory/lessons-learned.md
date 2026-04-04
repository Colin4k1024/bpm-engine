# Lessons Learned

## 2026-04-04 — bpm-engine-review

| 场景 | 问题 | 建议 |
|------|------|------|
| 审查范围过大 | 单次审查覆盖全项目导致报告碎片化 | 下次先定义深度覆盖 vs 广度扫描的边界 |
| 并行分组结构 | 3 组并行（arch/BPMN/quality）高效收敛 | 适合复杂多维度审查任务 |
| ADR 必要性 | P0 和 H1 问题需要独立跟踪，不适合混入审查报告 | 重要架构决策创建 ADR |
| 只读审查约束 | 用户明确要求不改代码，简化了审查范围 | 重大重构建议先完成审查再进入实现 |

## 2026-04-04 — bpm-engine-evolution-plan

| 场景 | 问题 | 建议 |
|------|------|------|
| Sprint 2 ParallelJoin 测试 | 测试期望新方案 B 行为，但实现尚未修改 | 今后实现任务应先于测试任务，或测试用 OLD 逻辑验证 |
| Crash recovery 测试 | 时间相关测试（lock expiry）需要足够等待时间 | 测试中 sleep 时长必须大于 lock duration |
| PostgreSQL 适配器 | sqlx 与 rusqlite 冲突 | 选择 tokio-postgres + deadpool-postgres 避免依赖冲突 |
| 并发测试价值 | fetch_and_lock 并发测试验证了 ADR-001 修复有效性 | 关键 invariant 应优先补充并发测试 |
