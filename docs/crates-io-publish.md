# crates.io 发布指南

由于网络限制，需要手动发布到 crates.io。

## 前提条件

1. 拥有 crates.io 账号
2. 已生成 API token（在 https://crates.io/settings/tokens）

## 发布步骤

### 1. 配置网络（可选）

如果在国内网络环境，建议使用代理或配置镜像：

```bash
# 方法1: 使用代理
export https_proxy=http://127.0.0.1:7890
export http_proxy=http://127.0.0.1:7890

# 方法2: 临时移除镜像配置
mv ~/.cargo/config.toml ~/.cargo/config.toml.backup
```

### 2. 登录 crates.io

```bash
cargo login <your-api-token>
```

### 3. 发布各个 crate

按照依赖顺序发布：

```bash
# 1. Core (无外部依赖)
cd crates/core
cargo publish

# 2. Storage (依赖 core)
cd ../storage
cargo publish

# 3. Runtime (依赖 core, storage)
cd ../runtime
cargo publish

# 4. Memory Adapter
cd ../adapters/memory
cargo publish

# 5. BPMN Parser
cd ../../bpmn
cargo publish

# 6. Worker SDK
cd ../worker-sdk
cargo publish

# 7. REST Server
cd ../server/rest
cargo publish

# 8. 主包
cd ../..
cargo publish
```

### 4. 恢复配置（如果修改过）

```bash
mv ~/.cargo/config.toml.backup ~/.cargo/config.toml
```

### 5. 验证发布

```bash
# 检查包是否可用
cargo search bpm-engine

# 测试安装
cargo install bpm-engine
```

## 发布检查清单

- [ ] 所有测试通过：`cargo test --workspace`
- [ ] 代码格式正确：`cargo fmt --check`
- [ ] 无 clippy 警告：`cargo clippy --workspace --all-targets -- -D warnings`
- [ ] 版本号已更新
- [ ] CHANGELOG 已更新
- [ ] README 中的版本号已更新

## 版本号规范

遵循 [Semantic Versioning](https://semver.org/):

- **MAJOR**: 不兼容的 API 变更
- **MINOR**: 向后兼容的功能新增
- **PATCH**: 向后兼容的 bug 修复

## 自动化发布（推荐）

使用 GitHub Actions 自动发布：

```yaml
# .github/workflows/release.yml
name: Release

on:
  push:
    tags:
      - 'v*'

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo publish
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

## 故障排除

### 网络超时

```bash
# 使用 sparse 协议
export CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
cargo publish
```

### 版本已存在

```bash
# 更新版本号
# 编辑 Cargo.toml 中的 version 字段
cargo update
```

### 依赖未发布

确保按照依赖顺序发布，或使用 `--no-verify` 跳过验证：

```bash
cargo publish --no-verify
```
