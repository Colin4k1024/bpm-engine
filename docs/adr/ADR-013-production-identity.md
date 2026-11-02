# ADR-013: 生产身份信任边界——JWT iss/aud 验证与密钥轮换

## 决策信息

- **编号**: ADR-013
- **标题**: JWT `iss`/`aud` claim 验证、多密钥轮换、短期令牌；信任边界与本地开发模式保留
- **状态**: Accepted
- **日期**: 2026-11-02
- **Owner**: tech-lead
- **关联**: Roadmap N5-2（Issue #74）；E5-1（JWT/RBAC 基线）、ADR-009（租户隔离）、ADR-011（敏感数据）

## 背景与约束

现行 JWT 实现（`crates/server/rest/src/middleware.rs`）：
- HS256，claims `{ sub, tenant, role, exp [, iat] }`
- 签名/expiry/claims 验证，失败 → 401 `invalid_token`
- **无 `iss`/`aud` 验证**——任何持有 secret 的方都能 mint 任意 issuer 的 token
- **单密钥**——无轮换；更换 secret 导致所有现有 token 立即失效
- 已有：tenant 来自 claim、X-Tenant-Id 在 JWT 模式弃用、RBAC 权限矩阵

## 决策结果

### 1. JWT `iss`/`aud` 验证

新增可选配置：
- `auth.jwt_issuer`（env `BPM_AUTH__JWT_ISSUER`）：预期 issuer。设置后，JWT 的 `iss` claim 必须匹配，否则 401 `invalid_token`。
- `auth.jwt_audience`（env `BPM_AUTH__JWT_AUDIENCE`）：预期 audience。设置后，JWT 的 `aud` claim 必须匹配。

未设置时 = 不验证（向后兼容，现有部署不需要变更）。

### 2. 密钥轮换

新增 `auth.jwt_secrets`（env `BPM_AUTH__JWT_SECRETS`，逗号分隔）：多个接受密钥。签名验证尝试每个密钥，任一成功即通过。

轮换流程：
1. 新密钥加入 `jwt_secrets` 列表（旧密钥仍在）
2. 新 token 用新密钥 mint
3. 旧 token 仍被接受（旧密钥在列表中）
4. 过渡期后，从列表移除旧密钥 → 旧 token 失效

`jwt_secret`（单个）仍作为简化配置（等价于 `jwt_secrets` 只有一个密钥）。

### 3. 短期令牌

`exp` 验证已存在（E5-1）。推荐：
- access token 短期（15-60 分钟）
- refresh token 由外部 IdP 管理（引擎不处理 refresh）
- `iat` 记录签发时间（已在 claims 中）

### 4. 信任边界

| 模式 | iss/aud | 密钥 | 租户来源 | 适用 |
|------|:-------:|:----:|:--------:|------|
| JWT | 验证（配置时） | 多密钥轮换 | claim（verified） | 生产 |
| API key | — | 单 key | X-Tenant-Id（untrusted） | legacy/bootstrap |
| Open | — | — | X-Tenant-Id（dev） | 开发/测试 |

**红线**：
- JWT 模式的 tenant 来自 verified claim，不接受 X-Tenant-Id header（冲突 → 403）
- API key 模式的 tenant 是 untrusted header（与 E5-1 一致）
- iss/aud 验证只在 JWT 模式生效（API key/open 模式无 JWT claims）

### 5. 可测样例

1. **iss 验证**：token iss="wrong" → 401；iss="expected" → 200
2. **aud 验证**：token aud="wrong" → 401；aud="expected" → 200
3. **密钥轮换**：old secret token → 200（列表有 old）；移除 old → 401
4. **跨租户**：tenant A token 访问 tenant B 资源 → 404
5. **过期**：exp in past → 401
6. **本地开发**：open 模式无 secret → 200（无 auth）

## 非目标

- **不做 refresh token 管理**（外部 IdP 责任）
- **不做 RS256/非对称签名**（HS256 足够 v1；RS256 需要 JWKS 获取，另立 issue）
- **不做 token 撤销列表**（短期 token + 密钥轮换覆盖撤销场景）
- **不改变 API key/open 模式**（向后兼容）

## 后续动作

- 实现 iss/aud 验证 + 多密钥轮换（middleware.rs + config）
- 测试（cross-tenant, expired, rotated key scenarios）
- `docs/rbac.md`：更新 JWT 配置文档（iss/aud/secrets）
