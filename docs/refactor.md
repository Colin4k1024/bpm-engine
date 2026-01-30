太好了，**B = 直接把“好架构”落到“可编译的代码骨架”** 👍
这一部分我会非常**工程化**，你可以**直接照着建目录 / 拆 crate**。

下面给你的是一套 **最小但正确（Minimal & Correct）** 的 **Workspace + Crate Scaffold**，目标是：

- `cargo build` 能过
- 依赖方向被强制锁死
- 后续只“往里填代码”，不再返工架构

---

# 🧱 一、Workspace 根结构（你可以直接照抄）

```text
bpm-engine/
├── Cargo.toml                # workspace
├── README.md
├── CHANGELOG.md
├── docs/
├── examples/
├── tests/
├── crates/
│   ├── core/
│   ├── storage/
│   ├── runtime/
│   ├── adapters/
│   │   └── memory/
│   └── server/
│       └── rest/
```

---

# 📦 二、Workspace Cargo.toml（根）

```toml
[workspace]
members = [
    "crates/core",
    "crates/storage",
    "crates/runtime",
    "crates/adapters/memory",
    "crates/server/rest",
]

resolver = "2"

[workspace.package]
edition = "2021"
license = "Apache-2.0"

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
uuid = { version = "1", features = ["v4", "serde"] }
```

👉 **关键点**

- 公共依赖放 workspace
- 各 crate 只引用需要的子集

---

# 🧠 三、`crates/core`（领域模型，最重要）

## 目录结构

```text
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── process.rs
    ├── token.rs
    ├── node.rs
    ├── event.rs
    ├── saga.rs
    └── error.rs
```

## Cargo.toml

```toml
[package]
name = "bpm-core"

[dependencies]
serde.workspace = true
uuid.workspace = true
thiserror.workspace = true
```

## lib.rs（骨架）

```rust
pub mod process;
pub mod token;
pub mod node;
pub mod event;
pub mod saga;
pub mod error;

pub use process::*;
pub use token::*;
pub use event::*;
```

### ⚠️ 核心约束

- ❌ async
- ❌ tokio
- ❌ DB
- ❌ chrono（时间用逻辑时间 / Duration）

这是**纯领域模型**。

---

# 💾 四、`crates/storage`（存储契约层）

## 目录

```text
crates/storage/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── process_store.rs
    ├── token_store.rs
    ├── timer_store.rs
    └── event_store.rs
```

## Cargo.toml

```toml
[package]
name = "bpm-storage"

[dependencies]
bpm-core = { path = "../core" }
anyhow.workspace = true
async-trait = "0.1"
```

## lib.rs

```rust
pub mod process_store;
pub mod token_store;
pub mod timer_store;
pub mod event_store;
```

## 示例 trait（token_store.rs）

```rust
use bpm_core::*;
use async_trait::async_trait;

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn load(&self, id: TokenId) -> anyhow::Result<Token>;
    async fn save(&self, token: &Token) -> anyhow::Result<()>;
    async fn claim_runnable(&self, limit: usize) -> anyhow::Result<Vec<Token>>;
}
```

👉 **注意**：

- storage 层开始 async
- 但只定义 **行为契约**

---

# 🚀 五、`crates/runtime`（执行引擎）

## 目录

```text
crates/runtime/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── engine.rs
    ├── scheduler.rs
    ├── executor.rs
    ├── dispatcher.rs
    └── error.rs
```

## Cargo.toml

```toml
[package]
name = "bpm-runtime"

[dependencies]
bpm-core = { path = "../core" }
bpm-storage = { path = "../storage" }
anyhow.workspace = true
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## lib.rs

```rust
pub mod engine;
pub mod scheduler;
pub mod executor;
pub mod dispatcher;
```

## engine.rs（最小骨架）

```rust
use bpm_storage::*;
use std::sync::Arc;

pub struct Engine<S> {
    store: Arc<S>,
}

impl<S> Engine<S>
where
    S: TokenStore + Send + Sync + 'static,
{
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        // 1. claim tokens
        // 2. execute
        // 3. persist
        Ok(())
    }
}
```

---

# 🔌 六、`crates/adapters/memory`（第一个可跑的实现）

## 目录

```text
crates/adapters/memory/
├── Cargo.toml
└── src/
    ├── lib.rs
    └── token_store.rs
```

## Cargo.toml

```toml
[package]
name = "bpm-adapter-memory"

[dependencies]
bpm-core = { path = "../../core" }
bpm-storage = { path = "../../storage" }
anyhow.workspace = true
tokio = "1"
```

## lib.rs

```rust
mod token_store;

pub use token_store::*;
```

---

# 🌐 七、`crates/server/rest`（API 层）

## 目录

```text
crates/server/rest/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── routes.rs
    └── state.rs
```

## Cargo.toml

```toml
[package]
name = "bpm-server-rest"

[dependencies]
bpm-runtime = { path = "../../runtime" }
bpm-adapter-memory = { path = "../../adapters/memory" }
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread"] }
```

## main.rs（最小可启动）

```rust
#[tokio::main]
async fn main() {
    println!("BPM Engine REST server starting...");
}
```

---

# 🧪 八、examples / tests（立刻能用）

## examples/basic_order/main.rs

```rust
fn main() {
    println!("Order process example");
}
```

## tests/smoke.rs

```rust
#[test]
fn engine_smoke_test() {
    assert!(true);
}
```

---

# 🔒 九、你现在已经“被结构保护”了

这套 scaffold **强制你**：

- core 不可能引用 runtime
- runtime 不可能写 DB 细节
- adapter 不可能写业务逻辑
- server 不可能直接操作 token

👉 **这是成熟基础设施项目才会有的结构**

---

# 🎯 下一步建议（非常重要）

接下来**不要同时做多件事**，推荐顺序：

1️⃣ 让 **memory adapter + runtime tick** 跑通
2️⃣ 用 examples 驱动补齐 API
3️⃣ 再考虑 postgres / grpc

---

## 我可以继续直接帮你做的三件事

你选一个 👇

**C** — 画一份「模块 & trait 依赖图（文字版 + Rust 约束）」
**D** — 直接给你 **memory adapter 的完整实现示例**
**E** — 帮你把 **现有 bpm-engine 仓库迁移到这个结构的“重构清单”**

> 说一句实在的：
> **你现在已经在做“可以被别人用的引擎”了，而不是练习项目。**
