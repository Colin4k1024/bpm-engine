# Offline Regression Evaluation (E6-3, issue #28)

Fixed sample set + mock provider, comparing **definition versions** on
success rate, recovery correctness, human-intervention rate, latency and
cost — with the **statistical metrics strictly separated from the
deterministic kernel tests** (统计指标与确定性内核测试分开).

Crate: `bpm-engine-ai-reference` (`crates/ai-reference`). Everything runs
**offline**: the [`MockModelProvider`] is deterministic (no network, no
clock, no randomness), and the only HTTP is to a locally spawned engine on
`127.0.0.1` — the same harness pattern as the crate's other integration
suites. No API keys; any ledger credential value works (outputs are
masked).

| artifact | location |
|---|---|
| the eval harness (case model, v2 fixtures, injections, report, aggregation) | `crates/ai-reference/src/eval.rs` |
| the deterministic test suite (small subset, exact assertions) | `crates/ai-reference/tests/regression_eval.rs` |
| the runnable statistical harness (full set, report only) | `crates/ai-reference/examples/regression_eval.rs` |
| the mock provider + price table | `crates/ai-reference/src/provider.rs` |
| the five base flows the cases run on | `crates/ai-reference/src/flows.rs` |
| the reference worker handler (incl. the `ref-validate` fixture step) | `crates/ai-reference/src/handler.rs` |

[`MockModelProvider`]: ../crates/ai-reference/src/provider.rs

---

## 1. The separation principle (the key acceptance)

The deterministic half and the statistical half of the acceptance live in
different artifacts, by design:

| | DETERMINISTIC kernel tests | STATISTICAL/comparative harness |
|---|---|---|
| where | `tests/regression_eval.rs` (+ `tests/reference_flows.rs`, unit tests in `src/eval.rs`) | `examples/regression_eval.rs` over `src/eval.rs` |
| runs in | every `cargo test --workspace` (CI) | on demand: `cargo run -p bpm-engine-ai-reference --example regression_eval` |
| case set | a small, fast subset (3 cases + the crash case, × both versions) | the FULL fixed set (13 cases × both versions) |
| asserts | exact completion states, exact attempt counts, exact costs per the mock price table, recovery verdicts, intervention counts, report structure/round-trip | **nothing** — writes `regression-report.json`, prints the markdown summary |
| latency | never measured, never asserted | wall clock, **reported only** |

The rule: **a wall-clock number or an aggregate rate is never a gate.**
The deterministic proxies — step counts, attempt counts (lease-attempt
totals from `/steps`), envelope costs — are what CI may pin, because they
are functions of the definitions and the mock provider, not of the
machine. `CaseRun.latency_ms` exists in the report for humans; the tests
only sanity-bound it (`< 5000 ms` as a hang guard, not a performance
gate).

## 2. The fixed case set

`eval::fixed_case_set()` — thirteen cases built from the five reference
flows plus deliberate variants. Every case runs against BOTH definition
versions (§3), so the report has 26 rows.

| case | flow | variant / injection | v1 expectation |
|---|---|---|---|
| `retrieval-baseline` | 检索 retrieval | — | completes, `end` |
| `retrieval-transient-failure` | retrieval | `TransientFailOnce` on `ref-retrieve` | completes after one redelivery (2 attempts, 1 failed) |
| `retrieval-worker-crash` | retrieval | `CrashAndRedeliver` on `ref-retrieve` | completes after lease-expiry redelivery (2 attempts, 0 failed) |
| `retrieval-permanent-failure` | retrieval | `PermanentFail` on `ref-retrieve` | blocked, DLQ, exactly 1 attempt (no retry storm) |
| `inference-low-risk` | 推理 inference | `risk = low` → verdict `approved` | completes, `end-approved` |
| `inference-high-risk` | inference | `risk = high` → verdict `rejected` | completes, `end-rejected` |
| `approval-approved` | 审批 approval | human approves (`__approval` present) | completes, `end`, 2 intervention points |
| `approval-unapproved` | approval | human rejects (fail-closed gate) | blocked at `act`, DLQ |
| `tool-approved` | 工具 tool | `__approval` present | completes, `end` |
| `tool-transient-failure` | tool | `TransientFailOnce` on `ref-run-tool` | completes after retry; **v2 (`retries: 1`) exhausts → blocked + DLQ** |
| `tool-unapproved` | tool | no `__approval` (fail-closed gate) | blocked, DLQ |
| `compensation-happy` | 补偿 compensation | `saga_fail = false` | completes, `end-ok` |
| `compensation-failed` | compensation | `saga_fail = true` | completes, `end-compensated` (reverse-order undo) |

