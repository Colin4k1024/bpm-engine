//! NodeExecutor: execute node for token, advance token (design: overview §3.2).

use crate::model::{Node, ProcessInstance, Token};

/// Execution context passed to executor (variables, services, etc.).
pub struct ExecutionContext<'a> {
    pub instance: &'a mut ProcessInstance,
}

/// Design: overview §3.2 — execute(node, token, ctx).
pub trait NodeExecutor {
    fn execute(
        &self,
        node: &Node,
        token: &mut Token,
        ctx: &mut ExecutionContext<'_>,
    );
}
