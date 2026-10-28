//! Middleware: authentication (JWT / API key / open mode), RBAC (E5-1),
//! basic rate limiting, and request-id injection.
//!
//! # Authentication modes (E5-1, issue #23)
//!
//! Precedence, per request:
//!
//! 1. **JWT** — when `BPM_JWT_SECRET` is configured (from `auth.jwt_secret`
//!    / `BPM_AUTH__JWT_SECRET`), `/api/v1` requests carry
//!    `Authorization: Bearer <jwt>` (HS256). Signature/expiry/claims are
//!    verified and become the request [`Identity`]; failures answer 401
//!    `invalid_token`. A JWT request's tenant comes FROM THE CLAIMS: the
//!    client `X-Tenant-Id` header is deprecated for this mode and a header
//!    disagreeing with the claim is rejected with 403 `forbidden`.
//! 2. **API key** — when no bearer token is presented, a configured
//!    `BPM_API_KEY` (from `auth.api_key`) authenticates the request as the
//!    legacy admin/bootstrap credential with FULL access; `X-Tenant-Id` is
//!    honored (untrusted client input, as before E5-1). Missing or invalid
//!    key answers 401 `unauthorized`.
//! 3. **Open** — with neither secret configured the server accepts every
//!    request (PoC mode); `X-Tenant-Id` remains a dev-mode affordance.
//!
//! # RBAC
//!
//! JWT-authenticated (principal) requests are authorized against the
//! permission table below (method + path pattern → roles); a role without
//! permission answers 403 `forbidden`. The admin API key bypasses role
//! checks (documented bootstrap path), and open mode has no credentials to
//! authorize. Cross-tenant isolation is enforced in the handlers from the
//! verified tenant claim (see `docs/rbac.md`).

use crate::routes::{error_codes, ErrorResponse};
use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::warn;

// --- Identity (E5-1) ---------------------------------------------------------

/// The role of a JWT-verified principal (E5-1). Exactly these four roles
/// exist; the claim string must match one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Deploys, activates, and deprecates process definitions.
    Deployer,
    /// Starts instances, completes user tasks, replays, operates the DLQ.
    Operator,
    /// Fetches/locks, completes, fails, and extends external tasks.
    Worker,
    /// Reads everything (all reads are role-agnostic, but this is the
    /// read-only role by design).
    Auditor,
}

impl Role {
    /// The wire string of the role (the JWT `role` claim value).
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Deployer => "deployer",
            Role::Operator => "operator",
            Role::Worker => "worker",
            Role::Auditor => "auditor",
        }
    }

    /// Parse the JWT `role` claim; unknown strings are invalid tokens.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "deployer" => Some(Role::Deployer),
            "operator" => Some(Role::Operator),
            "worker" => Some(Role::Worker),
            "auditor" => Some(Role::Auditor),
            _ => None,
        }
    }
}

/// Every defined role, in declaration order (documentation/test surface).
pub const ROLES: &[Role] = &[Role::Deployer, Role::Operator, Role::Worker, Role::Auditor];

/// JWT claims (E5-1): HS256, `{ sub, tenant, role, exp [, iat] }`.
///
/// `sub` is the actor, `tenant` the tenant scope (non-empty), `role` one of
/// the four [`Role`] strings, `exp` the unix-seconds expiry (enforced with
/// zero leeway).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Actor (subject): the verified principal identity.
    pub sub: String,
    /// Tenant id: the request's tenant scope. Non-empty.
    pub tenant: String,
    /// Role: `deployer` | `operator` | `worker` | `auditor`.
    pub role: String,
    /// Expiry, unix seconds. Required and enforced.
    pub exp: u64,
    /// Issued-at, unix seconds. Optional metadata (not required, not
    /// validated); absent from the token when not minted with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<u64>,
}

