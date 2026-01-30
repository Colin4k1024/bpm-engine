太好了，这一步是**BPM 引擎真正“长骨头”的地方**。
下面我给你的是一套 **可以直接指导你写代码的 Token 生命周期状态图设计**，**含并行（Fork / Join）**，而且和你前面那个 **Event + Handler 模型是完全对齐的**。

你可以把它当成 **Token 子系统的设计规范**。

---

# Token 生命周期状态图

##（支持并行 / Gateway）

---

## 1️⃣ Token 的设计定位（先统一认知）

> **Token = 流程中的“执行权 + 位置”**

- Token 永远绑定 **一个流程实例**
- Token 同一时间只在 **一个 Node 上**
- 并行 ≠ 一个 Token 多线程
  👉 并行 = **Token 分裂**

---

## 2️⃣ Token 状态枚举（完整且可扩展）

```rust
enum TokenStatus {
    Created,        // 刚创建，未进入节点
    Ready,          // 可执行（在节点入口）
    Executing,      // 节点执行中（ServiceTask）
    Waiting,        // 被阻塞（UserTask / Timer / Join）
    Suspended,      // 人工挂起（可选）
    Completed,      // 当前节点完成
    Terminated,     // 被终止
}
```

👉 **V1 最少用：Created / Ready / Waiting / Completed**

---

## 3️⃣ 核心生命周期（线性流程）

```
        ┌─────────┐
        │ Created │
        └────┬────┘
             │
             ▼
        ┌─────────┐
        │  Ready  │ ◄──────────────┐
        └────┬────┘                │
             │                     │
             ▼                     │
     ┌──────────────┐              │
     │  Executing   │              │
     └────┬─────────┘              │
          │                         │
          ▼                         │
     ┌──────────────┐              │
     │  Waiting     │──────────────┘
     └────┬─────────┘
          │
          ▼
     ┌──────────────┐
     │  Completed   │
     └────┬─────────┘
          │
          ▼
     ┌──────────────┐
     │   Ready      │ (next node)
     └──────────────┘
```

---

## 4️⃣ 各 NodeType 对 Token 的影响（对照表）

| NodeType                | 行为                          |
| ----------------------- | ----------------------------- |
| Start                   | Created → Ready               |
| ServiceTask             | Ready → Executing → Completed |
| UserTask                | Ready → Waiting               |
| Exclusive Gateway       | Ready → Completed             |
| Parallel Gateway (Fork) | Ready → Completed + 新 Token  |
| Parallel Gateway (Join) | Ready → Waiting / Completed   |
| End                     | Ready → Terminated            |

---

## 5️⃣ 并行网关（Parallel Gateway）详解

这是 BPM 最复杂的地方，我们拆成 **Fork** 和 **Join** 两种行为。

---

### 5.1 Parallel Fork（并行分支）

#### 语义

> 一个 Token 进入 → N 个 Token 出来

#### 状态图

```
        Token A (Ready)
               │
               ▼
      ┌────────────────┐
      │ Parallel Fork  │
      └──────┬─────────┘
             │
      ┌──────┴──────────────┐
      ▼        ▼             ▼
 Token B    Token C       Token D
  Ready      Ready         Ready
```

#### 行为规则（必须严格）

1. 原 Token：

   - Ready → Completed

2. 每条 outgoing：

   - 创建新 Token（Created → Ready）

3. 所有新 Token：

   - 绑定同一个 `parallel_group_id`

```rust
struct Token {
    id: TokenId,
    node_id: NodeId,
    status: TokenStatus,
    parallel_group: Option<ParallelGroupId>,
}
```

---

### 5.2 Parallel Join（并行汇聚）

#### 语义

> 等待同一并行组的所有 Token 都到达

#### 状态图

```
Token B ─┐
         ├──► Parallel Join ──► Token E
Token C ─┤
         │
Token D ─┘
```

#### Join 判定规则（极其重要）

```text
JOIN 成立，当且仅当：
- 同一 process_instance
- 同一 parallel_group
- 所有 expected branch Token 都到达 join 节点
```

#### 状态迁移

| Token    | 状态变化            |
| -------- | ------------------- |
| 未到齐   | Ready → Waiting     |
| 到齐时   | Waiting → Completed |
| 新 Token | Created → Ready     |

---

### Join 的内部状态（你必须存）

```rust
ParallelJoinState {
    parallel_group_id,
    expected: usize,
    arrived: HashSet<TokenId>,
}
```

---

## 6️⃣ 并行 Token 的完整生命周期（文字版）

```
Fork Gateway:
  Token A (Ready)
    → Completed
    → spawn Token B, C, D (Ready)

Parallel Branch:
  Token B → ServiceTask → Waiting → Completed
  Token C → UserTask → Waiting → Completed
  Token D → ServiceTask → Completed

Join Gateway:
  Token B arrives → Waiting
  Token C arrives → Waiting
  Token D arrives → Waiting
  All arrived:
    - Mark B, C, D as Completed
    - Create Token E (Ready)
```

---

## 7️⃣ Token + Event 的映射关系（关键）

| Token 状态变化        | 触发 Event               |
| --------------------- | ------------------------ |
| Created → Ready       | TokenArrived             |
| Ready → Executing     | NodeExecutionStarted     |
| Executing → Completed | TokenCompleted           |
| Ready → Waiting       | TokenBlocked             |
| Waiting → Ready       | TokenResumed             |
| Completed → Ready     | TokenArrived (next node) |

👉 **Event 是 Token 状态变化的外显形式**

---

## 8️⃣ 设计中必须避免的 3 个坑（重要）

### ❌ 1. Join 不绑定 Fork

> 会导致“串错线”

✔ 必须用 `parallel_group_id`

---

### ❌ 2. Token 状态隐式变化

> 状态写在逻辑里，没人知道

✔ 所有变化 → Event

---

### ❌ 3. Join 不可恢复

> 系统重启后 Join 永远卡住

✔ JoinState 必须持久化

---

## 9️⃣ Token 生命周期 = 你写代码的“边界条件表”

建议你在代码里直接写一个：

```rust
impl Token {
    fn transition(&mut self, to: TokenStatus) {
        // 校验合法状态迁移
        self.status = to;
    }
}
```

---

## 🔟 你现在可以立刻开写的代码模块

**强烈建议顺序：**

1️⃣ `TokenStatus` + 状态迁移校验
2️⃣ Parallel Fork 的 Token 复制
3️⃣ Parallel JoinState + 判定逻辑
4️⃣ Event ↔ Token 状态映射

---

## 最后一句（架构级忠告）

> **并行不是“多线程”，而是“多 Token”**
> 谁把这个想清楚，BPM 引擎就不会崩。

---

如果你愿意，下一步我可以继续帮你补齐 **最后一个难点**：

- 👉 **定时器 / 超时 / Retry 的 Token 模型**
- 👉 **补偿（Saga）如何和 Token 生命周期结合**
- 👉 **Token + DB 锁 / 并发控制设计**

你选一个，我们把这个引擎直接推到“能用级别”。