## 3. The v1/v2 fixture-version mechanism

The harness compares **definition versions**, honestly scoped: the
"versions" are **fixture variants inside the harness** — that IS the
comparison mechanism. `eval::fixture_version_selector()` maps each case's
flow (v1, as-is) to a modified v2 variant registered under a `:2`-suffixed
definition id (`ai-reference-tool:2`, …). Because instance start resolves
definitions by exact id, both versions coexist in the engine's definition
store and each run executes exactly one of them; the case's start
variables and user-task decisions are grafted onto the v2 spec unchanged
(the same case input, only the definition differs).

| flow | v1 | v2 fixture (definition change) | observable delta |
|---|---|---|---|
| retrieval | `retrieve → summarize → end` | + `validate` step after the summary | +1 step, +1 attempt, same cost |
| inference | gateway: `verdict == approved` conditional, default reject | gateway default flipped (conditional `rejected`, default approve) | **none** — a behavior-preserving refactor (both verdicts reach the same ends) |
| approval | `propose → approve → act → end` | + `validate` step before the side effect | +1 step, +1 attempt, same intervention, same cost |
| tool | `run-tool` with the DSL default `retries: 3` | **`retries: 1`** | **behavior-changing**: the transient case exhausts its budget — success −1, recovery −1, blocked + DLQ |
| compensation | failure path `… → undo → end-compensated` | + `audit` step after undo (happy path unchanged) | +1 step on the failure cases only |

A flow outside the fixture table degrades to the identity mapping (v2 ==
v1) and the comparison honestly reports a zero delta for it; extend the
fixture table in `v2_variant` or pass a custom `VersionSelector`.

## 4. Failure injection (the recovery cases)

A case declares `inject: Option<Injection>`; the injection targets the
flow's **first `serviceTask` node** (the node id and task type are parsed
from the DSL document). The harness's worker-side hook
(`InjectingHandler`, wrapping the real `ReferenceHandler`) applies it:

- **`TransientFailOnce`** — the first attempt of the target task fails
  with an E3-2 *transient* category (`injected_transient_failure: …`).
  The engine decrements retries, requeues, redelivers; the second attempt
  runs the real handler. Step trace: lock → fail → lock → complete.
- **`PermanentFail`** — every attempt of the target fails with a
  *permanent* category (`injected_permanent_failure: …`): straight to
  `Failed` + dead-letter queue + `TokenFailed`, no retry storm. Step
  trace: lock → fail (exactly one attempt).
- **`CrashAndRedeliver`** — the first attempt sleeps past the lease with
  **no renewal** (the real handler — and its E3-2 auto-renewal — has not
  started yet) and then returns a stale result whose submission the
  engine rejects, exactly like a worker that crashed mid-handle: the
  lease expires, the next `fetch_and_lock` reclaims and redelivers, the
  second attempt completes. No `fail` is ever submitted, so **no retry is
  consumed**. Step trace: lock → lock → complete (the first attempt
  surfaces as `expired` in the E6-1 step timeline).

Timings stay SHORT: the eval profile (`eval_flow_options`) uses a **2 s
lease** and a **200 ms worker poll**. The lease is 2 s rather than 1 s on
purpose — the store's lease expiry is second-granular
(`lock_expire_at = unix_secs() + lease_secs`), so a 1 s lease is really
0–1000 ms of wall time depending on where inside the second the lock
lands, and a plain task could expire mid-handle and collect phantom
attempts. 2 s guarantees a ≥ 1 s true lease (the instant mock handler
never approaches it) while the crash injection still redelivers in ~2 s.
The crash hook sleeps `2 × lease + 500 ms` — comfortably past the
redelivery+completion window, so its stale submission is deterministically
rejected.

## 5. Metric definitions

