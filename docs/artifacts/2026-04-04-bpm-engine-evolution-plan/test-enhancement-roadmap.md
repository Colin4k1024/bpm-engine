# BPM Engine 测试增强路线图

**讨论组**: qa-engineer + tech-lead
**日期**: 2026-04-04
**状态**: 已完成

---

## 当前差距分析

### 测试分布现状 (23 tests)

| 层级 | 测试数 | 覆盖内容 | 缺口 |
|------|--------|----------|------|
| BPMN 解析/编译 | 14 | 基础语法验证 | 仅 happy path，无 BPMN 语义校验 |
| Memory 适配器 | 3 | external_task 基本流程 | 无并发、无边界 |
| 集成测试 | 6 | token claim、join、外部任务 | fetch_and_lock 无并发、join 无 group_id 语义 |
| Doc-tests | 0 | - | 所有 crate 未启用 |
| 单元测试 (core) | 0 | Token 状态机、Saga | 完全缺失 |

### 关键风险对照

| 级别 | 问题 | 当前覆盖 | 缺口 |
|------|------|----------|------|
| **P0** | fetch_and_lock TOCTOU | 串行测试有，并发测试无 | ADR-001 已修复，**无并发验证** |
| **H1** | ParallelJoin group_id 语义 | 2 个基础测试 | 无多 fork 汇聚、死锁场景 |
| **H3** | EL 表达式负数解析 | 0 | 完全未覆盖 |
| M | try_join expected=0/1 | 0 | 边界条件缺失 |
| M | Token 状态机 | 0 | core crate 无单元测试 |
| L | Saga 补偿顺序 | 0 | 仅文档，无实现验证 |

---

## 测试分层建议

```
┌─────────────────────────────────────┐
│  Chaos/E2E (1-5%)                   │
│  - 随机 kill、完整流程端到端          │
└─────────────────────────────────────┘
          ▲
┌─────────────────────────────────────┐
│  Integration Tests (30-40%)         │
│  - Token claim 并发                  │
│  - ParallelJoin group_id 语义        │
│  - ExternalTask fetch_and_lock 并发  │
│  - Crash recovery + outbox          │
└─────────────────────────────────────┘
          ▲
┌─────────────────────────────────────┐
│  Unit Tests (60-70%)                │
│  - Token 状态机 (core)              │
│  - Saga 补偿顺序                    │
│  - EL 表达式解析                    │
│  - try_join 边界 (expected=0/1)     │
└─────────────────────────────────────┘
```

**理由**:
- **core** 是纯逻辑，最适合单元测试
- **runtime** 依赖 storage traits，适合集成测试
- 参考: Camunda/Flowable ~65-70% 覆盖率

---

## 高优先级补强项

### P0: fetch_and_lock 并发测试

```rust
// N=16 worker 同时 fetch 同一 task_type，验证同一 task 永远只被一个 worker 获得
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_fetch_and_lock_guarantees_single_owner() {
    let repo = Arc::new(MemoryRepo::new());
    for i in 0..10 {
        repo.create(&format!("token-{}", i), "inst", "payment", 3, 60, HashMap::new()).await.unwrap();
    }
    let n = 16;
    let mut handles = Vec::new();
    for wid in 0..n {
        let repo = Arc::clone(&repo);
        handles.push(tokio::spawn(async move {
            repo.fetch_and_lock(&format!("worker-{}", wid), &["payment".to_string()], 10, Duration::from_secs(30)).await.unwrap()
        }));
    }
    let results: Vec<Vec<ExternalTask>> = futures::future::join_all(handles).await.into_iter().map(|r| r.unwrap()).collect();
    // 验证: 每个 task 最多被一个 worker 获得
    ...
}
```

### H1: ParallelJoin group_id 语义测试场景

| 场景 | 描述 | 预期行为 |
|------|------|----------|
| 单 fork | 3 分支 → join → 1 输出 | join 正确汇聚 |
| 多 fork 汇聚 | fork1(2分支) + fork2(2分支) → 同一 join | 待 ADR-002 决策 |
| 乱序到达 | 分支 2,3,1 顺序到达 join | 第3个分支到达时触发 |
| group_id 冲突 | 两个不同 fork 产生相同 group_id | 应分别计数 |

### H3: try_join 边界测试

```rust
// expected=0 时，第一次 try_join 就应返回 true
// expected=1 时，第一次 try_join 就应返回 true
// 多个并发任务同时 try_join，验证计数正确
```

---

## 覆盖率目标建议

