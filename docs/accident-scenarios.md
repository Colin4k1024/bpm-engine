# Accident-level scenarios: "Can the engine explain what happened?"

These scenarios answer: **After something goes wrong, can the engine show exactly what happened?** Each is runnable and verifiable via the Trace API and Replay.

---

## 1. Payment + timeout + retry + completion

**Question:** Payment task times out or fails; we retry and then complete. Can we see the full lifecycle?

**Steps:**

1. Start the engine: `cargo run -p bpm-engine-server-rest`
2. Start a process instance (payment-flow):  
   `curl -X POST http://127.0.0.1:3000/api/v1/process-instances -H "Content-Type: application/json" -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'`  
   Note `instance_id`.
3. Run the payment worker: `cargo run -p bpm-engine-worker-sdk --example payment`  
   The worker fetches the task, completes it; process finishes.
4. **With retries:** Deploy a process that has an external task with retries (e.g. 3). Start an instance. Call `POST .../external-tasks/:task_id/fail` with the same worker_id twice (retry); then fetch-and-lock again and complete. The trace will show `ExternalTaskFailed` twice and `ExternalTaskCompleted` once.

**Verify:**

- `GET /api/v1/process-instances/:id/trace` — `token_timelines` show token movement; `external_task_history` shows Locked → Failed → Locked → Failed → Locked → Completed (or similar).
- `POST /api/v1/process-instances/:id/replay` then `GET /api/v1/replay/:session_id/snapshot` and step through — replay reproduces the same final state.

**Expected:** Trace and replay give a full, ordered view of payment task lock, failures, retries, and completion.

---

## 2. Parallel fork: one branch fails before join

**Question:** We have a parallel fork and join; one branch fails before the join. Does the engine keep join semantics and can we see which branch failed?

**Steps:**

1. Deploy a process with ParallelFork → two branches → ParallelJoin (e.g. one branch has an external task that you fail, the other completes).
2. Start an instance. Complete one branch; fail the other branch’s external task (e.g. `POST .../external-tasks/:task_id/fail` until final fail).
3. The join never fires (all branches did not complete). Instance remains in a state where one token is at the join (waiting) and the failed branch has no token at the join.

**Verify:**

- `GET /api/v1/process-instances/:id` — `tokens` show which nodes still have tokens; status reflects the failed branch.
- `GET /api/v1/process-instances/:id/trace` — `token_timelines` show each branch’s events; you see one branch with `TokenFailed` / `ExternalTaskFailed` and the other with completion.
- Replay: replay the instance; snapshot at the end matches the current state (join not completed, one branch failed).

**Expected:** No partial join; trace and replay show exactly which branch failed and where tokens are.

---

## 3. Worker crash → lock expiry → reclaim → another worker takes the task

**Question:** Worker A locks a task and crashes; lock expires; worker B fetches the same task. Can we see Locked (A) → expiry/reclaim → Locked (B) → Completed?

**Steps:**

1. Start the engine. Start an instance with an external task (e.g. payment-flow).
2. Start worker A: `cargo run -p bpm-engine-worker-sdk --example duplicate_workers` (or set `WORKER_ID=worker-A`). It fetches and locks the task.
3. Kill worker A before it completes (e.g. Ctrl+C or kill the process). Do **not** call fail; the task stays LOCKED with worker A’s lease.
4. Wait for lock expiry (e.g. 30 seconds with default lock_duration).
5. Optionally call `POST .../external-tasks/fetch-and-lock` from the engine to trigger reclaim (or rely on the next fetch). Start worker B: same example with `WORKER_ID=worker-B`. Worker B will fetch the task (after reclaim or expiry).
6. Worker B completes the task.

**Verify:**

- `GET /api/v1/process-instances/:id/trace` — `external_task_history` for that task shows `ExternalTaskLocked` (worker A), then after expiry no further Locked until worker B locks and completes; you see `ExternalTaskLocked` (worker B) and `ExternalTaskCompleted`.
- History events are ordered by `occurred_at`; you can confirm lock ownership and that only one worker completed.

**Expected:** Trace shows the task’s full lifecycle: first lock, expiry/reclaim, second lock, completion. No double completion.

---

## Summary

| Scenario              | What to run / do                         | How to verify                          |
|-----------------------|------------------------------------------|----------------------------------------|
| Payment + retry       | Instance + worker fail twice then complete | Trace `external_task_history`; replay  |
| Fork one branch fails | Instance + complete one branch, fail other | Trace `token_timelines`; instance state |
| Worker crash + reclaim| Two workers; kill first, wait, second completes | Trace `external_task_history`          |

Use `GET /api/v1/process-instances/:id/trace` and replay (`POST .../replay`, `GET .../snapshot`, step) as the main tools to answer “what happened?” after each scenario.
