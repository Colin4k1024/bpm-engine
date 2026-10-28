//! OpenAPI ↔ implementation consistency suite (E2-4, issue #14).
//!
//! `docs/openapi.yaml` is the normative, testable REST contract. This
//! suite runs in the DEFAULT CI job (`cargo test --workspace`) and checks
//! the spec against the running implementation in five ways:
//!
//! 1. **Route parity (spec → impl)**: for every `(path, method)` in the
//!    spec, a request with the spec method is issued against the real
//!    router (memory backend). The route must EXIST: the response status
//!    must differ from a route-level 404. For handlers that legitimately
//!    answer 404 (unknown ids), the spec uses syntactically-valid but
//!    nonexistent ids and the body must be OUR error JSON (an `error`
//!    string field), never axum's plain-text route-not-found body.
//!    Wrong-method requests on existing paths answer 405, so a missing
//!    method shows up as a route-level 404 too.
//! 2. **Route parity (impl → spec)**: the routes the router ACTUALLY
//!    registers — derived at registration time via
//!    `routes::registered_routes()`, not a hand-maintained table — must
//!    appear in the spec, and vice versa (set equality): a route cannot
//!    ship without spec coverage and the spec cannot document a route
//!    that does not exist.
//! 3. **Error-code consistency**: the `ErrorResponse.code` enum in the
//!    spec must equal `routes::error_codes::ERROR_CODES` exactly (set
//!    equality), every string literal passed to `with_code(...)` /
//!    `bad_request_envelope(...)` in the routes source (parsed via
//!    `include_str!`) must be a registered code, and every
//!    `error_codes::X` identifier used in the source must be a declared
//!    const (whose registration is asserted by a routes unit test).
//! 4. **Sample-response schema smoke**: representative live responses
//!    (health, instance list with pagination, task list, an error body,
//!    a dry-run report incl. the serde-tagged `Terminus`, and deploy
//!    error bodies) validate against the spec's JSON schemas. Validation
//!    uses a small hand-rolled checker for the subset this spec uses
//!    (`$ref`/`type`/`required`/`properties`/`items`/
//!    `additionalProperties`/`enum`) rather than pulling in a full
//!    JSON-Schema crate: the spec is written in that subset by
//!    construction, the checker is ~100 auditable lines, and the CI
//!    contract stays lightweight.
//! 5. **Schema-subset enforcement**: the validator FAILS LOUDLY on any
//!    keyword outside its whitelist (see
//!    [`SUPPORTED_SCHEMA_KEYWORDS`]) — a spec schema the checker would
//!    silently ignore is a broken contract, so the whole spec is walked
//!    and unsupported keywords (oneOf/anyOf/format/minimum/...) are
//!    rejected with the offending path and keyword.
//! 6. **Agent tool catalog lockstep (E4-1, issue #19)**: the committed
//!    catalog served by `GET /capabilities`
//!    (`crates/server/rest/capabilities/agent-tools.json`) must equal
//!    what `docs/openapi.yaml` generates, byte for byte; the catalog
//!    must cover every operation and every canonical error code; and
//!    the endpoint must serve the committed artifact with the JSON
//!    content type, validating against the spec's AgentToolsCatalog
//!    schema.
//!
//! Memory driver — no Docker needed.

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::routes::{error_codes, registered_routes};
use bpm_engine_server_rest::{build_app, AppHandle};
use serde_json::Value;
use std::collections::BTreeSet;

/// The OpenAPI contract, embedded from the repo (no runtime file
/// dependency — the same file CI checks is the file the docs build uses).
const OPENAPI_YAML: &str = include_str!("../../../../docs/openapi.yaml");
/// The routes source, used to scan for code literals near their use sites.
const ROUTES_SRC: &str = include_str!("../src/routes.rs");
/// The committed agent tool catalog (E4-1), embedded from the repo —
/// the same bytes `GET /capabilities` serves.
const AGENT_TOOLS_JSON: &str = include_str!("../capabilities/agent-tools.json");

struct TestServer {
    base_url: String,
    handle: AppHandle,
    serve_task: tokio::task::JoinHandle<()>,
}

async fn spawn_server() -> TestServer {
    let config = BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        // High enough that the parity sweep (one request per spec
        // operation) never trips the limiter — 429s here would be noise.
        rate_limit: RateLimitConfig {
            requests_per_minute: 10_000,
        },
        ..Default::default()
    };
    let app = build_app(&config).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let handle = app;
    let router = handle.router.clone();
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
        handle,
        serve_task,
    }
}

