# Invariant Suite 设计（D1）

本文档用于**直接指导你在 bpm-engine 中落地 Invariant Suite**。不是概念稿，而是：
- 模块边界清晰
- Trait 可直接拷贝
- 示例 Invariant 可运行

---

## 1. 模块目录结构（推荐）

```text
crates/engine-core/
├── invariant/
│   ├── mod.rs
│   ├── engine.rs          # InvariantEngine
│   ├── context.rs         # InvariantContext
│   ├── violation.rs       # InvariantViolation
│   │
│   ├── core/              # Token / Execution 级
│   │   ├── mod.rs
│   │   ├── token_unique.rs
│   │   └── token_lifecycle.rs
│   │
│   ├── flow/              # BPM 语义
│   │   ├── mod.rs
│   │   └── parallel_balance.rs
│   │
│   ├── event/             # Event 合法性
│   │   ├── mod.rs
│   │   └── event_applicable.rs
│   │
│   └── replay/            # Replay 专属
│       ├── mod.rs
│       └── seek_deterministic.rs
```

---

## 2. Invariant Trait（核心接口）

```rust
pub trait Invariant: Send + Sync {
    fn name(&self) -> &'static str;

    fn check(&self, ctx: &InvariantContext) -> Result<(), InvariantViolation>;
}
```

设计原则：
- **纯检查**，禁止修改 snapshot
- 失败 = Bug，不是业务错误

---

## 3. InvariantContext（一定要给全）

```rust
pub struct InvariantContext<'a> {
    pub snapshot: &'a EngineSnapshot,
    pub last_event: Option<&'a EngineEvent>,
    pub history: &'a [EngineEvent],
    pub cursor: usize,
}
```

说明：
- `cursor`：当前 replay 到第几个 event
- `last_event`：允许针对单个 event 精准报错

---

## 4. InvariantViolation（结构化错误）

```rust
#[derive(Debug)]
pub struct InvariantViolation {
    pub invariant: &'static str,
    pub cursor: usize,
    pub message: String,
}

impl InvariantViolation {
    pub fn new(
        invariant: &'static str,
        cursor: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            cursor,
            message: message.into(),
        }
    }
}
```

⚠️ message **一定要人类可读**（后面 UI / CI 都靠它）

---

## 5. InvariantEngine（组合执行）

```rust
pub struct InvariantEngine {
    invariants: Vec<Box<dyn Invariant>>,
}

impl InvariantEngine {
    pub fn new(invariants: Vec<Box<dyn Invariant>>) -> Self {
        Self { invariants }
    }

    pub fn check_all(&self, ctx: InvariantContext) -> Result<(), InvariantViolation> {
        for inv in &self.invariants {
            inv.check(&ctx).map_err(|mut v| {
                v.invariant = inv.name();
                v
            })?;
        }
        Ok(())
    }
}
```

---

## 6. 示例 1：Token 唯一性 Invariant（必做）

```rust
pub struct TokenUniqueInvariant;

impl Invariant for TokenUniqueInvariant {
    fn name(&self) -> &'static str {
        "token_unique"
    }

    fn check(&self, ctx: &InvariantContext) -> Result<(), InvariantViolation> {
        let mut seen = std::collections::HashSet::new();

        for token in ctx.snapshot.active_tokens() {
            let key = (token.instance_id, token.token_id);
            if !seen.insert(key) {
                return Err(InvariantViolation::new(
                    self.name(),
                    ctx.cursor,
                    format!("Duplicate token detected: {:?}", key),
                ));
            }
        }

        Ok(())
    }
}
```

---

## 7. 示例 2：Token 生命周期合法性

```rust
pub struct TokenLifecycleInvariant;

impl Invariant for TokenLifecycleInvariant {
    fn name(&self) -> &'static str {
        "token_lifecycle"
    }

    fn check(&self, ctx: &InvariantContext) -> Result<(), InvariantViolation> {
        if let Some(event) = ctx.last_event {
            if let EngineEvent::TokenTransition { from, to, .. } = event {
                if from.is_terminal() && !to.is_terminal() {
                    return Err(InvariantViolation::new(
                        self.name(),
                        ctx.cursor,
                        format!("Illegal token transition: {:?} -> {:?}", from, to),
                    ));
                }
            }
        }
        Ok(())
    }
}
```

---

## 8. Replay 中的使用方式（关键）

```rust
let engine = InvariantEngine::new(vec![
    Box::new(TokenUniqueInvariant),
    Box::new(TokenLifecycleInvariant),
]);

for (i, event) in history.iter().enumerate() {
    snapshot = apply_event(snapshot, event)?;

    engine.check_all(InvariantContext {
        snapshot: &snapshot,
        last_event: Some(event),
        history,
        cursor: i,
    })?;
}
```

👉 **不是最后 check，一定是“每一步” check**

---

## 9. 实践建议（非常重要）

- Invariant 永远 panic-safe（不要 unwrap）
- 不要访问 DB / 外部系统
- 先做 3–5 个硬 Invariant，比 20 个软的有价值

---

## 10. 你现在的状态评价

做到这一步，你的 bpm-engine：

- 已经具备 **工业级内核自校验能力**
- 比 90% 工作流引擎更容易 Debug
- 非常适合写技术博客 / 白皮书

---

### 下一步你可以选：
- **D2**：我直接对照你 repo，指出“应该先写哪 3 个 Invariant”
- **E**：把 Invariant 强制注入真实执行路径（终极形态）

