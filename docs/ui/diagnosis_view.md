# AI Diagnosis View — E6-2 诊断视图 (issue #27)

> The per-instance **diagnosis surface** of the operations console: the
> E6-1 step timeline (`GET /api/v1/process-instances/:id/steps`) rendered
> so an operator can answer "which step, which worker, why did it fail,
> what did the AI attempt cost, and where do I recover it" — with recovery
> jumps from an error straight into the filtered history view and into
> the dead-letter queue. Pure UI: no backend changes; the wire contract
> is `docs/trace.md` + `docs/openapi.yaml` (normative).

---

## 1. Where it lives

| Piece | Location |
|-------|----------|
| Route | `/diagnosis/:instanceId` (`ui/src/App.tsx`) |
| Page | `ui/src/pages/DiagnosisPage.tsx` — loads `api.getInstanceSteps`, renders the breadcrumb back to instances/trace, refresh action |
| Panel | `ui/src/components/StepsPanel.tsx` — the step timeline, attempts, envelope metadata, artifacts, jump links |
| API client | `api.getInstanceSteps(id)` in `ui/src/api/client.ts` + the `Steps*` types in `ui/src/api/types.ts` |

**Entry points** (existing navigation patterns, no new sidebar item — the
view is instance-scoped like `/trace/:instanceId`):

- every instance row on the Instances page gets a `Diagnosis →` link
  next to `Open trace →`;
- the trace page instance header gets an `AI diagnosis` button next to
  Refresh (`InstanceHeader` gained an optional `actions` slot);
- the diagnosis page links back with `Open full trace →`.

## 2. What it renders

The E6-1 response verbatim — **metadata only, never a variable value**
(the `/steps` contract is sensitive-by-default; see `docs/trace.md` §4):

- **Summary strip** — definition id, instance state
  (`RUNNING|COMPLETED|TERMINATED`), trace id (= instance id in v1), step
  count.
- **One item per step** (`(node_id, token_id)`, causal order):
  - `#step_no`, node id, **kind badge** (`node` gray / `external_task`
    blue / `timer` amber), a **status badge**, token id, and an
    expandable event list (`event_type` + `occurred_at` only);
  - **external-task steps**: one row per worker lease attempt —
    `worker_id`, `locked_at → completed_or_failed` (formatted) with the
    computed duration, `retries_remaining`, and the recorded `error`
    (monospace, visually truncated, full text on hover);
  - **AI envelope metadata** per attempt (and, when no attempt carries
    one, the step-level last-recorded summary): model
    `provider/name (version)`, `tools` as `name@version`, `cost`
    (`currency + amount` formatted from `amount_micros`, tokens in/out,
    duration ms), `execution_id`, `retry_category`, and **artifact
    references** — URI as inline code with a copy-to-clipboard button,
    plus size / media type / sha256 prefix. URIs are never rendered as
    hyperlinks: the engine contract says artifact URIs are references
    that are never dereferenced.
- **Attempt outcome badges** with distinct colors:
  `in_flight` purple, `completed` green, `failed` red, `expired` amber
  (lease elapsed, task redelivered — E1-3). Each non-obvious outcome
  carries a one-line explanation (lease held / redelivered).
- **Waiting & approval states** — honest v1 rules, derived from the step
  kind + its user-task events only:
  - `UserTaskCreated` without `UserTaskCompleted` → **approval
    required** (orange badge, highlighted step border);
  - an attempt with outcome `in_flight` → **lease held** (the step is
    waiting for that worker);
  - `TokenCompleted` seen → completed; anything else → **waiting**
    (covers timer waits: a `timer` step whose token has not completed);
  - `TokenFailed` → failed.
  Envelope/tools-based approval detection (e.g. an approval tool in the
  envelope) is deliberately NOT guessed in v1 — the user-task events are
  the only honest signal the wire shape carries today.
- **Opaque envelopes** — `{"opaque": true}` renders as "envelope
  redacted or unparsable — nothing surfaced" (the E2-3 history
  redaction policy made the stored envelope value unavailable; the view
  never sees and never reconstructs it).

## 3. Error → history jump

Every failed attempt offers **`View in history →`**,
which navigates to the instance's existing
history view on the trace page with the filter query params the history
endpoint already supports:

```
/trace/:instanceId?token_id=<step token>&event_type=ExternalTaskFailed
```

`TracePage` applies `token_id` / `event_type` from the URL to its
history filters **once per navigation** (`useSearchParams` effect); the
timeline then shows exactly the events behind the failure and the
payload inspector still works on each of them. Absent params never
clobber filters the user selected manually. When the step's token could
not be correlated (empty `token_id`), the link filters on the event type
alone.

## 4. Dead-letter jump

When a failed attempt is **terminal** — `retries_remaining` reached 0
(legacy exhaustion) or the worker reported `retry_category: permanent`
(E3-2 fast path; both routes land the task in the DLQ) — the attempt and
the step offer **`View dead letters →`**:

```
/dead-letters?instance_id=<instanceId>
```

`DeadLettersPage` accepts the `instance_id` query param as a client-side
pre-filter of the queue (a chip shows the active filter with a
`Clear filter` action; absent param = full queue, unchanged). The queue
entry itself then links back to `/trace/:instance_id` as before, closing
the loop.

## 5. Wire fields (reference)

Exact shapes live in `docs/openapi.yaml` (`StepsResponse` family) and
`docs/trace.md`; the TS interfaces in `ui/src/api/types.ts` mirror them:
`StepsResponse { trace_id, instance_id, process_def_id, state, steps[] }`,
`StepView { step_no, node_id, token_id, kind, events[], attempts?,
envelope_summary?, variables_touched? }`, `StepAttemptView { worker_id,
outcome, locked_at, completed_or_failed?, error?, retries_remaining?,
envelope? }`, `StepEnvelopeSummary { execution_id?, retry_category?,
model?, tools?, cost?, artifacts?, opaque? }`. Server-optional fields
(`skip_serializing_if`) are optional in TS too; `occurred_at` /
`locked_at` / `completed_or_failed` are Unix **seconds** strings and are
formatted through the shared i18n date formatter.

## 6. Testing & limits

- E2E: `ui/e2e/diagnosis-view.spec.js` (Playwright, same harness as
  `console-pages.spec.js`) — drives everything through the real REST API,
  no mocks: a payment-flow instance failed 3× (attempt 1 with a full AI
  envelope incl. cost + artifact; attempt 3 exhausts the retries → DLQ),
  an instance holding a live lease, and an approval instance; then
  asserts the step/attempt/badge rendering, the envelope metadata, the
  artifact URI + copy button, the error→history deep link with filters
  applied, the DLQ deep link with the instance filter + clear, and the
  zh localization.
- Limitations (honest): approval detection is event-based only (above);
  the same-second ordering caveat of `/steps` (docs/trace.md §2)
  applies to what the timeline can show; attempt `duration` is
  seconds-granular (both timestamps are Unix seconds).
