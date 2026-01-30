//! TokenScheduler: find executable tokens, advance token lifecycle (design: overview §3.1).

use crate::model::{ProcessInstance, Token};

/// Design: overview §3.1 — poll returns executable tokens.
pub trait TokenScheduler {
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token>;
}

/// Default implementation: tokens not waiting are executable (legacy model).
pub struct DefaultTokenScheduler;

impl TokenScheduler for DefaultTokenScheduler {
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token> {
        instance.tokens.iter().filter(|t| !t.waiting()).collect()
    }
}
