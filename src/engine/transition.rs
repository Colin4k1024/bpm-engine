//! Token transition helpers: move_token, evaluate_exclusive_gateway (shared with legacy engine logic).

use super::el;
use crate::model::{EdgeCondition, Node, Token, TokenMode, TokenStatus};
use std::collections::HashMap;

fn new_token(node_id: String, parallel_group_id: Option<String>) -> Token {
    Token {
        id: uuid::Uuid::new_v4().to_string(),
        node_id,
        status: TokenStatus::Ready,
        mode: TokenMode::Forward,
        version: 0,
        attempt: 0,
        parallel_group_id,
        updated_at: None,
    }
}

pub fn move_token(node: &Node) -> Vec<Token> {
    node.outgoing_edges
        .iter()
        .map(|e| new_token(e.target.to_string(), None))
        .collect()
}

pub fn move_token_with_group(node: &Node, parallel_group_id: String) -> Vec<Token> {
    node.outgoing_edges
        .iter()
        .map(|e| new_token(e.target.to_string(), Some(parallel_group_id.clone())))
        .collect()
}

pub fn evaluate_exclusive_gateway(
    node: &Node,
    variables: &HashMap<String, String>,
) -> Option<Token> {
    let mut default_target: Option<crate::model::NodeId> = None;
    for edge in &node.outgoing_edges {
        match &edge.condition {
            None => return Some(new_token(edge.target.to_string(), None)),
            Some(EdgeCondition::Default) => default_target = Some(edge.target),
            Some(EdgeCondition::VariableEq { key, value }) => {
                if variables.get(key).as_deref() == Some(value) {
                    return Some(new_token(edge.target.to_string(), None));
                }
            }
            Some(EdgeCondition::Expression(expr)) => {
                if el::eval_condition(expr, variables).unwrap_or(false) {
                    return Some(new_token(edge.target.to_string(), None));
                }
            }
        }
    }
    default_target.map(|t| new_token(t.to_string(), None))
}
