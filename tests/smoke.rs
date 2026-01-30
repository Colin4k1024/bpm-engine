//! Smoke test: workspace crates and engine minimal API.

use bpm_engine::bpm_core::{Token, TokenStatus};

#[test]
fn engine_smoke_test() {
    let _token = Token {
        id: "t1".to_string(),
        node_id: "n1".to_string(),
        status: TokenStatus::Ready,
        mode: bpm_engine::bpm_core::TokenMode::Forward,
        version: 0,
        attempt: 0,
        parallel_group_id: None,
        updated_at: None,
    };
    assert!(!_token.waiting());
}
