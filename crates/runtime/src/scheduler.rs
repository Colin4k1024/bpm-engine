use bpm_engine_core::{ProcessInstance, Token};

/// Trait for polling tokens that are ready for execution.
pub trait TokenScheduler {
    /// Return tokens from the instance that are ready to execute (not waiting).
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token>;
}

/// Default scheduler: returns all tokens that are not in Waiting state.
pub struct DefaultTokenScheduler;

impl TokenScheduler for DefaultTokenScheduler {
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token> {
        instance.tokens.iter().filter(|t| !t.waiting()).collect()
    }
}
