//! Middleware: API key authentication and basic rate limiting.

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Serialize)]
struct AuthError {
    error: String,
}

/// API key authentication middleware.
/// Checks `X-API-Key` header against configured key.
/// If `BPM_API_KEY` env var is unset, authentication is disabled (PoC mode).
pub async fn api_key_auth(headers: HeaderMap, request: Request, next: Next) -> Response {
    let expected_key = std::env::var("BPM_API_KEY").ok();

    // If no key configured, allow all requests (PoC mode)
    let Some(expected) = expected_key else {
        return next.run(request).await;
    };

    let provided = headers.get("x-api-key").and_then(|v| v.to_str().ok());

    match provided {
        Some(key) if key == expected => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(AuthError {
                error: "missing or invalid API key".to_string(),
            }),
        )
            .into_response(),
    }
}

/// Simple sliding-window rate limiter state.
pub struct RateLimiterState {
    window_start: Instant,
    request_count: u64,
    max_requests: u64,
    window_secs: u64,
}

impl RateLimiterState {
    pub fn new(max_requests: u64, window_secs: u64) -> Self {
        Self {
            window_start: Instant::now(),
            request_count: 0,
            max_requests,
            window_secs,
        }
    }

    fn check(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.window_start).as_secs();
        if elapsed >= self.window_secs {
            self.window_start = now;
            self.request_count = 1;
            true
        } else if self.request_count < self.max_requests {
            self.request_count += 1;
            true
        } else {
            false
        }
    }
}

/// Rate limiter shared state (wrap in Arc<Mutex<>> for use with axum).
pub type SharedRateLimiter = Arc<Mutex<RateLimiterState>>;

/// Rate limiting middleware.
/// Returns 429 when request rate exceeds configured threshold.
pub async fn rate_limit(
    axum::extract::State(limiter): axum::extract::State<SharedRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let allowed = {
        let mut state = limiter.lock().await;
        state.check()
    };

    if allowed {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(AuthError {
                error: "rate limit exceeded".to_string(),
            }),
        )
            .into_response()
    }
}