impl TestServer {
    async fn stop(self) {
        self.handle.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Parsed spec handle: the document (converted to JSON for pointer
/// navigation) plus helpers.
struct Spec {
    doc: Value,
}

impl Spec {
    fn parse() -> Self {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(OPENAPI_YAML).expect("docs/openapi.yaml must be valid YAML");
        // YAML → JSON once: JSON pointers then work on plain objects.
        let doc = serde_json::to_value(yaml).expect("yaml must convert to json");
        Self { doc }
    }

    /// `(path, method)` pairs from the spec, with `{param}` placeholders
    /// converted to the axum `:param` form used by the registration
    /// registry.
    fn operations(&self) -> Vec<(String, String)> {
        let paths = self
            .doc
            .get("paths")
            .and_then(|p| p.as_object())
            .expect("spec must have a paths object");
        let mut out = Vec::new();
        for (path, item) in paths {
            for (method, _op) in item.as_object().expect("path item must be an object") {
                if !matches!(method.as_str(), "get" | "post" | "put" | "delete" | "patch") {
                    continue; // parameters/responses keys at the path-item level
                }
                let axum_path = path.replace('{', ":");
                let axum_path = axum_path.replace('}', "");
                out.push((axum_path, method.to_uppercase()));
            }
        }
        out.sort();
        out
    }

    /// The error-code enum values of the ErrorResponse schema.
    fn error_code_enum(&self) -> Vec<String> {
        let code_schema = self
            .doc
            .pointer("/components/schemas/ErrorResponse/properties/code")
            .expect("ErrorResponse.code must exist in the spec");
        code_schema
            .get("enum")
            .and_then(|e| e.as_array())
            .expect("ErrorResponse.code must be an enum")
            .iter()
            .map(|v| v.as_str().expect("enum values must be strings").to_string())
            .collect()
    }

    /// Resolve a schema node by JSON pointer path.
    fn schema(&self, pointer: &str) -> Value {
        self.doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("schema not found at {pointer}"))
            .clone()
    }
}

// --- 1+2: route parity ------------------------------------------------------

/// Substitute `:param` placeholders with a syntactically valid but
/// nonexistent identifier, so a 404 answer can only come from a handler
/// (unknown id), never from the router (unknown route).
fn concrete_path(path: &str) -> String {
    let missing_id = "00000000-0000-4000-8000-000000000000";
    let missing_compound = format!("{}:{}", missing_id, "00000000-node");
    path.replace(":id", missing_id)
        .replace(":session_id", missing_id)
        .replace(":key", "nonexistent-key")
        .replace(":task_id", &missing_compound)
}

#[tokio::test]
async fn every_spec_operation_exists_on_the_router() {
    let server = spawn_server().await;
    let spec = Spec::parse();
    let client = reqwest::Client::new();

    for (path, method) in spec.operations() {
        let url = format!("{}{}", server.base_url, concrete_path(&path));
        let req = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            &url,
        );
        // JSON bodies keep body-consuming handlers on their typed paths
        // (an invalid_json 4xx still proves the route exists).
        let req = if method == "POST" || method == "PUT" {
            req.header("content-type", "application/json").body("{}")
        } else {
            req
        };
        let resp = req.send().await.expect("request must succeed");
        let status = resp.status();
        if status.as_u16() == 404 {
            // A 404 is acceptable ONLY when it comes from a handler that
            // legitimately rejects unknown ids: the body must be our
            // ErrorResponse JSON (an `error` string field), not axum's
            // plain-text route-not-found body.
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let is_our_error = body.get("error").and_then(|e| e.as_str()).is_some();
            assert!(
                is_our_error,
                "spec operation {method} {path} answered 404 with a non-ErrorResponse body \
                 ({body}): the route or its handler does not exist as documented"
            );
        } else {
            // 405 (method not allowed on an existing path) would also
            // indicate a spec/impl method mismatch.
            assert_ne!(
                status.as_u16(),
                405,
                "spec operation {method} {path} answered 405: method mismatch with the router"
            );
        }
    }

    server.stop().await;
}

#[test]
fn registered_routes_and_spec_paths_are_the_same_set() {
    let spec = Spec::parse();
    let spec_ops: BTreeSet<(String, String)> = spec.operations().into_iter().collect();
    let mut registered: BTreeSet<(String, String)> = registered_routes()
        .into_iter()
        .map(|(m, p)| (p.to_string(), m.to_string()))
        .collect();
    // `GET /metrics` is registered only under the `observability` build
    // feature and is deliberately NOT part of the default wire contract
    // (documented in docs/openapi.yaml); drop it when present.
    registered.remove(&("GET".to_string(), "/metrics".to_string()));

    assert_eq!(
        spec_ops,
        registered,
        "the routes DERIVED from the actual registrations \
         (routes::registered_routes()) and docs/openapi.yaml must describe the \
         same (method, path) set; spec-only: {:?}, registered-only: {:?}",
        spec_ops.difference(&registered).collect::<Vec<_>>(),
        registered.difference(&spec_ops).collect::<Vec<_>>()
    );
}

// --- 3: error-code consistency ----------------------------------------------

#[test]
fn spec_error_code_enum_equals_the_canonical_error_codes() {
    let spec = Spec::parse();
    let spec_codes: BTreeSet<String> = spec.error_code_enum().into_iter().collect();
    let canonical: BTreeSet<String> = error_codes::ERROR_CODES
        .iter()
        .map(|c| c.to_string())
        .collect();

    assert_eq!(
        spec_codes,
        canonical,
        "docs/openapi.yaml ErrorResponse.code enum and routes::error_codes::ERROR_CODES \
         must be the same set; spec-only: {:?}, code-only: {:?}",
        spec_codes.difference(&canonical).collect::<Vec<_>>(),
        canonical.difference(&spec_codes).collect::<Vec<_>>()
    );
}

/// Extract quoted string literals that appear as arguments of
/// `with_code(...)` or `bad_request_envelope(...)` in the routes source.
///
/// Pragmatic source scan (the codes module is the single source of
/// truth; this catches a future literal that forgot to register a code).
/// Only literals with the code SHAPE — a pure lowercase snake_case word
/// — are considered; prose messages, format strings (they contain
/// spaces), and dynamic arguments (`e.code()`, const references) are
/// ignored.
fn code_literals_in_source(src: &str) -> BTreeSet<String> {
    /// Scan double-quoted string literals out of a source window
    /// (handling escaped quotes).
    fn quoted_literals(window: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = window;
        while let Some(open) = rest.find('"') {
            let after_open = &rest[open + 1..];
            let mut end = None;
            let mut escaped = false;
            for (idx, ch) in after_open.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                match ch {
                    '\\' => escaped = true,
                    '"' => {
                        end = Some(idx);
                        break;
                    }
                    _ => {}
                }
            }
            match end {
                Some(end) => {
                    out.push(after_open[..end].to_string());
                    rest = &after_open[end + 1..];
                }
                None => break,
            }
        }
        out
    }

