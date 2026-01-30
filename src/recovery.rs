//! Whitepaper §12 + docs_recovery §5.3: Crash recovery and Reconcile.
//! Load Running instances and tokens; reconcile token state (Executing → Ready, increment attempt); reschedule events.

use crate::engine::{payloads, EngineEvent};
use crate::model::{Token, TokenStatus};
use crate::persistence::{ProcessInstanceRepo, TokenRepo};
use std::collections::VecDeque;

/// Reconcile rules (docs_recovery §5.3): Ready → enqueue TokenArrived; Executing → reset to Ready, increment attempt in DB, then enqueue TokenArrived; Waiting/Created/Suspended → skip; Completed/Terminated → ignore.
pub fn recover(
    process_repo: &dyn ProcessInstanceRepo,
    token_repo: Option<&dyn TokenRepo>,
    out: &mut VecDeque<EngineEvent>,
) {
    let running_ids = process_repo.list_running();
    for instance_id in running_ids {
        let Some(instance) = process_repo.load(&instance_id) else {
            continue;
        };
        for token in &instance.tokens {
            match token.status {
                TokenStatus::Ready => {
                    out.push_back(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: instance.id.clone(),
                        token_id: token.id.clone(),
                        node_id: token.node_id.clone(),
                    }));
                }
                TokenStatus::Executing => {
                    if let Some(tr) = token_repo {
                        let reset = Token {
                            id: token.id.clone(),
                            node_id: token.node_id.clone(),
                            status: TokenStatus::Ready,
                            mode: token.mode,
                            version: token.version,
                            attempt: token.attempt.saturating_add(1),
                            parallel_group_id: token.parallel_group_id.clone(),
                            updated_at: None,
                        };
                        if tr.update_token_cas(&instance.id, &reset) {
                            out.push_back(EngineEvent::TokenArrived(payloads::TokenArrived {
                                instance_id: instance.id.clone(),
                                token_id: token.id.clone(),
                                node_id: token.node_id.clone(),
                            }));
                        }
                    } else {
                        out.push_back(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: instance.id.clone(),
                            token_id: token.id.clone(),
                            node_id: token.node_id.clone(),
                        }));
                    }
                }
                TokenStatus::Waiting | TokenStatus::Suspended | TokenStatus::Created => {}
                TokenStatus::Completed | TokenStatus::Terminated => {}
            }
        }
    }
}
