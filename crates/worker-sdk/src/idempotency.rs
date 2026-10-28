//! Worker-side idempotency helpers (E3-2).
//!
//! At-least-once redelivery means the same task may reach a worker twice
//! (lease expiry after a crash, duplicate fetch, shutdown abort). The
//! [`IdempotencyGuard`] answers "have I already processed this?" for the
//! CURRENT worker process, and [`derive_key`] builds a stable key from the
//! task id + a SHA-256 of the relevant inputs.
//!
//! Scope — per process only:
//! - The guard is a bounded in-memory map. It survives redelivery within
//!   this process's lifetime; it does NOT survive a restart, and it is not
//!   shared between worker processes.
//! - For durable, cross-restart dedup, derive a stable key and check it
//!   against YOUR durable store before performing the side effect, then
//!   record it transactionally with the effect. The engine-side hook for
//!   the same idea is the E3-1 envelope's `idempotency_key`
//!   (`bpm_engine_core::ai_task::AiTaskEnvelope`): a deterministic hash
//!   over the task input + model + tool versions, visible in history for
//!   audit. See `docs/worker-context.md` and
//!   `crates/worker-sdk/examples/idempotency.rs`.

use std::collections::{HashMap, VecDeque};

/// Default key capacity before the oldest entries are evicted.
pub const DEFAULT_MAX_KEYS: usize = 10_000;

/// Bounded, per-process "already processed?" tracker (E3-2).
///
/// ```text
/// static GUARD: IdempotencyGuard = IdempotencyGuard::new(); // or per-handler
///
/// let key = derive_key(&task.task_id, &input);
/// if !guard.check_and_mark(&key) {
///     return TaskResult::Complete { variables: cached_result };
/// }
/// let result = do_side_effect(&input).await;
/// ```
///
/// The bound is FIFO: when the map is full the OLDEST keys are evicted, so
/// a redelivered task whose key was evicted is processed again (correct:
/// the guard is a best-effort dedup, the durable answer is the caller's
/// store — see the module docs). If the attempt FAILS, call
/// [`IdempotencyGuard::unmark`] so the retry can run.
pub struct IdempotencyGuard {
    /// Insertion-ordered keys (front = oldest) — the eviction queue.
    order: VecDeque<String>,
    /// Key membership — the actual "processed?" lookup.
    marked: HashMap<String, ()>,
    max_keys: usize,
}

impl Default for IdempotencyGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl IdempotencyGuard {
    /// Create a guard with the default capacity ([`DEFAULT_MAX_KEYS`]).
    pub fn new() -> Self {
        Self::with_capacity_bound(DEFAULT_MAX_KEYS)
    }

    /// Create a guard that tracks at most `max_keys` entries, evicting the
    /// oldest when the bound is exceeded.
    pub fn with_capacity_bound(max_keys: usize) -> Self {
        Self {
            order: VecDeque::new(),
            marked: HashMap::new(),
            max_keys: max_keys.max(1),
        }
    }

    /// Returns `true` and MARKS the key when it has not been processed yet;
    /// returns `false` when the key was already marked.
    ///
    /// Marking on the happy-path check ("check-and-set") closes the race
    /// where two concurrent deliveries of the same task would both pass a
    /// check-then-mark sequence.
    pub fn check_and_mark(&mut self, key: &str) -> bool {
        if self.marked.contains_key(key) {
            return false;
        }
        self.marked.insert(key.to_string(), ());
        self.order.push_back(key.to_string());
        while self.marked.len() > self.max_keys {
            match self.order.pop_front() {
                Some(evicted) => {
                    self.marked.remove(&evicted);
                }
                None => break,
            }
        }
        true
    }

    /// Un-mark a key so a retry may process it again (call after a failed
    /// attempt). Returns whether the key was marked.
    pub fn unmark(&mut self, key: &str) -> bool {
        if self.marked.remove(key).is_some() {
            // The stale entry stays in `order`; a later re-mark pushes a
            // fresh one. `order` is only an eviction hint, so a stale
            // pointer is harmless: `marked.remove` on it is a no-op.
            true
        } else {
            false
        }
    }

    /// Whether the key is currently marked.
    pub fn is_marked(&self, key: &str) -> bool {
        self.marked.contains_key(key)
    }

    /// Number of tracked keys (bounded by the capacity).
    pub fn len(&self) -> usize {
        self.marked.len()
    }

    /// Whether no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.marked.is_empty()
    }
}

/// Derive a stable idempotency key: `"<task_id>:sha256:<hex>"` where the
/// digest is over `input` (the relevant task inputs — e.g. a canonical
/// serialization of the variables you act on).
///
/// Same task + same input ⇒ same key; different input ⇒ different key.
/// Redeliveries carry the same task_id and inputs, so the key is stable
/// across attempts (which is the point).
pub fn derive_key(task_id: &str, input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", byte);
    }
    format!("{task_id}:sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_and_input_is_rejected_twice() {
        let mut guard = IdempotencyGuard::new();
        let key = derive_key("task-1", "amount=100");
        assert!(guard.check_and_mark(&key), "first mark must succeed");
        assert!(
            !guard.check_and_mark(&key),
            "same key + same input must be rejected (already marked)"
        );
        assert!(guard.is_marked(&key));
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn different_input_yields_a_new_key() {
        let key_a = derive_key("task-1", "amount=100");
        let key_b = derive_key("task-1", "amount=200");
        assert_ne!(key_a, key_b, "different input must derive a different key");
        assert!(key_a.starts_with("task-1:sha256:"));
        assert_eq!(key_a.len(), "task-1:sha256:".len() + 64);

        let mut guard = IdempotencyGuard::new();
        assert!(guard.check_and_mark(&key_a));
        assert!(
            guard.check_and_mark(&key_b),
            "a different input is a different unit of work: allowed"
        );
    }

    #[test]
    fn different_task_id_yields_a_different_key() {
        assert_ne!(derive_key("task-1", "x"), derive_key("task-2", "x"));
    }

    #[test]
    fn derive_key_is_deterministic() {
        assert_eq!(derive_key("task-1", "x"), derive_key("task-1", "x"));
    }

    #[test]
    fn unmark_allows_retry() {
        let mut guard = IdempotencyGuard::new();
        let key = derive_key("task-1", "in");
        assert!(guard.check_and_mark(&key));
        assert!(!guard.check_and_mark(&key));
        // The attempt failed: unmark so the retry may run.
        assert!(guard.unmark(&key));
        assert!(guard.check_and_mark(&key), "retry after unmark must pass");
        // The retry succeeded; a later unmark removes the re-mark too.
        assert!(guard.unmark(&key), "re-marked key can be unmarked again");
        assert!(!guard.is_marked(&key));
        assert!(!guard.unmark(&key), "nothing left to remove");
    }

    #[test]
    fn capacity_bound_evicts_oldest() {
        let mut guard = IdempotencyGuard::with_capacity_bound(2);
        let k1 = derive_key("t", "1");
        let k2 = derive_key("t", "2");
        let k3 = derive_key("t", "3");
        assert!(guard.check_and_mark(&k1));
        assert!(guard.check_and_mark(&k2));
        assert_eq!(guard.len(), 2);
        // Pushing k3 evicts k1 (oldest) — the guard stays bounded.
        assert!(guard.check_and_mark(&k3));
        assert_eq!(guard.len(), 2);
        assert!(!guard.is_marked(&k1), "oldest key evicted at capacity");
        assert!(guard.is_marked(&k2));
        assert!(guard.is_marked(&k3));
        // The evicted key is processable again (best-effort dedup).
        assert!(guard.check_and_mark(&k1));
    }
}