    let mut out = BTreeSet::new();
    for marker in ["with_code(", "bad_request_envelope("] {
        let mut rest = src;
        while let Some(pos) = rest.find(marker) {
            // Argument window: the balanced-parenthesis region of the
            // call (string-aware, so `)` or `;` inside message literals
            // cannot truncate it).
            let after = &rest[pos + marker.len()..];
            let window = paren_region(after);
            for literal in quoted_literals(window) {
                let is_code_shape = !literal.is_empty()
                    && literal
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && literal
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_lowercase());
                if is_code_shape {
                    out.insert(literal);
                }
            }
            rest = after;
        }
    }
    out
}

/// Return the source region covering the balanced parentheses of a call
/// whose opening `(` has just been consumed: from the start until depth
/// returns to zero (string literals are skipped so their contents do not
/// affect the depth). Unbalanced input yields the rest of the text.
fn paren_region(src: &str) -> &str {
    let mut depth: usize = 1;
    let mut chars = src.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => {
                // Skip the string literal (handling escapes).
                while let Some((_, inner)) = chars.next() {
                    if inner == '\\' {
                        let _ = chars.next();
                    } else if inner == '"' {
                        break;
                    }
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &src[..idx];
                }
            }
            _ => {}
        }
    }
    src
}

#[test]
fn every_code_literal_used_in_routes_is_registered() {
    let canonical: BTreeSet<String> = error_codes::ERROR_CODES
        .iter()
        .map(|c| c.to_string())
        .collect();
    let literals = code_literals_in_source(ROUTES_SRC);
    for literal in &literals {
        assert!(
            canonical.contains(literal),
            "code literal {literal:?} is used in routes.rs but is not registered in \
             error_codes::ERROR_CODES (and therefore missing from docs/openapi.yaml)"
        );
    }
}

/// The literal scan above cannot see codes referenced through the
/// `error_codes::X` consts. This scan asserts every such identifier is a
/// const DECLARED in the module; their VALUES are checked for
/// registration by the routes unit test
/// `routes::tests::every_error_code_const_is_registered` (which names
/// every const explicitly), closing the loop: a code cannot be emitted
/// on either channel without being registered.
#[test]
fn error_codes_identifiers_used_in_routes_are_declared_consts() {
    const PREFIX: &str = "error_codes::";
    let mut rest = ROUTES_SRC;
    while let Some(pos) = rest.find(PREFIX) {
        let after = &rest[pos + PREFIX.len()..];
        let ident: String = after
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || *c == '_')
            .collect();
        if !ident.is_empty() {
            assert!(
                ROUTES_SRC.contains(&format!("pub const {ident}:")),
                "routes.rs uses {PREFIX}{ident}, but no matching `pub const {ident}` \
                 is declared in the error_codes module"
            );
        }
        rest = after;
    }
}