/// Mint an HS256 JWT with the given claims fields (operator/test helper).
///
/// `role` must be a known [`Role`] string. The token is signed with
/// `secret`; use the same secret as `auth.jwt_secret` / `BPM_JWT_SECRET`
/// on the server. See `examples/mint_jwt.rs` for the CLI wrapper.
pub fn mint_jwt(
    secret: &str,
    sub: &str,
    tenant: &str,
    role: &str,
    exp: u64,
) -> anyhow::Result<String> {
    if Role::parse(role).is_none() {
        anyhow::bail!("unknown role {role:?}: expected one of deployer, operator, worker, auditor");
    }
    let claims = JwtClaims {
        sub: sub.to_string(),
        tenant: tenant.to_string(),
        role: role.to_string(),
        exp,
        iat: None,
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

/// The verified identity of a request (E5-1), inserted into request
/// extensions by [`authenticate`] and read by handlers.
#[derive(Debug, Clone)]
pub enum Identity {
    /// JWT-verified principal: actor/tenant/role from verified claims.
    /// The tenant comes from the claim; a disagreeing `X-Tenant-Id` header
    /// was already rejected by the middleware.
    Principal {
        /// Verified actor (the `sub` claim).
        actor: String,
        /// Verified tenant (the `tenant` claim).
        tenant: String,
        /// Verified role (the `role` claim).
        role: Role,
    },
    /// Legacy admin API key (bootstrap path): full access, no role checks;
    /// `X-Tenant-Id` honored as untrusted client input.
    ApiKey { tenant: Option<String> },
    /// Open mode (no credentials configured): anonymous; `X-Tenant-Id`
    /// honored as the pre-E5-1 dev-mode affordance.
    Open { tenant: Option<String> },
}

impl Identity {
    /// The tenant scope for store queries and engine contexts.
    ///
    /// [`Identity::Principal`] → the verified claim; the other identities →
    /// the (untrusted) `X-Tenant-Id` header value, as before E5-1.
    pub fn tenant(&self) -> Option<&str> {
        match self {
            Identity::Principal { tenant, .. } => Some(tenant.as_str()),
            Identity::ApiKey { tenant } | Identity::Open { tenant } => tenant.as_deref(),
        }
    }

    /// The JWT role, when the identity is a verified principal.
    pub fn role(&self) -> Option<Role> {
        match self {
            Identity::Principal { role, .. } => Some(*role),
            _ => None,
        }
    }

    /// The actor identity of the request (for logging; E5-3 will persist
    /// it into the audit trail — not this issue's scope).
    pub fn actor(&self) -> &str {
        match self {
            Identity::Principal { actor, .. } => actor.as_str(),
            Identity::ApiKey { .. } => "api-key",
            Identity::Open { .. } => "anonymous",
        }
    }
}

/// Read a non-empty env var (empty means "not configured").
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Extract the bearer token from `Authorization: Bearer <token>`.
/// `Err(())` when the header exists but is not a bearer header.
fn bearer_token(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return Ok(None);
    };
    let token = value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty());
    match token {
        Some(token) => Ok(Some(token)),
        None => Err(()),
    }
}

/// Extract the `X-Tenant-Id` header (untrusted client input).
fn tenant_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

/// Verify a JWT against the server secret → the principal's
/// `(actor, tenant, role)` claim values.
fn verify_jwt(token: &str, secret: &str) -> Result<(String, String, Role), String> {
    let mut validation = Validation::new(Algorithm::HS256);
    // Strict expiry: no leeway (the engine's tokens are short-lived).
    validation.leeway = 0;
    let data = decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| format!("token rejected: {}", e))?;
    let claims = data.claims;
    if claims.sub.is_empty() {
        return Err("sub claim must not be empty".to_string());
    }
    if claims.tenant.is_empty() {
        return Err("tenant claim must not be empty".to_string());
    }
    let role = Role::parse(&claims.role)
        .ok_or_else(|| format!("unknown role {:?} in token", claims.role))?;
    Ok((claims.sub, claims.tenant, role))
}

fn unauthorized_response(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse::with_code(msg, error_codes::UNAUTHORIZED)),
    )
        .into_response()
}

fn invalid_token_response(reason: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse::with_code(
            format!("invalid or missing JWT: {reason}"),
            error_codes::INVALID_TOKEN,
        )),
    )
        .into_response()
}

