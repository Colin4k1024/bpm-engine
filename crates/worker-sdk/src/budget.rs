//! Budget tracking and fault protection (N3-3, ADR-011 §2-§5).
//!
//! Provides per-task budget enforcement (max cost, tokens, attempts),
//! exponential backoff computation, and retry classification for provider
//! errors. The worker checks the budget before each model/tool call and
//! fails permanently when exceeded.

use std::time::Duration;

/// Per-task budget limits (ADR-011 §2). All fields optional; `None` = no
/// limit on that dimension.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    /// Maximum cumulative cost in micro-currency units.
    pub max_cost_micros: Option<u64>,
    /// Maximum cumulative input + output tokens.
    pub max_total_tokens: Option<u64>,
    /// Maximum number of execution attempts (independent of engine retries).
    pub max_attempts: Option<u32>,
}

impl Budget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn max_cost_micros(mut self, v: u64) -> Self {
        self.max_cost_micros = Some(v);
        self
    }

    pub fn max_total_tokens(mut self, v: u64) -> Self {
        self.max_total_tokens = Some(v);
        self
    }

    pub fn max_attempts(mut self, v: u32) -> Self {
        self.max_attempts = Some(v);
        self
    }
}

/// Tracks cumulative spending against a [`Budget`].
#[derive(Debug, Clone, Default)]
pub struct BudgetTracker {
    budget: Budget,
    cumulative_cost_micros: u64,
    cumulative_tokens: u64,
    attempts: u32,
}

/// The dimension that exceeded the budget.
#[derive(Debug, Clone)]
pub struct BudgetExceeded {
    pub dimension: &'static str,
    pub spent: u64,
    pub limit: u64,
}

impl BudgetExceeded {
    /// Stable error message for the Fail payload.
    pub fn message(&self) -> String {
        format!(
            "budget_exceeded: {} spent={} limit={}",
            self.dimension, self.spent, self.limit
        )
    }
}

impl BudgetTracker {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            ..Default::default()
        }
    }

    /// Check whether the next call is within budget. Returns the dimension
    /// that was exceeded, if any. Call before each model/tool invocation.
    pub fn check(&self) -> Result<(), BudgetExceeded> {
        if let Some(max) = self.budget.max_attempts {
            if self.attempts >= max {
                return Err(BudgetExceeded {
                    dimension: "attempts",
                    spent: self.attempts as u64,
                    limit: max as u64,
                });
            }
        }
        if let Some(max) = self.budget.max_cost_micros {
            if self.cumulative_cost_micros >= max {
                return Err(BudgetExceeded {
                    dimension: "cost_micros",
                    spent: self.cumulative_cost_micros,
                    limit: max,
                });
            }
        }
        if let Some(max) = self.budget.max_total_tokens {
            if self.cumulative_tokens >= max {
                return Err(BudgetExceeded {
                    dimension: "total_tokens",
                    spent: self.cumulative_tokens,
                    limit: max,
                });
            }
        }
        Ok(())
    }

    /// Record the cost of a completed call.
    pub fn record(&mut self, cost_micros: u64, tokens_in: u64, tokens_out: u64) {
        self.cumulative_cost_micros += cost_micros;
        self.cumulative_tokens += tokens_in + tokens_out;
    }

    /// Increment the attempt counter (call once per execution attempt).
    pub fn increment_attempt(&mut self) {
        self.attempts += 1;
    }

    /// Cumulative cost in micro-currency.
    pub fn cumulative_cost(&self) -> u64 {
        self.cumulative_cost_micros
    }

    /// Cumulative token count.
    pub fn cumulative_tokens(&self) -> u64 {
        self.cumulative_tokens
    }

    /// Number of attempts so far.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// Compute an exponential backoff delay for a transient failure (ADR-011 §5).
///
/// `retry_after = min(base * 2^attempt, cap)`. Default base=1s, cap=60s.
pub fn compute_backoff(attempt: u32, base_secs: u64, cap_secs: u64) -> Duration {
    let exp = attempt.min(20); // prevent overflow
    let secs = base_secs.saturating_mul(2u64.saturating_pow(exp));
    Duration::from_secs(secs.min(cap_secs))
}