/// The middleware's typed codes are part of the same stable contract.
#[test]
fn middleware_codes_are_registered() {
    for code in ["unauthorized", "invalid_token", "forbidden", "rate_limited"] {
        assert!(
            error_codes::ERROR_CODES.contains(&code),
            "middleware code {code:?} must be part of the canonical error-code list"
        );
    }
}

// --- 4: sample-response schema smoke ----------------------------------------

/// The JSON-Schema keywords the validator below understands — its
/// WHITELIST. Any other keyword found in a schema of
/// `docs/openapi.yaml` is a LOUD failure: the checker would silently
/// ignore it, which is exactly how a spec drifts from reality (E2-4
/// review fix). If a schema genuinely needs a new construct, extend the
/// validator and this list together.
///
/// `description`, `default`, `deprecated`, and `title` are pure
/// documentation metadata with no validation semantics — safe to skip.
const SUPPORTED_SCHEMA_KEYWORDS: &[&str] = &[
    "$ref",
    "type",
    "nullable",
    "required",
    "properties",
    "items",
    "additionalProperties",
    "enum",
    "description",
    "default",
    "deprecated",
    "title",
];

/// Fail loudly when a schema uses keywords outside
/// [`SUPPORTED_SCHEMA_KEYWORDS`]. Called at every schema node the
/// validator visits (recursing into nested schemas; `$ref` targets are
/// covered by the spec-wide walk in
/// [`the_spec_uses_only_the_supported_schema_subset`]).
fn assert_schema_subset(schema: &Value, path: &str) {
    let Some(obj) = schema.as_object() else {
        return; // `enum` arrays, boolean additionalProperties, etc.
    };
    for keyword in obj.keys() {
        assert!(
            SUPPORTED_SCHEMA_KEYWORDS.contains(&keyword.as_str()),
            "{path}: schema keyword {keyword:?} is not in the supported subset \
             {SUPPORTED_SCHEMA_KEYWORDS:?} — the hand-rolled validator would \
             silently ignore it; rewrite the schema into the subset (or extend \
             the validator and the whitelist together)"
        );
    }
    if let Some(Value::Object(props)) = obj.get("properties") {
        for (name, prop) in props {
            assert_schema_subset(prop, &format!("{path}.properties.{name}"));
        }
    }
    if let Some(items) = obj.get("items") {
        assert_schema_subset(items, &format!("{path}.items"));
    }
    if let Some(additional) = obj.get("additionalProperties") {
        // Schema form only (the boolean form has no nested schema).
        if additional.as_object().is_some_and(|a| !a.is_empty()) {
            assert_schema_subset(additional, &format!("{path}.additionalProperties"));
        }
    }
}

/// Minimal JSON-Schema subset validator for the constructs this spec
/// uses: `$ref`, `type` (object/array/string/integer/number/boolean),
/// `required`, `properties`, `items`, `additionalProperties` (schema
/// form), and `enum`. Keywords outside [`SUPPORTED_SCHEMA_KEYWORDS`]
/// fail loudly instead of being silently ignored (E2-4 review fix).
fn validate(spec: &Spec, schema: &Value, value: &Value, path: &str) {
    assert_schema_subset(schema, path);

    // $ref resolution (local pointers only).
    if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        assert!(
            reference.starts_with("#/"),
            "only local refs are supported: {reference}"
        );
        let pointer = &reference[1..];
        let target = spec.schema(pointer);
        validate(spec, &target, value, path);
        return;
    }

    if let Some(expected) = schema.get("enum").and_then(|e| e.as_array()) {
        assert!(
            expected.contains(value),
            "{path}: {value:?} is not one of the enum values {expected:?}"
        );
        return;
    }

    // The schema `type` may be a string (`type: string`) or an array
    // (`type: ["string", "null"]` — the OpenAPI 3.1 nullable form). A
    // JSON null is only valid when "null" is an allowed member.
    let type_raw = schema.get("type");
    if let Some(types) = type_raw.and_then(|t| t.as_array()) {
        let names: Vec<&str> = types.iter().filter_map(|t| t.as_str()).collect();
        if value.is_null() {
            assert!(
                names.contains(&"null"),
                "{path}: null is not an allowed type here (allowed: {names:?})"
            );
            return;
        }
        let Some(type_name) = names.iter().find(|t| **t != "null") else {
            return;
        };
        validate_typed(spec, schema, type_name, value, path);
        return;
    }
    let Some(type_name) = type_raw.and_then(|t| t.as_str()) else {
        return; // enum-only or descriptive schema
    };
    validate_typed(spec, schema, type_name, value, path);
}

