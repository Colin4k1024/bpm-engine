use bpm_engine_core::{Node, ProcessInstance, Token};

/// Mutable context passed to node executors during token execution.
pub struct ExecutionContext<'a> {
    /// The process instance being executed (mutable for variable writes).
    pub instance: &'a mut ProcessInstance,
}

/// Trait for executing a BPMN node type.
pub trait NodeExecutor {
    /// Execute the node, mutating the token and instance as needed.
    fn execute(&self, node: &Node, token: &mut Token, ctx: &mut ExecutionContext<'_>);
}