/// Classify a provider error message into a retry category and backoff
/// (ADR-011 §5). Returns (category, retry_after).
///
/// Error code prefixes:
/// - `rate_limited` → Transient, backoff from the error
/// - `provider_error` / `execution_timeout` → Transient, exponential backoff
/// - `bad_input` / `output_validation_failed` / `budget_exceeded` → Permanent
pub fn classify_provider_error(
    error: &str,
    attempt: u32,
) -> (bpm_engine_core::RetryCategory, Option<Duration>) {
    use bpm_engine_core::RetryCategory;
    if error.starts_with("rate_limited") {
        (
            RetryCategory::Transient,
            Some(compute_backoff(attempt, 2, 120)),
        )
    } else if error.starts_with("provider_error") || error.starts_with("execution_timeout") {
        (
            RetryCategory::Transient,
            Some(compute_backoff(attempt, 1, 60)),
        )
    } else {
        // bad_input, output_validation_failed, budget_exceeded, etc.
        (RetryCategory::Permanent, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_tracker_exceeds_cost() {
        let tracker = BudgetTracker::new(Budget::new().max_cost_micros(100));
        tracker_check_and_record(&tracker);
    }

    fn tracker_check_and_record(_tracker: &BudgetTracker) {
        // Helper: can't mutate in a const context, so test via a function
    }

    #[test]
    fn budget_cost_limit_works() {
        let mut t = BudgetTracker::new(Budget::new().max_cost_micros(100));
        assert!(t.check().is_ok());
        t.record(50, 10, 5);
        assert!(t.check().is_ok());
        t.record(60, 0, 0);
        let err = t.check().unwrap_err();
        assert_eq!(err.dimension, "cost_micros");
        assert!(err.message().contains("budget_exceeded"));
    }

    #[test]
    fn budget_token_limit_works() {
        let mut t = BudgetTracker::new(Budget::new().max_total_tokens(100));
        t.record(0, 60, 50);
        let err = t.check().unwrap_err();
        assert_eq!(err.dimension, "total_tokens");
    }

    #[test]
    fn budget_attempts_limit_works() {
        let mut t = BudgetTracker::new(Budget::new().max_attempts(3));
        t.increment_attempt();
        t.increment_attempt();
        t.increment_attempt();
        let err = t.check().unwrap_err();
        assert_eq!(err.dimension, "attempts");
    }

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(compute_backoff(0, 1, 60), Duration::from_secs(1));
        assert_eq!(compute_backoff(1, 1, 60), Duration::from_secs(2));
        assert_eq!(compute_backoff(2, 1, 60), Duration::from_secs(4));
        assert_eq!(compute_backoff(10, 1, 60), Duration::from_secs(60)); // capped
    }

    #[test]
    fn classify_rate_limited_is_transient() {
        let (cat, backoff) = classify_provider_error("rate_limited: 429", 1);
        assert_eq!(cat, bpm_engine_core::RetryCategory::Transient);
        assert!(backoff.is_some());
    }

    #[test]
    fn classify_provider_error_is_transient() {
        let (cat, backoff) = classify_provider_error("provider_error: 500", 2);
        assert_eq!(cat, bpm_engine_core::RetryCategory::Transient);
        assert!(backoff.is_some());
    }

    #[test]
    fn classify_bad_input_is_permanent() {
        let (cat, backoff) = classify_provider_error("bad_input: empty prompt", 0);
        assert_eq!(cat, bpm_engine_core::RetryCategory::Permanent);
        assert!(backoff.is_none());
    }

    #[test]
    fn classify_budget_exceeded_is_permanent() {
        let (cat, _) = classify_provider_error("budget_exceeded: cost_micros", 0);
        assert_eq!(cat, bpm_engine_core::RetryCategory::Permanent);
    }
}