/// Type dispatch for a single concrete type name.
fn validate_typed(spec: &Spec, schema: &Value, type_name: &str, value: &Value, path: &str) {
    match type_name {
        "object" => {
            let obj = value
                .as_object()
                .unwrap_or_else(|| panic!("{path}: expected an object, got {value:?}"));
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for field in required {
                    let field = field.as_str().expect("required entries must be strings");
                    assert!(
                        obj.contains_key(field),
                        "{path}: required field {field:?} missing in {value:?}"
                    );
                }
            }
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (name, prop_schema) in props {
                    if let Some(actual) = obj.get(name) {
                        validate(spec, prop_schema, actual, &format!("{path}.{name}"));
                    }
                }
            }
            if let Some(additional) = schema.get("additionalProperties") {
                if let Some(additional_schema) = additional.as_object() {
                    if !additional_schema.is_empty() {
                        for (name, actual) in obj {
                            validate(spec, additional, actual, &format!("{path}.{name}"));
                        }
                    }
                }
            }
        }
        "array" => {
            let arr = value
                .as_array()
                .unwrap_or_else(|| panic!("{path}: expected an array, got {value:?}"));
            if let Some(items) = schema.get("items") {
                for (idx, item) in arr.iter().enumerate() {
                    validate(spec, items, item, &format!("{path}[{idx}]"));
                }
            }
        }
        "string" => assert!(
            value.is_string(),
            "{path}: expected a string, got {value:?}"
        ),
        "integer" => assert!(
            value.is_i64() || value.is_u64(),
            "{path}: expected an integer, got {value:?}"
        ),
        "number" => assert!(
            value.is_number(),
            "{path}: expected a number, got {value:?}"
        ),
        "boolean" => assert!(
            value.is_boolean(),
            "{path}: expected a boolean, got {value:?}"
        ),
        other => panic!("{path}: unsupported schema type {other:?} in the validator subset"),
    }
}