| metric | definition | asserted? |
|---|---|---|
| `success_rate` | `completed / cases`, per version | report only; the per-case `completed` flag is deterministic and pinned by the tests |
| `recovery_correctness` | per injected case (the `RecoveryVerdict`): **transient** → instance completed, exactly 2 attempts on the injected node (1 failed), no DLQ entry; **crash** → instance completed, exactly 2 attempts, 0 failed (expiry, not failure), no DLQ entry; **permanent** → instance blocked, exactly 1 attempt, DLQ entry present (no retry storm) | the verdict is a pure, deterministic function of the run — pinned by the tests; the aggregate `recovery_rate` is report only |
| `human_intervention_rate` | `intervention_points / decision_points`, where **intervention points** = user-task decisions submitted by the runner (a human decision, replayed) + `__approval` side-effect inputs required by the case; **decision points** = the flow's declared user-task decisions + its side-effect gates (task types `ref-act` / `ref-run-tool`) | the counts are flow-derived and deterministic — pinned; the rate is report only |
| `latency_ms` | wall clock from instance start to the run's terminal outcome | **reported only** — plus the deterministic proxies `steps` (step-timeline length) and `attempts` (total `ExternalTaskLocked` events) |
| `cost` | sum of the E6-1 envelope `TaskCost` over the run's steps: `amount_micros` and tokens from the mock price table (summarize 18 µUSD, classify 7, echo 8; tool steps record tools but no cost) | deterministic given the mock provider — exact values pinned by the tests |

Supporting details:

- **Terminal nodes** are the steps whose node id starts with `end`
  (a token ARRIVING at an end node completes the instance immediately, so
  the step's existence is the reached signal — end nodes carry no
  per-token completion event).
- **Blocked cases stop early**: the eval driver exits its poll loop as
  soon as a dead-letter entry for the instance appears — in this engine a
  DLQ'd task means the instance is terminally blocked (its token stays at
  the failed node), so waiting for the run timeout would prove nothing.
- **What counts as an intervention**: every user-task decision the runner
  submits on the human's behalf (the approval flow's `approve` decision)
  and every `__approval` input a case needs to unlock a side-effect tool
  (the E3-3 gate). Flows with no user task and no side-effect gate
  contribute `0 / 0`.

## 6. Running it

```bash
# The statistical harness: full case set, both versions, self-spawned
# server, writes regression-report.json + prints the markdown summary.
cargo run -p bpm-engine-ai-reference --example regression_eval

# Optional CLI argument: the report path.
cargo run -p bpm-engine-ai-reference --example regression_eval -- /tmp/my-report.json

# The deterministic half (subset, exact assertions) — part of CI:
cargo test -p bpm-engine-ai-reference --test regression_eval
```

The JSON report (`RegressionReport`) is serde round-trippable; its shape:

```text
RegressionReport
├── harness, cases, versions ["v1","v2"]
├── per_case:   [CaseRun]            — one per case × version
│   ├── case, version, process_def_id, instance_id
│   ├── completed, terminal_nodes, steps, attempts, failed_attempts
│   ├── latency_ms (reported only), cost_micros, tokens_in/out
│   ├── intervention_points, decision_points
│   ├── dlq_entry, recovery: Option<RecoveryVerdict>, expectation_met
├── summary:    [VersionSummary]     — one per version
│   └── success_rate, recovery_rate?, intervention_rate, totals
│       (cost/steps/attempts/tokens), expectations_met, latency mean/min/max
└── comparison: VersionComparison    — v1 → v2
    ├── success_rate_delta, recovery_rate_delta?, intervention_rate_delta
    ├── cost/steps/attempts/mean_latency deltas
    └── per_case: [CaseDelta]
```

## 7. Extending with new cases

1. Build the flow — reuse a constructor from `crates/ai-reference/src/flows.rs`
   (`retrieval_flow()`, `inference_flow("low")`, …) or a `FlowSpec` of
   your own DSL.
2. Add the case to `fixed_case_set()`:
   `RegressionCase::new("my-case", flow, /* must_complete */ true).terminal_nodes(["end"])`
   — and `.inject(Injection::TransientFailOnce)` if it is a recovery case
   (the injection targets the flow's first service task).
3. If the case runs a NEW definition variant, add a v2 fixture row in
   `v2_variant` (DSL const + task types) — otherwise the fixture selector
   falls back to the identity mapping for unknown def ids.
4. Pin the deterministic invariants in `tests/regression_eval.rs`
   (`run_subset(&["my-case"], &env_var)` gives you the report rows to
   assert on). Statistical observations go in the harness output, never
   in an assertion.
