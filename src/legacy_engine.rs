//! Legacy monolithic engine (step/run_until_wait/complete_user_task).
//! Kept until Step 3 migrates to event pump; then logic moves to handlers.

use crate::model::*;
use std::collections::HashMap;

pub struct Engine;

impl Engine {
    pub fn start(def: &ProcessDefinition) -> ProcessInstance {
        ProcessInstance {
            id: uuid::Uuid::new_v4().to_string(),
            process_def_id: def.id.to_string(),
            tenant_id: None,
            tokens: vec![Token {
                id: uuid::Uuid::new_v4().to_string(),
                node_id: def.start.to_string(),
                status: TokenStatus::Ready,
                mode: TokenMode::Forward,
                version: 0,
                attempt: 0,
                parallel_group_id: None,
                updated_at: None,
            }],
            variables: Default::default(),
            state: InstanceState::Running,
            version: 0,
        }
    }

    pub fn step(def: &ProcessDefinition, instance: &mut ProcessInstance) {
        if instance.completed() {
            return;
        }

        let mut new_tokens = vec![];
        let mut services_to_run = vec![];

        for token in instance.tokens.iter_mut() {
            if token.waiting() {
                continue;
            }

            let node = match def.nodes.get(token.node_id.as_str()) {
                Some(n) => n,
                None => continue,
            };

            match node.node_type {
                NodeType::Start => {
                    new_tokens.extend(Self::move_token(node));
                    token.status = TokenStatus::Waiting;
                }

                NodeType::ServiceTask(service) => {
                    services_to_run.push((token.node_id.clone(), service));
                    new_tokens.extend(Self::move_token(node));
                    token.status = TokenStatus::Waiting;
                }

                NodeType::UserTask => {
                    println!("⏸ UserTask at node {}", node.id);
                    token.status = TokenStatus::Waiting;
                }

                NodeType::ExclusiveGateway => {
                    if let Some(t) = Self::evaluate_exclusive_gateway(node, &instance.variables) {
                        new_tokens.push(t);
                    }
                    token.status = TokenStatus::Waiting;
                }

                NodeType::End => {
                    instance.state = InstanceState::Completed;
                    println!("✅ Process completed");
                }
                NodeType::ParallelFork | NodeType::ParallelJoin { .. } => {
                    // Handled by event-pump TokenArrivedHandler; legacy engine no-op
                }
            }
        }

        for (_, service) in services_to_run {
            service(instance);
        }

        instance.tokens.extend(new_tokens);
    }

    pub fn run_until_wait(def: &ProcessDefinition, instance: &mut ProcessInstance) {
        while !instance.completed() {
            Self::step(def, instance);
            if instance.completed() {
                return;
            }
            if instance.tokens.iter().all(|t| t.waiting()) {
                return;
            }
        }
    }

    pub fn complete_user_task(
        def: &ProcessDefinition,
        instance: &mut ProcessInstance,
        node_id: &str,
    ) {
        let node = match def.nodes.get(node_id) {
            Some(n) => n,
            None => return,
        };
        instance.tokens.retain(|t| t.node_id != node_id);
        instance.tokens.extend(Self::move_token(node));
        Self::run_until_wait(def, instance);
    }

    fn move_token(node: &Node) -> Vec<Token> {
        node.outgoing_edges
            .iter()
            .map(|e| Token {
                id: uuid::Uuid::new_v4().to_string(),
                node_id: e.target.to_string(),
                status: TokenStatus::Ready,
                mode: TokenMode::Forward,
                version: 0,
                attempt: 0,
                parallel_group_id: None,
                updated_at: None,
            })
            .collect()
    }

    fn evaluate_exclusive_gateway(
        node: &Node,
        variables: &HashMap<String, String>,
    ) -> Option<Token> {
        use crate::model::EdgeCondition;
        let mut default_target: Option<NodeId> = None;
        for edge in &node.outgoing_edges {
            match &edge.condition {
                None => {
                    return Some(Token {
                        id: uuid::Uuid::new_v4().to_string(),
                        node_id: edge.target.to_string(),
                        status: TokenStatus::Ready,
                        mode: TokenMode::Forward,
                        version: 0,
                        attempt: 0,
                        parallel_group_id: None,
                        updated_at: None,
                    });
                }
                Some(EdgeCondition::Default) => default_target = Some(edge.target),
                Some(EdgeCondition::VariableEq { key, value }) => {
                    if variables.get(key).as_deref() == Some(value) {
                        return Some(Token {
                            id: uuid::Uuid::new_v4().to_string(),
                            node_id: edge.target.to_string(),
                            status: TokenStatus::Ready,
                            mode: TokenMode::Forward,
                            version: 0,
                            attempt: 0,
                            parallel_group_id: None,
                            updated_at: None,
                        });
                    }
                }
                Some(EdgeCondition::Expression(expr)) => {
                    if crate::engine::el::eval_condition(expr, variables).unwrap_or(false) {
                        return Some(Token {
                            id: uuid::Uuid::new_v4().to_string(),
                            node_id: edge.target.to_string(),
                            status: TokenStatus::Ready,
                            mode: TokenMode::Forward,
                            version: 0,
                            attempt: 0,
                            parallel_group_id: None,
                            updated_at: None,
                        });
                    }
                }
            }
        }
        default_target.map(|t| Token {
            id: uuid::Uuid::new_v4().to_string(),
            node_id: t.to_string(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        })
    }
}