| 阶段 | 行覆盖率目标 | 新增测试 | 累计 |
|------|-------------|----------|------|
| 短期 (1-2 sprint) | 50% | +20-25 | ~45-50 |
| 中期 (3-4 sprint) | 70% | +30-35 | ~75-85 |
| 长期 | 80%+ | +20 | ~95-105 |

### 开源 BPM 引擎参考

| 项目 | 覆盖率 | 测试策略 |
|------|--------|----------|
| Camunda (Java) | ~70% | 单元 + 集成 + E2E |
| Flowable (Java) | ~65% | 单元 + BPMN 语义 |
| Zeebe (Go) | ~75% | 单元 + 状态机测试 |

---

## 测试增强路线图

### 短期 (Sprint 1-2, ~2-4 周)

#### Sprint 1: P0 并发 + H3 边界

| # | 测试 | 类型 | 优先级 | 工作量 |
|---|------|------|--------|--------|
| 1 | fetch_and_lock 并发测试 (N=16) | 集成 | P0 | 2h |
| 2 | try_join expected=0/1 边界 | 单元 | H3 | 1h |
| 3 | EL 表达式负数解析 | 单元 | H3 | 2h |
| 4 | Token 状态机单元测试 (core) | 单元 | M | 4h |

#### Sprint 2: H1 ParallelJoin 语义

| # | 测试 | 类型 | 优先级 | 工作量 |
|---|------|------|--------|--------|
| 5 | ParallelJoin 单 fork 汇聚 | 集成 | H1 | 2h |
| 6 | ParallelJoin 乱序到达 | 集成 | H1 | 1h |
| 7 | ParallelJoin group_id 冲突 | 集成 | H1 | 3h |
| 8 | Saga 补偿顺序 (A→B→C 失败) | 单元 | M | 3h |

**短期交付**: 8 个新测试，覆盖率 23 → 45%

---

### 中期 (Sprint 3-4, ~4-8 周)

#### Sprint 3: 恢复 + 外部任务扩展

| # | 测试 | 类型 | 工作量 |
|---|------|------|--------|
| 9 | Crash during token execution + recovery | 集成 | 4h |
| 10 | Outbox 消息发布 + replay | 集成 | 4h |
| 11 | External task lease expiry + reclaim | 集成 | 2h |
| 12 | External task 多 worker 竞争 | 集成 | 3h |
| 13 | BPMN 并行网关编译语义 | 单元 | 3h |
| 14 | BPMN 顺序流条件表达式 | 单元 | 2h |

#### Sprint 4: Token + Saga 深化

| # | 测试 | 类型 | 工作量 |
|---|------|------|--------|
| 15 | Token exactly-once 幂等性 | 集成 | 3h |
| 16 | Token InvalidTransition 拒绝 | 单元 | 2h |
| 17 | Parallel saga 补偿 | 单元 | 3h |
| 18 | Timer 持久化 + 重启恢复 | 集成 | 4h |
| 19 | Doc-tests 启用 (storage traits) | Doc | 2h |
| 20 | Doc-tests 启用 (runtime API) | Doc | 2h |

**中期交付**: 覆盖率 45 → 70%

---

### 长期 (Sprint 5+, ~8+ 周)

| # | 测试 | 类型 | 工作量 |
|---|------|------|--------|
| 21 | E2E 完整流程 smoke | E2E | 3h |
| 22 | Random kill + restart chaos | Chaos | 4h |
| 23 | 并行 fork + join BPMN 压力 | 集成 | 3h |
| 24 | 多 tenant 并发隔离 | 集成 | 4h |
| 25 | API contract tests (REST) | 集成 | 3h |
| 26 | Worker SDK 外部任务循环 | 集成 | 3h |
| 27 | Coverage instrumentation + reporting | DevOps | 2h |

**长期交付**: 覆盖率 70 → 80%+

---

## 关键里程碑

```
Sprint 1: P0消除 (fetch_and_lock并发 + try_join边界 + EL负数)
Sprint 2: H1消除 (ParallelJoin group_id 语义测试)
Sprint 3: 70% 覆盖 (恢复 + 外部任务 + Saga)
Sprint 4: Doc-tests 启用 + API 测试
Sprint 5+: 80%+ 覆盖 + Chaos + E2E
```

---

## 测试基础设施建议

```bash
# 覆盖率门禁建议
cargo llvm-cov --fail-under-lines 50  # 短期
cargo llvm-cov --fail-under-lines 70  # 中期
cargo llvm-cov --fail-under-lines 80  # 长期
```

**工具建议**:
- `cargo-llvm-cov` - 覆盖率追踪
- `rstest` - 参数化测试
- `proptest` - 状态机属性测试