/// Deploy a minimal BPMN definition and start instances so the sampled
/// responses carry real data.
async fn seed(server: &TestServer, client: &reqwest::Client, instances: usize) {
    let bpmn = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="spec-probe" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(bpmn.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deploy should return 201");

    for _ in 0..instances {
        let resp = client
            .post(format!("{}/api/v1/process-instances", server.base_url))
            .json(&serde_json::json!({ "process_def_id": "spec-probe" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "start should return 201");
    }
}

#[tokio::test]
async fn sample_responses_validate_against_the_spec_schemas() {
    let server = spawn_server().await;
    let spec = Spec::parse();
    let client = reqwest::Client::new();
    seed(&server, &client, 3).await;

    // health
    let body: Value = client
        .get(format!("{}/health", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate(
        &spec,
        &spec.schema("/components/schemas/HealthResponse"),
        &body,
        "health",
    );

    // instance list with pagination
    let body: Value = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .query(&[("limit", "2"), ("offset", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    validate(
        &spec,
        &spec.schema("/components/schemas/InstanceListResponse"),
        &body,
        "instance-list",
    );
    assert_eq!(body["pagination"]["limit"], 2, "limit must be honored");
    assert_eq!(body["pagination"]["offset"], 1, "offset must be honored");
    assert_eq!(
        body["pagination"]["returned"].as_u64().unwrap(),
        body["instances"].as_array().unwrap().len() as u64
    );
    assert_eq!(
        body["instances"].as_array().map(|a| a.len()),
        Some(2),
        "the seeded page must contain exactly two instances"
    );

    // task list (user tasks of the running instances)
    let body: Value = client
        .get(format!("{}/api/v1/tasks", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let schema = serde_json::json!({
        "type": "array",
        "items": spec.schema("/components/schemas/TaskListItem"),
    });
    validate(&spec, &schema, &body, "task-list");
    assert_eq!(
        body.as_array().map(|a| a.len()),
        Some(3),
        "three seeded instances each wait at the user task"
    );

    // error response (404 not_found on an unknown instance id)
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/00000000-0000-4000-8000-000000000000",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    validate(
        &spec,
        &spec.schema("/components/schemas/ErrorResponse"),
        &body,
        "error-response",
    );
    assert_eq!(body["code"], "not_found");

    server.stop().await;
}

// --- 5: schema-subset enforcement (whole spec) -------------------------------

/// Walk the whole spec document and run [`assert_schema_subset`] on every
/// schema node: the named `components/schemas` entries (also the `$ref`
/// targets) plus every inline `schema:` position (path/operation
/// parameters, request bodies, responses, component parameters and
/// responses). A keyword the validator would silently ignore fails the
/// suite with its path and keyword.
#[test]
fn the_spec_uses_only_the_supported_schema_subset() {
    let spec = Spec::parse();

    let schemas = spec
        .doc
        .pointer("/components/schemas")
        .and_then(|s| s.as_object())
        .expect("spec must have components/schemas");
    for (name, schema) in schemas {
        assert_schema_subset(schema, &format!("/components/schemas/{name}"));
    }

    fn walk_inline_schemas(node: &Value, path: &str) {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    // `schema` positions: operation/path parameters,
                    // request bodies, media types, component parameters
                    // and responses. A mapping named `schema` that is not
                    // a schema object (none in this spec) is still
                    // keyword-checked harmlessly.
                    if key == "schema" {
                        assert_schema_subset(value, &format!("{path}/schema"));
                    } else {
                        walk_inline_schemas(value, &format!("{path}/{key}"));
                    }
                }
            }
            Value::Array(items) => {
                for (idx, item) in items.iter().enumerate() {
                    walk_inline_schemas(item, &format!("{path}/{idx}"));
                }
            }
            _ => {}
        }
    }
    walk_inline_schemas(&spec.doc, "");
}

// --- 6: dry-run and deploy-error sample validation ---------------------------

/// BPMN that parks the dry-run walk at a user task (`waitingAt`).
const DRY_RUN_WAITING_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="dry-wait" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

/// BPMN that walks straight to the end event (`completedAt`).
const DRY_RUN_COMPLETED_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="dry-end" isExecutable="true">
    <startEvent id="start"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;

/// BPMN that fails semantic compilation (no end event): deploy answers
/// 400 `{"errors": [...]}`, validate answers 200 with the same array as
/// `compile_errors` data.
const COMPILE_ERROR_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="broken-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="broken-work"/>
      </extensionElements>
    </serviceTask>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
  </process>
</definitions>"#;

/// POST a source document to an endpoint and return `(status, body)`.
async fn post_source(
    client: &reqwest::Client,
    base_url: &str,
    endpoint: &str,
    source: &str,
) -> (reqwest::StatusCode, Value) {
    let resp = client
        .post(format!("{base_url}/api/v1/process-definitions/{endpoint}"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "format": "bpmn", "source": source }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// The dry-run response — including the serde-tagged `Terminus` object —
/// must validate against the spec's `DryRunResponse` schema (E2-4 review
/// fix: the schema previously modeled a oneOf of per-variant field sets
/// that never matched the actual `{"type": ..., "node_id": ...}` wire
/// shape).
#[tokio::test]
async fn dry_run_response_validates_against_the_spec_schemas() {
    let server = spawn_server().await;
    let spec = Spec::parse();
    let client = reqwest::Client::new();

    // User task: the walk parks at `review` → waitingAt WITH a reason.
    let (status, body) =
        post_source(&client, &server.base_url, "dry-run", DRY_RUN_WAITING_BPMN).await;
    assert_eq!(status, 200, "dry-run must answer 200: {body}");
    validate(
        &spec,
        &spec.schema("/components/schemas/DryRunResponse"),
        &body,
        "dry-run-waiting",
    );
    assert_eq!(body["dry_run"]["terminus"]["type"], "waitingAt");
    assert_eq!(body["dry_run"]["terminus"]["node_id"], "review");
    assert!(
        body["dry_run"]["terminus"]["reason"].is_string(),
        "waitingAt carries a reason: {body}"
    );

    // Straight-through flow: the walk reaches `end` → completedAt,
    // WITHOUT a reason field on the wire.
    let (status, body) =
        post_source(&client, &server.base_url, "dry-run", DRY_RUN_COMPLETED_BPMN).await;
    assert_eq!(status, 200, "dry-run must answer 200: {body}");
    validate(
        &spec,
        &spec.schema("/components/schemas/DryRunResponse"),
        &body,
        "dry-run-completed",
    );
    assert_eq!(body["dry_run"]["terminus"]["type"], "completedAt");
    assert_eq!(body["dry_run"]["terminus"]["node_id"], "end");
    assert!(
        body["dry_run"]["terminus"].get("reason").is_none(),
        "completedAt must not carry a reason: {body}"
    );

    server.stop().await;
}

/// The deploy 400 error bodies (untagged serde enum: `{"error": ...}` on
/// parse failure, `{"errors": [...]}` on compile failure) and the
/// validate 200 `compile_errors` data arm must validate against the
/// spec's combined `DeployError` schema and the concrete element schemas
/// (E2-4 review fix: these were previously a oneOf the subset validator
/// silently ignored).
#[tokio::test]
async fn deploy_and_validate_error_bodies_validate_against_the_spec_schemas() {
    let server = spawn_server().await;
    let spec = Spec::parse();
    let client = reqwest::Client::new();

    // Compile-error deploy: 400 with {"errors": [CompilerError...]}.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(COMPILE_ERROR_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "compile errors are a 400 at deploy");
    let body: Value = resp.json().await.unwrap();
    validate(
        &spec,
        &spec.schema("/components/schemas/DeployError"),
        &body,
        "deploy-compile-error",
    );
    let errors = body["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty(), "at least one compile error: {body}");
    for (idx, err) in errors.iter().enumerate() {
        // The concrete element shape, including the nullable
        // node_id/flow_id/hint fields (serde serializes None as null).
        validate(
            &spec,
            &spec.schema("/components/schemas/CompilerError"),
            err,
            &format!("deploy-compile-error.errors[{idx}]"),
        );
    }
    assert!(
        body.get("error").is_none(),
        "the compile shape carries errors, not error: {body}"
    );

    // Parse-error deploy: 400 with {"error": "..."}.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body("<not-bpmn".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "parse errors are a 400 at deploy");
    let body: Value = resp.json().await.unwrap();
    validate(
        &spec,
        &spec.schema("/components/schemas/DeployError"),
        &body,
        "deploy-parse-error",
    );
    // The strict single-message shape still matches.
    validate(
        &spec,
        &spec.schema("/components/schemas/DeployErrorParse"),
        &body,
        "deploy-parse-error-strict",
    );
    assert!(
        body["error"].is_string(),
        "the parse shape carries error: {body}"
    );
    assert!(
        body.get("errors").is_none(),
        "the parse shape must not carry errors: {body}"
    );

    // Validate: the same compile failure comes back as 200 DATA
    // (valid: false + compile_errors — the array arm of the untagged
    // CompileErrorsView).
    let (status, body) =
        post_source(&client, &server.base_url, "validate", COMPILE_ERROR_BPMN).await;
    assert_eq!(status, 200, "validate returns compile errors as data");
    validate(
        &spec,
        &spec.schema("/components/schemas/ValidateResponse"),
        &body,
        "validate-compile-errors",
    );
    let errors = body["compile_errors"].as_array().expect("compile_errors");
    for (idx, err) in errors.iter().enumerate() {
        validate(
            &spec,
            &spec.schema("/components/schemas/CompilerError"),
            err,
            &format!("validate-compile-errors[{idx}]"),
        );
    }

    server.stop().await;
}

// --- 7: agent tool catalog (E4-1, issue #19) ----------------------------------

/// Lockstep: regenerating the catalog from the spec must reproduce the
/// committed artifact byte for byte (the DSL `schema_lockstep` pattern)
/// — the catalog can never drift from the contract it describes.
#[test]
fn capabilities_catalog_matches_generated_output() {
    let spec = Spec::parse();
    let generated = bpm_engine_server_rest::capabilities::generate_catalog(&spec.doc);
    assert_eq!(
        generated, AGENT_TOOLS_JSON,
        "crates/server/rest/capabilities/agent-tools.json drifted from what \
         docs/openapi.yaml generates; regenerate with `cargo run -p \
         bpm-engine-server-rest --example dump_capabilities`"
    );
}

/// Structural catalog contract: header fields, one example per DISTINCT
/// canonical error code (set equality), one tool per spec operation,
/// every tool's operation exists in the spec, the classification and
/// auth rules hold, and every error code a tool lists is canonical.
#[test]
fn capabilities_catalog_covers_the_whole_contract() {
    let catalog: Value =
        serde_json::from_str(AGENT_TOOLS_JSON).expect("the committed catalog must be valid JSON");
    let spec = Spec::parse();

    assert_eq!(catalog["catalog_version"], "1");
    assert_eq!(catalog["api_version"], spec.doc["info"]["version"]);
    assert_eq!(catalog["generated_from"], "docs/openapi.yaml");
    assert!(
        catalog["regenerate"]
            .as_str()
            .is_some_and(|cmd| cmd.contains("dump_capabilities")),
        "the catalog header must document the regeneration command"
    );

    // One example per DISTINCT canonical code — set equality with the
    // single source of truth, routes::error_codes::ERROR_CODES.
    let canonical: BTreeSet<String> = error_codes::ERROR_CODES
        .iter()
        .map(|c| c.to_string())
        .collect();
    let examples: BTreeSet<String> = catalog["error_examples"]
        .as_object()
        .expect("error_examples must be an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        examples, canonical,
        "the catalog must carry one example per DISTINCT canonical error code"
    );
    for (code, example) in catalog["error_examples"]
        .as_object()
        .expect("error_examples")
    {
        // The example body is a self-consistent ErrorResponse: the code
        // field names the map key, the status is a 4xx/5xx.
        assert_eq!(
            example["body"]["code"],
            code.as_str(),
            "example body code must match the map key"
        );
        let status = example["status"].as_u64().expect("example status");
        assert!(
            (400..600).contains(&status),
            "example status for {code:?} must be an error status, got {status}"
        );
    }

    // One tool per spec operation; every tool documents a REAL spec
    // operation (in the spec's {param} form).
    let spec_ops: BTreeSet<(String, String)> = spec
        .operations()
        .into_iter()
        .map(|(path, method)| (method, path))
        .collect();
    let tools = catalog["tools"].as_array().expect("tools must be an array");
    assert_eq!(
        tools.len(),
        spec_ops.len(),
        "the catalog must carry exactly one tool per spec operation"
    );
    for tool in tools {
        let operation = tool["operation"].as_str().expect("operation string");
        let (method, path) = operation
            .split_once(' ')
            .unwrap_or_else(|| panic!("operation must be \"<METHOD> <path>\": {operation:?}"));
        // The catalog keeps the spec's {param} form; the operations
        // helper normalizes to the axum :param form — convert alike.
        let axum_path = path.replace('{', ":").replace('}', "");
        assert!(
            spec_ops.contains(&(method.to_string(), axum_path)),
            "tool {} documents an operation that is not in the spec: {operation}",
            tool["name"].as_str().unwrap_or("<unnamed>")
        );

        // Classification: GET → read_only; the two side-effect-free
        // POSTs → read_only; every other POST/PUT/DELETE → mutating.
        let name = tool["name"].as_str().expect("tool name");
        let expected_classification = if method == "GET"
            || matches!(
                name,
                "validate_process_definition" | "dry_run_process_definition"
            ) {
            "read_only"
        } else {
            "mutating"
        };
        assert_eq!(
            tool["classification"], expected_classification,
            "wrong classification for {operation}"
        );
        assert!(
            tool["side_effects"].is_string(),
            "every tool must document side_effects: {operation}"
        );

        // Auth: the /api/v1 operations sit behind the X-API-Key layer
        // (when the server has one configured); the root routes do not.
        assert_eq!(
            tool["auth_required"].as_bool().expect("auth_required bool"),
            path.starts_with("/api/v1"),
            "auth_required must be true exactly for /api/v1 operations: {operation}"
        );

        // Every error code a tool lists is a canonical code.
        for err in tool["responses"]["errors"]
            .as_array()
            .expect("errors must be an array")
        {
            if let Some(code) = err["code"].as_str() {
                assert!(
                    canonical.contains(code),
                    "tool {name} lists non-canonical error code {code:?}"
                );
            }
        }
    }
}

/// GET /capabilities serves the committed catalog: 200, JSON content
/// type, the exact committed bytes, and a body that validates against
/// the spec's AgentToolsCatalog schema (one tool per operation).
#[tokio::test]
async fn capabilities_endpoint_serves_the_committed_catalog() {
    let server = spawn_server().await;
    let spec = Spec::parse();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/capabilities", server.base_url))
        .send()
        .await
        .expect("request must succeed");
    assert_eq!(resp.status(), 200, "the catalog endpoint must answer 200");
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .expect("content-type header");
    assert!(
        content_type.starts_with("application/json"),
        "the catalog must be served as JSON, got {content_type:?}"
    );
    let body = resp.text().await.expect("body must be readable");
    assert_eq!(
        body, AGENT_TOOLS_JSON,
        "GET /capabilities must serve the committed catalog verbatim"
    );

    let catalog: Value = serde_json::from_str(&body).expect("the served catalog must be JSON");
    assert_eq!(catalog["catalog_version"], "1");
    assert_eq!(
        catalog["tools"].as_array().map(Vec::len),
        Some(spec.operations().len()),
        "the served catalog must carry one tool per spec operation"
    );
    validate(
        &spec,
        &spec.schema("/components/schemas/AgentToolsCatalog"),
        &catalog,
        "capabilities",
    );

    server.stop().await;
}
