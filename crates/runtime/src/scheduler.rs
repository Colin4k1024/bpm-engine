use bpm_engine_core::{ProcessInstance, Token};

pub trait TokenScheduler {
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token>;
}

pub struct DefaultTokenScheduler;

impl TokenScheduler for DefaultTokenScheduler {
    fn poll<'a>(&self, instance: &'a ProcessInstance) -> Vec<&'a Token> {
        instance.tokens.iter().filter(|t| !t.waiting()).collect()
    }
}