fn forbidden_response(msg: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse::with_code(msg, error_codes::FORBIDDEN)),
    )
        .into_response()
}

/// Authentication + RBAC middleware (E5-1). See the module docs for the
/// mode precedence; the verified [`Identity`] is inserted into the request
/// extensions for handlers.
///
/// Rejections use the standard [`ErrorResponse`] shape with the stable
/// codes `unauthorized` (401, API-key path), `invalid_token` (401, JWT
/// path), and `forbidden` (403, tenant mismatch or RBAC denial).
pub async fn authenticate(mut request: Request, next: Next) -> Response {
    let jwt_secret = non_empty_env("BPM_JWT_SECRET");
    let api_key = non_empty_env("BPM_API_KEY");
    let headers = request.headers().clone();

    let identity = match (&jwt_secret, bearer_token(&headers)) {
        (Some(secret), Ok(Some(token))) => match verify_jwt(token, secret) {
            Ok((actor, tenant, role)) => {
                // X-Tenant-Id deprecation (E5-1): the tenant comes from the
                // verified claim; a DISAGREEING header is rejected instead of
                // silently overridden. An absent or agreeing header is fine.
                if let Some(header_tenant) = tenant_header(&headers) {
                    if header_tenant != tenant {
                        warn!(
                            claim_tenant = %tenant,
                            "rejecting request with a conflicting X-Tenant-Id header"
                        );
                        return forbidden_response(
                            "tenant mismatch: the X-Tenant-Id header conflicts with \
                             the token's tenant claim",
                        );
                    }
                }
                Identity::Principal {
                    actor,
                    tenant,
                    role,
                }
            }
            Err(reason) => return invalid_token_response(&reason),
        },
        // JWT enforced but no bearer token: the legacy admin API key is the
        // documented bootstrap fallback when one is configured.
        (Some(_), Ok(None)) => match &api_key {
            Some(expected) => match headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
                Some(provided) if provided == expected => Identity::ApiKey {
                    tenant: tenant_header(&headers),
                },
                Some(_) => return unauthorized_response("missing or invalid API key"),
                None => {
                    return invalid_token_response(
                        "missing bearer token (JWT auth is configured; \
                         send Authorization: Bearer <jwt>, or the admin X-API-Key)",
                    )
                }
            },
            None => {
                return invalid_token_response(
                    "missing bearer token (JWT auth is configured; \
                     send Authorization: Bearer <jwt>)",
                )
            }
        },
        // Malformed Authorization header under JWT enforcement.
        (Some(_), Err(())) => {
            return invalid_token_response(
                "Authorization header must be \"Bearer <token>\" when JWT auth is configured",
            )
        }
        // No JWT secret configured: legacy behavior — open mode, or the
        // optional API-key layer (401 `unauthorized` when enforced).
        (None, _) => match &api_key {
            Some(expected) => match headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
                Some(provided) if provided == expected => Identity::ApiKey {
                    tenant: tenant_header(&headers),
                },
                _ => return unauthorized_response("missing or invalid API key"),
            },
            None => Identity::Open {
                tenant: tenant_header(&headers),
            },
        },
    };

    // RBAC (E5-1): enforced for verified principals only — the admin API
    // key is the full-access bootstrap path, and open mode has no
    // credentials to authorize.
    if let Identity::Principal { role, .. } = &identity {
        let method = request.method().as_str();
        // The middleware sits inside the /api/v1 nest, so the URI path is
        // the prefix-stripped route path (e.g. "/process-instances/:id").
        let path = request.uri().path();
        match permission_for(method, path) {
            Some(roles) if roles.contains(role) => {}
            Some(_) => {
                warn!(method, path, role = role.as_str(), "RBAC denied request");
                return forbidden_response(&format!(
                    "role '{}' is not allowed to {} /api/v1{}",
                    role.as_str(),
                    method,
                    path
                ));
            }
            None => {
                // Fail closed: a registered route missing from the table is
                // a table bug — deny loudly rather than silently allow.
                tracing::error!(
                    method,
                    path,
                    "authenticated request hit a route with no RBAC rule; \
                     extend PERMISSIONS (fail closed)"
                );
                return forbidden_response("access denied");
            }
        }
    }

    request.extensions_mut().insert(identity);
    next.run(request).await
}

