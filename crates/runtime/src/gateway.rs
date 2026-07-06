//! GatewayEvaluator: evaluate gateway, return target node(s).

use std::collections::HashMap;

use bpm_engine_core::{EdgeCondition, Node, NodeId};

/// Trait for evaluating gateway routing decisions.
pub trait GatewayEvaluator {
    /// Evaluate which outgoing nodes should receive tokens.
    fn evaluate(&self, node: &Node, variables: &HashMap<String, String>) -> Vec<NodeId>;
}

/// Evaluator for exclusive (XOR) gateways — selects one outgoing path.
pub struct ExclusiveGatewayEvaluator;

impl GatewayEvaluator for ExclusiveGatewayEvaluator {
    fn evaluate(&self, node: &Node, variables: &HashMap<String, String>) -> Vec<NodeId> {
        let mut default_target: Option<NodeId> = None;
        for edge in &node.outgoing_edges {
            match &edge.condition {
                None => return vec![edge.target],
                Some(EdgeCondition::Default) => default_target = Some(edge.target),
                Some(EdgeCondition::VariableEq { key, value }) => {
                    if variables.get(key) == Some(value) {
                        return vec![edge.target];
                    }
                }
                Some(EdgeCondition::Expression(expr)) => {
                    if super::el::eval_condition(expr, variables).unwrap_or(false) {
                        return vec![edge.target];
                    }
                }
            }
        }
        default_target.map(|t| vec![t]).unwrap_or_default()
    }
}
