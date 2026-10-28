# ADR-005: Forward-Only SQL Migrations

## 决策信息

- **编号**: ADR-005
- **状态**: Accepted
- **日期**: 2026-10-28
- **Owner**: tech-lead
- **关联**: E1-1（Postgres 存储接入）、E1-2（迁移与部署契约）

## 背景与约束

E1-1 之前，schema 由 `migrate()` 内联的 `CREATE TABLE IF NOT EXISTS` 创建，无法表达"对已存在库的结构性变更"。E1-2 需要可演进的 schema：版本化迁移、升级/回退策略、CI 中验证空库与旧 schema 升级两条路径。

约束：
- 引擎是持久化优先、崩溃安全的：schema 变更必须与状态数据一致，不能出现"半迁移"库
- 迁移脚本必须编译期嵌入（`include_str!`），二进制部署无运行时文件依赖
- 动态 SQL 一律参数绑定（`$1`），拼接 SQL 是安全红线
- 存量部署：已有库由内联 `migrate()` 时代创建，无任何版本记录

## 备选方案

| 方案 | 优点 | 风险 |
|------|------|------|
| **A: 前向-only 版本化迁移（自研 runner）** | 单向维护、`schema_migrations` 即事实来源、零外部依赖 | 回退依赖备份，无自动 down |
| B: 双向迁移（每版本 up + down 脚本） | 理论上可自动回退 | up/down 双份维护必然漂移；down 丢数据语义无法自动推导 |
| C: 引入 refinery/sqlx-migrate 等框架 | 社区成熟 | 引入重量级依赖与抽象，与"纯 tokio-postgres + deadpool"栈不一致；baseline stamping 仍需自研 |

## 决策结果

**采用方案 A**：`crates/adapters/postgres/migrations/` 下的版本化 SQL 文件（`0001_init.sql`、`0002_query_indexes.sql`，前向递增），由 `src/migrations.rs` 内的轻量 runner 执行。

**设计要点**：
- `schema_migrations(version, name, applied_at)` 是 schema 事实来源；runner 只前向执行未记录的版本
- 每个迁移在独立事务中执行并写入版本记录：要么完整生效，要么无痕迹失败（幂等，重启安全）
- **Baseline stamping**：检测到 v1 表存在但 `schema_migrations` 无记录（内联 migrate 时代的存量库），将 0001 标记为已应用（不重复执行），继续 0002+；空库则从头全量应用
- `migrate_on_start = false` 时启动即校验 `migration_status`：存在 pending 版本则拒绝启动并报错（部署契约——禁止带着过期 schema 运行）
- 回退策略 = **恢复备份 / PITR**，必要时辅以人工评审的 down 脚本；不做自动 down 迁移
- `deploy/schema.sql` 降级为参考工件（与 migrations 保持同步），不再是迁移来源

**不做自动 down 的理由**：up/down 双维护在中长期必然漂移（回退路径缺乏 CI 压力测试）；引擎 pre-1.0、单服务语义，回退场景首选"恢复到上一个已验证快照"；数据性变更（如列删除）无法自动可逆。

## 后续动作

- backend-enginekeeper 维护 migrations 目录约定：只增不改，破坏性变更必须拆为独立版本并附运行手册
- CI `migrations` job 验证空库全量迁移与 legacy baseline 升级两条路径（service container，无需 Docker-in-Docker）
- 生产部署文档强调：升级前 `pg_dump`，`migrate_on_start = false` 的环境必须显式跑迁移或开启自动迁移