// --- RBAC permission table (E5-1) ---------------------------------------------

/// Path-segment wildcard: matches exactly one path parameter (`:id` etc.).
/// `*` never appears in the registered literal route segments.
const ANY: &str = "*";

/// Normalize a concrete request path into segments: leading `/` stripped,
/// empty segments dropped ("/tasks" → ["tasks"]).
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// One permission rule: `(method, path segments, allowed roles)`. A rule
/// matches a request when the method is equal, the segment count is equal,
/// and every segment is either literally equal or [`ANY`] in the rule.
const PERMISSIONS: &[(&str, &[&str], &[Role])] = &[
    // Reads: every authenticated role may read (E5-1 — cross-tenant reads
    // are rejected by tenancy in the handlers, not by role).
    ("GET", &["process-instances"], ROLES),
    ("GET", &["process-instances", ANY], ROLES),
    ("GET", &["process-instances", ANY, "trace"], ROLES),
    ("GET", &["process-instances", ANY, "history"], ROLES),
    ("GET", &["process-instances", ANY, "steps"], ROLES),
    // E5-2: the approval list of an instance is a read (operators use it
    // to discover pending approval ids; auditors inspect the records).
    ("GET", &["process-instances", ANY, "approvals"], ROLES),
    ("GET", &["replay", ANY, "snapshot"], ROLES),
    ("GET", &["process-definitions"], ROLES),
    ("GET", &["process-definitions", ANY], ROLES),
    ("GET", &["process-definitions", "versions", ANY], ROLES),
    ("GET", &["process-definitions", "active", ANY], ROLES),
    ("GET", &["tasks"], ROLES),
    ("GET", &["tasks", ANY, "form"], ROLES),
    ("GET", &["dead-letters"], ROLES),
    ("GET", &["dead-letters", ANY], ROLES),
    ("GET", &["invariants", "check"], ROLES),
    // Side-effect-free POSTs (E2-2 validate/dry-run): all roles.
    ("POST", &["process-definitions", "validate"], ROLES),
    ("POST", &["process-definitions", "dry-run"], ROLES),
    // Definition lifecycle: deployer only.
    (
        "POST",
        &["process-definitions", "deploy"],
        &[Role::Deployer],
    ),
    (
        "PUT",
        &["process-definitions", ANY, "activate"],
        &[Role::Deployer],
    ),
    (
        "PUT",
        &["process-definitions", ANY, "deprecate"],
        &[Role::Deployer],
    ),
    // Instance/task operations: operator only.
    ("POST", &["process-instances"], &[Role::Operator]),
    (
        "POST",
        &["process-instances", ANY, "replay"],
        &[Role::Operator],
    ),
    ("POST", &["replay", ANY, "step"], &[Role::Operator]),
    ("POST", &["replay", ANY, "seek"], &[Role::Operator]),
    ("DELETE", &["replay", ANY], &[Role::Operator]),
    ("POST", &["tasks", ANY, "complete"], &[Role::Operator]),
    // E5-2 instance lifecycle + approvals: operator actions (decide, pause,
    // resume, cancel — the operator role owns instance lifecycle per
    // ADR-008 §9).
    ("POST", &["approvals", ANY, "decide"], &[Role::Operator]),
    (
        "POST",
        &["process-instances", ANY, "pause"],
        &[Role::Operator],
    ),
    (
        "POST",
        &["process-instances", ANY, "resume"],
        &[Role::Operator],
    ),
    (
        "POST",
        &["process-instances", ANY, "cancel"],
        &[Role::Operator],
    ),
    ("POST", &["dead-letters", ANY, "requeue"], &[Role::Operator]),
    ("DELETE", &["dead-letters", ANY], &[Role::Operator]),
    // External-task lease operations: worker ONLY (E5-1 decision — the
    // worker role owns the lease lifecycle; operators administrate via the
    // DLQ, not by completing workers' tasks).
    (
        "POST",
        &["external-tasks", "fetch-and-lock"],
        &[Role::Worker],
    ),
    (
        "POST",
        &["external-tasks", ANY, "complete"],
        &[Role::Worker],
    ),
    ("POST", &["external-tasks", ANY, "fail"], &[Role::Worker]),
    (
        "POST",
        &["external-tasks", ANY, "extend-lock"],
        &[Role::Worker],
    ),
];

