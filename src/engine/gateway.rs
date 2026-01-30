//! GatewayEvaluator: evaluate gateway, return target node(s) (design: overview §3.3).

use std::collections::HashMap;

use crate::model::{EdgeCondition, Node};
use crate::model::NodeId;

/// Design: overview §3.3 — evaluate(gateway, ctx) -> Vec<NodeId>.
pub trait GatewayEvaluator {
    fn evaluate(
        &self,
        node: &Node,
        variables: &HashMap<String, String>,
    ) -> Vec<NodeId>;
}

/// Exclusive gateway: first matching edge or default.
pub struct ExclusiveGatewayEvaluator;

impl GatewayEvaluator for ExclusiveGatewayEvaluator {
    fn evaluate(
        &self,
        node: &Node,
        variables: &HashMap<String, String>,
    ) -> Vec<NodeId> {
        let mut default_target: Option<NodeId> = None;
        for edge in &node.outgoing_edges {
            match &edge.condition {
                None => return vec![edge.target],
                Some(EdgeCondition::Default) => default_target = Some(edge.target),
                Some(EdgeCondition::VariableEq { key, value }) => {
                    if variables.get(key).as_deref() == Some(value) {
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
