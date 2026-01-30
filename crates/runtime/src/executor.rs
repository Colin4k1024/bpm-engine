use bpm_core::{Node, ProcessInstance, Token};

pub struct ExecutionContext<'a> {
    pub instance: &'a mut ProcessInstance,
}

pub trait NodeExecutor {
    fn execute(&self, node: &Node, token: &mut Token, ctx: &mut ExecutionContext<'_>);
}