/// The roles allowed to call `(method, path)`, or `None` when no rule
/// matches (fail closed by the caller).
fn permission_for(method: &str, path: &str) -> Option<&'static [Role]> {
    let segments = path_segments(path);
    PERMISSIONS
        .iter()
        .find(|(rule_method, rule_segments, _)| {
            rule_method == &method
                && rule_segments.len() == segments.len()
                && rule_segments
                    .iter()
                    .zip(segments.iter())
                    .all(|(rule, seg)| *rule == ANY || rule == seg)
        })
        .map(|(_, _, roles)| *roles)
}

// --- Rate limiting ------------------------------------------------------------

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
///
/// Rejections use the standard [`ErrorResponse`] shape with the stable
/// `rate_limited` code (E2-4); the body previously had only the `error`
/// field, so this is an additive change.
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
            Json(ErrorResponse::with_code(
                "rate limit exceeded",
                error_codes::RATE_LIMITED,
            )),
        )
            .into_response()
    }
}

// --- Request id ---------------------------------------------------------------

/// Request-id middleware.
///
/// Generates a UUID for every incoming request, stores it in request extensions,
/// and returns it as `X-Request-Id` in the response headers.
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Insert into request extensions so handlers can access it
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let method = request.method().to_string();
    let uri = request.uri().to_string();

    // Log request start. Do NOT hold a span guard across .await (non-Send).
    tracing::info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        "incoming request"
    );

    let mut response = next.run(request).await;

    // Set X-Request-Id in response
    if let Ok(hv) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("X-Request-Id", hv);
    }

    response
}

/// Typed extractor for the request id, stored in request extensions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RequestId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_parse_and_roundtrip() {
        for role in ROLES {
            assert_eq!(Role::parse(role.as_str()), Some(*role));
        }
        assert!(Role::parse("admin").is_none());
        assert!(Role::parse("").is_none());
    }

    #[test]
    fn mint_jwt_requires_a_known_role() {
        assert!(mint_jwt("s", "a", "t", "superuser", 99).is_err());
        // Far-future expiry so the token verifies.
        let exp = 4_102_444_800; // 2100-01-01
        let token = mint_jwt("secret", "alice", "acme", "operator", exp).unwrap();
        let principal = verify_jwt(&token, "secret").unwrap();
        assert_eq!(
            principal,
            ("alice".to_string(), "acme".to_string(), Role::Operator)
        );
        // Wrong secret does not verify.
        assert!(verify_jwt(&token, "other-secret").is_err());
        // An expired token is rejected (strict expiry, zero leeway).
        let expired = mint_jwt("secret", "alice", "acme", "operator", 1).unwrap();
        let reason = verify_jwt(&expired, "secret").unwrap_err();
        assert!(
            reason.contains("ExpiredSignature"),
            "expired token rejection should name the cause, got {reason:?}"
        );
    }

    #[test]
    fn verify_jwt_rejects_bad_claims() {
        use jsonwebtoken::{encode, Header};
        let key = EncodingKey::from_secret(b"secret");
        let mk = |sub: &str, tenant: &str, role: &str| {
            let claims = JwtClaims {
                sub: sub.to_string(),
                tenant: tenant.to_string(),
                role: role.to_string(),
                exp: 4_102_444_800, // far future
                iat: None,
            };
            encode(&Header::new(Algorithm::HS256), &claims, &key).unwrap()
        };
        assert!(verify_jwt(&mk("", "acme", "operator"), "secret").is_err());
        assert!(verify_jwt(&mk("a", "", "operator"), "secret").is_err());
        assert!(verify_jwt(&mk("a", "acme", "root"), "secret").is_err());
    }

    #[test]
    fn bearer_token_parses_only_bearer_scheme() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), Ok(None));
        headers.insert("authorization", HeaderValue::from_static("Bearer  tok"));
        assert_eq!(bearer_token(&headers), Ok(Some("tok")));
        // Not a bearer header → Err (malformed under JWT enforcement).
        headers.insert("authorization", HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer_token(&headers), Err(()));
        headers.insert("authorization", HeaderValue::from_static("Bearer"));
        assert_eq!(bearer_token(&headers), Err(()));
    }

    #[test]
    fn permission_table_covers_every_registered_api_route() {
        // Drift guard: every route the router actually registers under
        // /api/v1 must have an RBAC rule — otherwise authenticated requests
        // to it would be denied by the fail-closed default.
        let routes = crate::routes::registered_routes();
        let api_routes: Vec<_> = routes
            .into_iter()
            .filter(|(_, path)| path.starts_with("/api/v1"))
            .collect();
        assert!(!api_routes.is_empty());
        for (method, path) in api_routes {
            let stripped = path.strip_prefix("/api/v1").unwrap();
            // Convert the registration's `:param` segments to ANY wildcards
            // and check a representative concrete path matches a rule.
            let concrete: String = stripped
                .split('/')
                .map(|seg| if seg.starts_with(':') { "x" } else { seg })
                .collect::<Vec<_>>()
                .join("/");
            assert!(
                permission_for(method, &concrete).is_some(),
                "no RBAC rule for {method} {path} — extend PERMISSIONS"
            );
        }
    }

    #[test]
    fn permission_matrix_decisions() {
        let all = ROLES;
        let deployer_only = &[Role::Deployer];
        let operator_only = &[Role::Operator];
        let worker_only = &[Role::Worker];
        let cases: &[(&str, &str, &[Role])] = &[
            ("GET", "/process-instances", all),
            ("GET", "/process-instances/i/steps", all),
            ("GET", "/dead-letters/dl", all),
            ("GET", "/invariants/check", all),
            ("POST", "/process-definitions/validate", all),
            ("POST", "/process-definitions/dry-run", all),
            ("POST", "/process-definitions/deploy", deployer_only),
            ("PUT", "/process-definitions/d/activate", deployer_only),
            ("PUT", "/process-definitions/d/deprecate", deployer_only),
            ("POST", "/process-instances", operator_only),
            ("POST", "/process-instances/i/replay", operator_only),
            // E5-2: approvals + instance lifecycle are operator actions.
            ("POST", "/approvals/a/decide", operator_only),
            ("POST", "/process-instances/i/pause", operator_only),
            ("POST", "/process-instances/i/resume", operator_only),
            ("POST", "/process-instances/i/cancel", operator_only),
            // ...while the approval LIST is a read (all roles).
            ("GET", "/process-instances/i/approvals", all),
            ("POST", "/replay/s/step", operator_only),
            ("DELETE", "/replay/s", operator_only),
            ("POST", "/tasks/t/complete", operator_only),
            ("POST", "/dead-letters/d/requeue", operator_only),
            ("DELETE", "/dead-letters/d", operator_only),
            ("POST", "/external-tasks/fetch-and-lock", worker_only),
            ("POST", "/external-tasks/t/complete", worker_only),
            ("POST", "/external-tasks/t/fail", worker_only),
            ("POST", "/external-tasks/t/extend-lock", worker_only),
        ];
        for (method, path, expected) in cases {
            assert_eq!(
                permission_for(method, path),
                Some(*expected),
                "wrong roles for {method} {path}"
            );
        }
        // Unknown paths and methods fail closed.
        assert_eq!(permission_for("GET", "/nope"), None);
        assert_eq!(permission_for("PATCH", "/process-instances"), None);
        // GET of the deploy POST path is a definition read (`:id` rule),
        // not a deploy.
        assert_eq!(
            permission_for("GET", "/process-definitions/deploy"),
            Some(all)
        );
    }
}
