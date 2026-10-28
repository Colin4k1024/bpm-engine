//! Public API surface snapshot for the `bpm-engine` facade (issue #21,
//! E4-3: 版本兼容测试 / version compatibility tests).
//!
//! This file is the semver tripwire for the facade: every documented public
//! entry point is referenced here through its facade path, and the ones whose
//! signatures matter are pinned by coercing them to `fn` pointers (exact
//! parameter and return types) or by constructing them. Renaming, removing,
//! or changing the signature of any pinned item breaks compilation of this
//! test — you cannot merge a breaking facade change without updating this
//! file and the list below.
//!
//! Beyond item names, the snapshot pins structure, so drift inside a type
//! cannot slip through either:
//!
//! - every public struct with public fields is constructed and **every field
//!   is read** (a rename/removal of any public field breaks compilation),
//! - every public enum is matched **exhaustively** with every variant and
//!   every named variant field bound (adding, removing, renaming a variant —
//!   or renaming a variant field — breaks compilation; the inline
//!   `NodeType::ServiceTask` fn-pointer variant is bound in its match arm,
//!   which cannot be constructed as a value here),
//! - the load-bearing free functions and methods (the full builder fluent
//!   API) are coerced to `fn` pointers.
//!
//! The workspace convention: **the facade's minor version is a compatibility
//! boundary**. Additive changes (new methods, new re-exported items) update
//! `API_SURFACE` and the release notes; removals or signature changes require
//! a major version bump and a migration note in the changelog.
//!
//! When you add, remove, rename or re-signature a public item:
//! 1. update the checks in `facade_surface_compiles_with_pinned_signatures`,
//! 2. update `API_SURFACE` (and `API_SURFACE_LEN`) to match,
//! 3. mention the change in `CHANGELOG.md`.
//!
//! Not pinned: feature-gated items (`observability` metrics, the `api` REST
//! server feature) — those are opt-in surfaces documented separately. The
//! worker-SDK compile checks are gated behind the default-on `worker-sdk`
//! feature (mirroring the facade re-export); the list below keeps documenting
//! those entries as part of the default surface.

use std::collections::HashMap;
use std::sync::Arc;

use bpm_engine::bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine::bpm_engine_bpmn::builder::{FormFieldSpec, ProcessBuilder};
use bpm_engine::bpm_engine_bpmn::{
    compile, parse, parse_and_compile, BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, CompileError,
    CompilerError, ErrorCode, ParseError,
};
use bpm_engine::bpm_engine_core::event::payloads;
use bpm_engine::bpm_engine_core::{
    is_valid_token_transition, CoreError, EdgeCondition, EngineEvent, ExternalTaskState, FormField,
    FormFieldType, InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition, ProcessInstance,
    Token, TokenMode, TokenStatus,
};
use bpm_engine::bpm_engine_dsl::{
    compile as dsl_compile, schema_json, DslCompileReport, DslCondition, DslDocument, DslEdge,
    DslError, DslErrorCode, DslNode, DslNodeType, DslProcess,
};
use bpm_engine::bpm_engine_runtime::{
    BpmEngine, EngineContext, EngineContextBuilder, EventHandler, ExternalTaskCompletedHandler,
    HistoryHandler, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{
    DefinitionStatus, ExternalTaskStore, HistoryRepo, ProcessDefinitionStore, ProcessInstanceStore,
    TimerStore, TokenStore,
};

/// The documented public surface of the `bpm-engine` facade, one entry per
/// line, in strict sorted order (asserted below so drift is easy to spot in
/// diffs). Keep in sync with the compile-time checks below.
const API_SURFACE: &[&str] = &[
    // In-memory adapter (default for development/tests).
    "bpm_engine::bpm_engine_adapter_memory::MemoryRepo",
    "bpm_engine::bpm_engine_adapter_memory::MemoryRepo::new",
    "bpm_engine::bpm_engine_adapter_memory::ProcessDefStore",
    "bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new",
    // BPMN 2.0 XML entry (parse + shared compiler).
    "bpm_engine::bpm_engine_bpmn::BpmnFlowNode",
    "bpm_engine::bpm_engine_bpmn::BpmnProcess",
    "bpm_engine::bpm_engine_bpmn::BpmnSequenceFlow",
    "bpm_engine::bpm_engine_bpmn::CompileError",
    "bpm_engine::bpm_engine_bpmn::CompilerError",
    "bpm_engine::bpm_engine_bpmn::ErrorCode",
    "bpm_engine::bpm_engine_bpmn::ParseError",
    // Typed builder (bpm-engine-bpmn::builder) — the Rust developer entry.
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::boolean",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::default_value",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::enum_field",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::label",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::number",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::required",
    "bpm_engine::bpm_engine_bpmn::builder::FormFieldSpec::string",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::build",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::edge",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::end_node",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::exclusive_gateway",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::external_task",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::form_field",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::form_key",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::is_default",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::name",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::new",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::parallel_gateway",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::process_name",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::retries",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::start_node",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::timeout_secs",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::timer_catch",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::user_task",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::when_expression",
    "bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder::when_variable_eq",
    "bpm_engine::bpm_engine_bpmn::compile",
    "bpm_engine::bpm_engine_bpmn::parse",
    "bpm_engine::bpm_engine_bpmn::parse_and_compile",
    // Core model and event types.
    "bpm_engine::bpm_engine_core::CoreError",
    "bpm_engine::bpm_engine_core::EdgeCondition",
    "bpm_engine::bpm_engine_core::EngineEvent",
    "bpm_engine::bpm_engine_core::ExternalTaskState",
    "bpm_engine::bpm_engine_core::FormField",
    "bpm_engine::bpm_engine_core::FormFieldType",
    "bpm_engine::bpm_engine_core::InstanceState",
    "bpm_engine::bpm_engine_core::Node",
    "bpm_engine::bpm_engine_core::NodeType",
    "bpm_engine::bpm_engine_core::OutgoingEdge",
    "bpm_engine::bpm_engine_core::ProcessDefinition",
    "bpm_engine::bpm_engine_core::ProcessInstance",
    "bpm_engine::bpm_engine_core::Token",
    "bpm_engine::bpm_engine_core::TokenMode",
    "bpm_engine::bpm_engine_core::TokenStatus",
    "bpm_engine::bpm_engine_core::is_valid_token_transition",
    "bpm_engine::bpm_engine_core::payloads",
    // JSON DSL entry (agents).
    "bpm_engine::bpm_engine_dsl::DslCompileReport",
    "bpm_engine::bpm_engine_dsl::DslCondition",
    "bpm_engine::bpm_engine_dsl::DslDocument",
    "bpm_engine::bpm_engine_dsl::DslEdge",
    "bpm_engine::bpm_engine_dsl::DslError",
    "bpm_engine::bpm_engine_dsl::DslErrorCode",
    "bpm_engine::bpm_engine_dsl::DslNode",
    "bpm_engine::bpm_engine_dsl::SUPPORTED_SCHEMA_VERSION",
    "bpm_engine::bpm_engine_dsl::compile",
    "bpm_engine::bpm_engine_dsl::schema_json",
    // Runtime engine and handlers.
    "bpm_engine::bpm_engine_runtime::BpmEngine",
    "bpm_engine::bpm_engine_runtime::BpmEngine::new",
    "bpm_engine::bpm_engine_runtime::BpmEngine::run_async",
    "bpm_engine::bpm_engine_runtime::EngineContext",
    "bpm_engine::bpm_engine_runtime::EngineContext::builder",
    "bpm_engine::bpm_engine_runtime::EngineContextBuilder",
    "bpm_engine::bpm_engine_runtime::EngineContextBuilder::build",
    "bpm_engine::bpm_engine_runtime::EventHandler",
    "bpm_engine::bpm_engine_runtime::ExternalTaskCompletedHandler",
    "bpm_engine::bpm_engine_runtime::HistoryHandler",
    "bpm_engine::bpm_engine_runtime::ProcessCompletedHandler",
    "bpm_engine::bpm_engine_runtime::ProcessStartHandler",
    "bpm_engine::bpm_engine_runtime::TokenArrivedHandler",
    "bpm_engine::bpm_engine_runtime::TokenArrivedHandler::new",
    "bpm_engine::bpm_engine_runtime::UserTaskCompletedHandler",
    // Storage traits (implement for custom backends).
    "bpm_engine::bpm_engine_storage::DefinitionStatus",
    "bpm_engine::bpm_engine_storage::ExternalTaskStore",
    "bpm_engine::bpm_engine_storage::HistoryRepo",
    "bpm_engine::bpm_engine_storage::ProcessDefinitionStore",
    "bpm_engine::bpm_engine_storage::ProcessInstanceStore",
    "bpm_engine::bpm_engine_storage::TimerStore",
    "bpm_engine::bpm_engine_storage::TokenStore",
    // Worker SDK (pull-based external task workers; default-on
    // `worker-sdk` facade feature).
    "bpm_engine::bpm_engine_worker_sdk::EngineClient",
    "bpm_engine::bpm_engine_worker_sdk::EngineClient::new",
    "bpm_engine::bpm_engine_worker_sdk::ExternalTask",
    "bpm_engine::bpm_engine_worker_sdk::TaskContext",
    "bpm_engine::bpm_engine_worker_sdk::TaskHandler",
    "bpm_engine::bpm_engine_worker_sdk::TaskResult",
    "bpm_engine::bpm_engine_worker_sdk::Worker",
    "bpm_engine::bpm_engine_worker_sdk::Worker::builder",
    "bpm_engine::bpm_engine_worker_sdk::WorkerConfig",
    "bpm_engine::bpm_engine_worker_sdk::WorkerConfig::new",
    "bpm_engine::bpm_engine_worker_sdk::worker::WorkerBuilder",
];

/// The documented entry count of [`API_SURFACE`]: **102 entries**. The test
/// below asserts the list length against this constant, so a count quoted in
/// a review or changelog cannot silently drift from the real list — when you
/// add or remove an entry, update the list and this number together.
const API_SURFACE_LEN: usize = 102;

#[test]
fn api_surface_list_is_sorted_unique_and_nonempty() {
    assert!(!API_SURFACE.is_empty());
    assert_eq!(
        API_SURFACE.len(),
        API_SURFACE_LEN,
        "API_SURFACE drifted from the documented count {API_SURFACE_LEN}: update API_SURFACE_LEN \
         together with the list"
    );
    let mut sorted = API_SURFACE.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        API_SURFACE.len(),
        "API_SURFACE must contain no duplicates"
    );
    let mut sorted = API_SURFACE.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        sorted.as_slice(),
        API_SURFACE,
        "API_SURFACE must be kept sorted so diffs are readable"
    );
}

/// Every documented facade entry compiles through its facade path, with the
/// load-bearing signatures pinned by `fn`-pointer coercion (arity, parameter
/// types and return type), and the structure of every public type pinned by
/// construction (all fields read) and exhaustive matches (all variants, all
/// variant fields bound). This is the compatibility snapshot: change the
/// facade, change this test.
#[test]
fn facade_surface_compiles_with_pinned_signatures() {
    // --- Typed builder (E4-3): entry, terminal, field spec helpers. ---
    let new_builder: fn(&str) -> ProcessBuilder = ProcessBuilder::new;
    let build: fn(&ProcessBuilder) -> Result<ProcessDefinition, CompileError> =
        ProcessBuilder::build;
    let spec_string: fn(&str) -> FormFieldSpec = FormFieldSpec::string;
    let spec_number: fn(&str) -> FormFieldSpec = FormFieldSpec::number;
    let spec_boolean: fn(&str) -> FormFieldSpec = FormFieldSpec::boolean;
    let spec_enum: fn(&str, &[&str]) -> FormFieldSpec = FormFieldSpec::enum_field;
    let spec_label: fn(FormFieldSpec, &str) -> FormFieldSpec = FormFieldSpec::label;
    let spec_required: fn(FormFieldSpec) -> FormFieldSpec = FormFieldSpec::required;
    let spec_default: fn(FormFieldSpec, &str) -> FormFieldSpec = FormFieldSpec::default_value;

    // The full fluent API (nodes, edges, node/edge option setters): each
    // method coerced to a `fn` pointer, pinning its exact signature. The
    // explicit `for<'a>` binder ties the returned `&mut ProcessBuilder` to
    // the builder argument, exactly like the methods' own elided lifetimes.
    let b_start_node: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::start_node;
    let b_end_node: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::end_node;
    let b_external_task: for<'a> fn(&'a mut ProcessBuilder, &str, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::external_task;
    let b_user_task: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::user_task;
    let b_timer_catch: for<'a> fn(&'a mut ProcessBuilder, &str, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::timer_catch;
    let b_exclusive_gateway: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::exclusive_gateway;
    let b_parallel_gateway: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::parallel_gateway;
    let b_edge: for<'a> fn(&'a mut ProcessBuilder, &str, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::edge;
    let b_when_variable_eq: for<'a> fn(
        &'a mut ProcessBuilder,
        &str,
        &str,
    ) -> &'a mut ProcessBuilder = ProcessBuilder::when_variable_eq;
    let b_when_expression: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::when_expression;
    let b_is_default: for<'a> fn(&'a mut ProcessBuilder) -> &'a mut ProcessBuilder =
        ProcessBuilder::is_default;
    let b_process_name: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::process_name;
    let b_name: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::name;
    let b_retries: for<'a> fn(&'a mut ProcessBuilder, i32) -> &'a mut ProcessBuilder =
        ProcessBuilder::retries;
    let b_timeout_secs: for<'a> fn(&'a mut ProcessBuilder, u64) -> &'a mut ProcessBuilder =
        ProcessBuilder::timeout_secs;
    let b_form_key: for<'a> fn(&'a mut ProcessBuilder, &str) -> &'a mut ProcessBuilder =
        ProcessBuilder::form_key;
    let b_form_field: for<'a> fn(&'a mut ProcessBuilder, FormFieldSpec) -> &'a mut ProcessBuilder =
        ProcessBuilder::form_field;
    let _ = (
        new_builder,
        build,
        spec_string,
        spec_number,
        spec_boolean,
        spec_enum,
        spec_label,
        spec_required,
        spec_default,
        b_start_node,
        b_end_node,
        b_external_task,
        b_user_task,
        b_timer_catch,
        b_exclusive_gateway,
        b_parallel_gateway,
        b_edge,
        b_when_variable_eq,
        b_when_expression,
        b_is_default,
        b_process_name,
        b_name,
        b_retries,
        b_timeout_secs,
        b_form_key,
        b_form_field,
    );

    // The builder entry is runnable end to end through the facade.
    let def = new_builder("surface-check")
        .start_node("start")
        .end_node("end")
        .edge("start", "end")
        .build()
        .expect("builder quickstart must compile");
    assert_eq!(def.id, "surface-check");

    // --- BPMN XML entry. ---
    let parse_xml: fn(&str) -> Result<BpmnProcess, ParseError> = parse;
    let parse_compile: fn(&str) -> Result<ProcessDefinition, CompileError> = parse_and_compile;
    let compile_model: fn(BpmnProcess) -> Result<ProcessDefinition, Vec<CompilerError>> = compile;
    let _ = (parse_xml, parse_compile, compile_model);

    // --- JSON DSL entry. ---
    let dsl: fn(&str) -> Result<ProcessDefinition, DslCompileReport> = dsl_compile;
    let schema: fn() -> String = schema_json;
    let _ = (dsl, schema);
    const _: u32 = bpm_engine::bpm_engine_dsl::SUPPORTED_SCHEMA_VERSION;

    // --- Core model free function. ---
    let valid_transition: fn(TokenStatus, TokenStatus) -> bool = is_valid_token_transition;
    let _ = valid_transition;

    // --- Runtime engine. ---
    let engine_new: fn(Vec<Box<dyn EventHandler>>) -> BpmEngine = BpmEngine::new;
    let _ = engine_new;
    // `run_async` is an async fn (no fn-pointer coercion); referencing the
    // item pins its name and receiver.
    let _ = BpmEngine::run_async;
    // The builder signature, factored through a type alias to keep clippy's
    // `type_complexity` happy — the alias is itself the pinned snapshot.
    type EngineContextBuilderFn = fn(
        std::sync::Arc<dyn ProcessInstanceStore>,
        std::sync::Arc<dyn TokenStore>,
        std::sync::Arc<dyn ProcessDefinitionStore>,
    ) -> EngineContextBuilder;
    let ctx_builder: EngineContextBuilderFn = EngineContext::builder;
    let _ = ctx_builder;
    // Handler type names (unit structs and the trait object stay pin-able).
    let handler_new: fn() -> TokenArrivedHandler = TokenArrivedHandler::new;
    let _ = handler_new;
    let _: Option<ProcessStartHandler> = None;
    let _: Option<UserTaskCompletedHandler> = None;
    let _: Option<ExternalTaskCompletedHandler> = None;
    let _: Option<ProcessCompletedHandler> = None;
    let _: Option<HistoryHandler> = None;
    let _: Option<Box<dyn EventHandler>> = None;

    // --- Storage traits stay object-safe (custom backends implement them). ---
    let _: Option<Box<dyn ProcessInstanceStore>> = None;
    let _: Option<Box<dyn TokenStore>> = None;
    let _: Option<Box<dyn ExternalTaskStore>> = None;
    let _: Option<Box<dyn TimerStore>> = None;
    let _: Option<Box<dyn HistoryRepo>> = None;
    let _: Option<Box<dyn ProcessDefinitionStore>> = None;

    // --- Memory adapter: entry points + the repo implements the store traits. ---
    let repo_new: fn() -> MemoryRepo = MemoryRepo::new;
    let defs_new: fn() -> ProcessDefStore = ProcessDefStore::new;
    let _ = (repo_new, defs_new);
    fn memory_repo_is_a_full_store<R>(_: &R)
    where
        R: ProcessInstanceStore + TokenStore + ExternalTaskStore + TimerStore + HistoryRepo,
    {
    }
    memory_repo_is_a_full_store(&MemoryRepo::new());

    // --- Field- and variant-level pins ----------------------------------
    // Every public struct with public fields is constructed and every field
    // read; every public enum is matched exhaustively with every variant
    // (and every named variant field) bound. A rename or removal of any of
    // those names breaks the corresponding helper — and this test.
    pin_token_fields();
    pin_process_instance_fields();
    pin_node_fields();
    pin_form_field_fields();
    pin_process_definition_fields();
    pin_token_lifecycle_enums();
    pin_core_error_variants();
    pin_edge_condition_variants();
    pin_node_type_variants();
    pin_engine_event_variants();
    pin_bpmn_sequence_flow_fields();
    pin_bpmn_process_fields();
    pin_compiler_error_fields();
    pin_bpmn_flow_node_variants();
    pin_bpmn_error_variants();
    pin_form_field_spec_fields();
    pin_dsl_document_fields();
    pin_dsl_node_fields();
    pin_dsl_edge_fields();
    pin_dsl_error_fields();
    pin_dsl_condition_variants();
    pin_dsl_error_code_variants();
    pin_definition_status_variants();
    pin_engine_context_fields();

    // --- Worker SDK: client + worker plumbing (feature `worker-sdk`). ---
    #[cfg(feature = "worker-sdk")]
    pin_worker_sdk_surface();
}

// ---------------------------------------------------------------------------
// Core model: field-level pins
// ---------------------------------------------------------------------------

/// Representative `Token` (pure data; the literal itself pins the field set).
fn sample_token() -> Token {
    Token {
        id: "token-1".to_string(),
        node_id: "node-1".to_string(),
        status: TokenStatus::Ready,
        mode: TokenMode::Compensation,
        version: 7,
        attempt: 2,
        parallel_group_id: Some("group-1".to_string()),
        updated_at: Some("2026-01-01T00:00:00Z".to_string()),
    }
}

/// `Token`: all 8 public fields read (id, node_id, status, mode, version,
/// attempt, parallel_group_id, updated_at).
fn pin_token_fields() -> String {
    let token = sample_token();
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        token.id,
        token.node_id,
        token.status,
        token.mode,
        token.version,
        token.attempt,
        token.parallel_group_id,
        token.updated_at
    )
}

/// `ProcessInstance`: all 9 public fields read (id, process_def_id,
/// tenant_id, tokens, variables, state, version, parent_instance_id,
/// parent_token_id).
fn pin_process_instance_fields() -> String {
    let instance = ProcessInstance {
        id: "inst-1".to_string(),
        process_def_id: "def-1".to_string(),
        tenant_id: Some("tenant-1".to_string()),
        tokens: vec![sample_token()],
        variables: HashMap::from([("amount".to_string(), "100".to_string())]),
        state: InstanceState::Running,
        version: 3,
        parent_instance_id: Some("parent-inst-1".to_string()),
        parent_token_id: Some("parent-token-1".to_string()),
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        instance.id,
        instance.process_def_id,
        instance.tenant_id,
        instance.tokens,
        instance.variables,
        instance.state,
        instance.version,
        instance.parent_instance_id,
        instance.parent_token_id
    )
}

/// `OutgoingEdge` (target, condition) and `Node` (id, node_type,
/// outgoing_edges): every public field read.
fn pin_node_fields() -> String {
    let edge = OutgoingEdge {
        target: "end",
        condition: Some(EdgeCondition::VariableEq {
            key: "amount".to_string(),
            value: "100".to_string(),
        }),
    };
    let node = Node {
        id: "start",
        node_type: NodeType::Start,
        outgoing_edges: vec![edge.clone()],
    };
    format!(
        "{:?}{:?} {:?}{:?}{:?}",
        edge.target, edge.condition, node.id, node.node_type, node.outgoing_edges
    )
}

/// `FormField`: all 6 public fields read (id, label, field_type, required,
/// default_value, options).
fn pin_form_field_fields() -> String {
    let field = FormField {
        id: "amount".to_string(),
        label: "Amount".to_string(),
        field_type: FormFieldType::Number,
        required: true,
        default_value: Some("0".to_string()),
        options: Some(vec!["high".to_string(), "low".to_string()]),
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}",
        field.id, field.label, field.field_type, field.required, field.default_value, field.options
    )
}

/// Representative `Node` for embedding in a `ProcessDefinition`.
fn sample_node() -> Node {
    Node {
        id: "start",
        node_type: NodeType::Start,
        outgoing_edges: vec![OutgoingEdge {
            target: "end",
            condition: Some(EdgeCondition::Default),
        }],
    }
}

/// `ProcessDefinition`: all 4 public fields read (id, nodes, start,
/// boundary_events).
fn pin_process_definition_fields() -> String {
    let definition = ProcessDefinition {
        id: "surface",
        nodes: HashMap::from([("start", sample_node())]),
        start: "start",
        boundary_events: HashMap::new(),
    };
    format!(
        "{:?}{:?}{:?}{:?}",
        definition.id, definition.nodes, definition.start, definition.boundary_events
    )
}

// ---------------------------------------------------------------------------
// Core model: variant-level pins
// ---------------------------------------------------------------------------

/// The fieldless core enums, exhaustively matched: `TokenStatus` (7),
/// `TokenMode` (2), `InstanceState` (3), `ExternalTaskState` (4),
/// `FormFieldType` (4).
fn pin_token_lifecycle_enums() -> String {
    let token_status = match TokenStatus::Created {
        TokenStatus::Created => "Created",
        TokenStatus::Ready => "Ready",
        TokenStatus::Executing => "Executing",
        TokenStatus::Waiting => "Waiting",
        TokenStatus::Suspended => "Suspended",
        TokenStatus::Completed => "Completed",
        TokenStatus::Terminated => "Terminated",
    };
    let token_mode = match TokenMode::Forward {
        TokenMode::Forward => "Forward",
        TokenMode::Compensation => "Compensation",
    };
    let instance_state = match InstanceState::Running {
        InstanceState::Running => "Running",
        InstanceState::Completed => "Completed",
        InstanceState::Terminated => "Terminated",
    };
    let external_task_state = match ExternalTaskState::Ready {
        ExternalTaskState::Ready => "Ready",
        ExternalTaskState::Locked => "Locked",
        ExternalTaskState::Completed => "Completed",
        ExternalTaskState::Failed => "Failed",
    };
    let form_field_type = match FormFieldType::String {
        FormFieldType::String => "String",
        FormFieldType::Number => "Number",
        FormFieldType::Boolean => "Boolean",
        FormFieldType::Enum => "Enum",
    };
    format!("{token_status}{token_mode}{instance_state}{external_task_state}{form_field_type}")
}

/// `CoreError`: both variants with their tuple fields bound.
fn pin_core_error_variants() -> String {
    match CoreError::NotFound("missing".to_string()) {
        CoreError::NotFound(what) => format!("NotFound({what})"),
        CoreError::InvalidState(why) => format!("InvalidState({why})"),
    }
}

/// `EdgeCondition`: all 3 variants with their fields bound.
fn pin_edge_condition_variants() -> String {
    match EdgeCondition::Default {
        EdgeCondition::VariableEq { key, value } => format!("VariableEq({key}{value})"),
        EdgeCondition::Expression(expression) => format!("Expression({expression})"),
        EdgeCondition::Default => "Default".to_string(),
    }
}

/// `NodeType`: all 18 variants exhaustively matched, every named field bound.
/// The `ServiceTask(fn(&mut ProcessInstance))` variant cannot be constructed
/// here as a value, so its fn-pointer field is bound in the match arm.
fn pin_node_type_variants() -> String {
    match NodeType::Start {
        NodeType::Start => "Start".to_string(),
        NodeType::End => "End".to_string(),
        NodeType::ServiceTask(run) => {
            let _ = run;
            "ServiceTask".to_string()
        }
        NodeType::UserTask {
            form_key,
            form_fields,
        } => format!("UserTask({form_key:?}{form_fields:?})"),
        NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } => format!("ExternalTask({task_type}{retries}{timeout_secs})"),
        NodeType::ExclusiveGateway => "ExclusiveGateway".to_string(),
        NodeType::ParallelFork => "ParallelFork".to_string(),
        NodeType::ParallelJoin { expected } => format!("ParallelJoin({expected})"),
        NodeType::TimerIntermediateCatch { timer_definition } => {
            format!("TimerIntermediateCatch({timer_definition})")
        }
        NodeType::BoundaryTimer {
            duration,
            is_interrupting,
        } => format!("BoundaryTimer({duration}{is_interrupting})"),
        NodeType::BoundaryError {
            error_code,
            is_interrupting,
        } => format!("BoundaryError({error_code:?}{is_interrupting})"),
        NodeType::CallActivity { called_process_key } => {
            format!("CallActivity({called_process_key})")
        }
        NodeType::MessageIntermediateCatch { message_name } => {
            format!("MessageIntermediateCatch({message_name})")
        }
        NodeType::MessageIntermediateThrow { message_name } => {
            format!("MessageIntermediateThrow({message_name})")
        }
        NodeType::SignalIntermediateThrow { signal_name } => {
            format!("SignalIntermediateThrow({signal_name})")
        }
        NodeType::SignalIntermediateCatch { signal_name } => {
            format!("SignalIntermediateCatch({signal_name})")
        }
        NodeType::TerminateEnd => "TerminateEnd".to_string(),
    }
}

/// `EngineEvent`: all 17 variants exhaustively matched, destructuring each
/// payload struct so every payload field name is pinned too.
fn pin_engine_event_variants() -> String {
    let event = EngineEvent::ProcessTerminated(payloads::ProcessTerminated {
        instance_id: "inst-1".to_string(),
    });
    match &event {
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id,
            instance_id,
            initial_variables,
        }) => format!("ProcessStarted({process_id}{instance_id}{initial_variables:?})"),
        EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id,
            token_id,
            node_id,
        }) => format!("TokenArrived({instance_id}{token_id}{node_id})"),
        EngineEvent::TokenCompleted(payloads::TokenCompleted {
            instance_id,
            token_id,
        }) => format!("TokenCompleted({instance_id}{token_id})"),
        EngineEvent::UserTaskCreated(payloads::UserTaskCreated {
            instance_id,
            node_id,
        }) => format!("UserTaskCreated({instance_id}{node_id})"),
        EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
            task_id,
            instance_id,
            node_id,
            variables,
        }) => format!("UserTaskCompleted({task_id}{instance_id}{node_id}{variables:?})"),
        EngineEvent::TimerFired(payloads::TimerFired {
            timer_id,
            token_id,
            node_id,
        }) => format!("TimerFired({timer_id}{token_id}{node_id})"),
        EngineEvent::TimerScheduled(payloads::TimerScheduled {
            timer_id,
            instance_id,
            token_id,
            node_id,
            fire_at,
        }) => format!("TimerScheduled({timer_id}{instance_id}{token_id}{node_id}{fire_at})"),
        EngineEvent::TokenFailed(payloads::TokenFailed {
            instance_id,
            token_id,
            node_id,
            reason,
        }) => format!("TokenFailed({instance_id}{token_id}{node_id}{reason})"),
        EngineEvent::SagaStarted(payloads::SagaStarted {
            instance_id,
            token_id,
            node_id,
        }) => format!("SagaStarted({instance_id}{token_id}{node_id})"),
        EngineEvent::SagaCompleted(payloads::SagaCompleted {
            instance_id,
            token_id,
            node_id,
        }) => format!("SagaCompleted({instance_id}{token_id}{node_id})"),
        EngineEvent::ProcessCompleted(payloads::ProcessCompleted { instance_id }) => {
            format!("ProcessCompleted({instance_id})")
        }
        EngineEvent::CallActivityStarted(payloads::CallActivityStarted {
            parent_instance_id,
            parent_token_id,
            child_instance_id,
            child_process_key,
        }) => format!(
            "CallActivityStarted({parent_instance_id}{parent_token_id}{child_instance_id}\
             {child_process_key})"
        ),
        EngineEvent::CallActivityCompleted(payloads::CallActivityCompleted {
            parent_instance_id,
            parent_token_id,
            child_instance_id,
        }) => format!(
            "CallActivityCompleted({parent_instance_id}{parent_token_id}{child_instance_id})"
        ),
        EngineEvent::MessageSent(payloads::MessageSent {
            instance_id,
            message_name,
        }) => format!("MessageSent({instance_id}{message_name})"),
        EngineEvent::SignalSent(payloads::SignalSent {
            instance_id,
            signal_name,
        }) => format!("SignalSent({instance_id}{signal_name})"),
        EngineEvent::ProcessTerminated(payloads::ProcessTerminated { instance_id }) => {
            format!("ProcessTerminated({instance_id})")
        }
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id,
            token_id,
            node_id,
            variables,
        }) => format!("ExternalTaskCompleted({instance_id}{token_id}{node_id}{variables:?})"),
    }
}

// ---------------------------------------------------------------------------
// BPMN: field- and variant-level pins
// ---------------------------------------------------------------------------

/// `BpmnSequenceFlow`: all 5 public fields read (id, source_ref, target_ref,
/// condition_expression, is_default).
fn pin_bpmn_sequence_flow_fields() -> String {
    let flow = BpmnSequenceFlow {
        id: "flow_0".to_string(),
        source_ref: "start".to_string(),
        target_ref: "end".to_string(),
        condition_expression: Some("${amount > 100}".to_string()),
        is_default: false,
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}",
        flow.id, flow.source_ref, flow.target_ref, flow.condition_expression, flow.is_default
    )
}

/// `BpmnProcess`: all 4 public fields read (id, name, flow_nodes,
/// sequence_flows).
fn pin_bpmn_process_fields() -> String {
    let process = BpmnProcess {
        id: "surface".to_string(),
        name: Some("Surface".to_string()),
        flow_nodes: HashMap::new(),
        sequence_flows: vec![BpmnSequenceFlow {
            id: "flow_0".to_string(),
            source_ref: "start".to_string(),
            target_ref: "end".to_string(),
            condition_expression: None,
            is_default: false,
        }],
    };
    format!(
        "{:?}{:?}{:?}{:?}",
        process.id, process.name, process.flow_nodes, process.sequence_flows
    )
}

/// `CompilerError`: all 5 public fields read (code, message, node_id,
/// flow_id, hint).
fn pin_compiler_error_fields() -> String {
    let error = CompilerError {
        code: ErrorCode::NoStartEvent,
        message: "no start event".to_string(),
        node_id: Some("start".to_string()),
        flow_id: Some("flow_0".to_string()),
        hint: Some("add a start event".to_string()),
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}",
        error.code, error.message, error.node_id, error.flow_id, error.hint
    )
}

/// `BpmnFlowNode`: all 15 variants exhaustively matched, every named field
/// bound (id, name, incoming, outgoing plus the variant-specific fields).
fn pin_bpmn_flow_node_variants() -> String {
    let node = BpmnFlowNode::StartEvent {
        id: "start".to_string(),
        name: None,
        incoming: vec![],
        outgoing: vec![],
    };
    match &node {
        BpmnFlowNode::StartEvent {
            id,
            name,
            incoming,
            outgoing,
        } => format!("StartEvent({id}{name:?}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::EndEvent {
            id,
            name,
            incoming,
            outgoing,
        } => format!("EndEvent({id}{name:?}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::ServiceTask {
            id,
            name,
            task_type,
            retries,
            timeout_secs,
            incoming,
            outgoing,
        } => format!(
            "ServiceTask({id}{name:?}{task_type}{retries}{timeout_secs}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::UserTask {
            id,
            name,
            form_key,
            form_fields,
            incoming,
            outgoing,
        } => format!("UserTask({id}{name:?}{form_key:?}{form_fields:?}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::ExclusiveGateway {
            id,
            name,
            incoming,
            outgoing,
        } => format!("ExclusiveGateway({id}{name:?}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::ParallelGateway {
            id,
            name,
            incoming,
            outgoing,
        } => format!("ParallelGateway({id}{name:?}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::SubProcess {
            id,
            name,
            flow_nodes,
            sequence_flows,
            incoming,
            outgoing,
        } => format!(
            "SubProcess({id}{name:?}{flow_nodes:?}{sequence_flows:?}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::TimerIntermediateCatchEvent {
            id,
            name,
            timer_type,
            incoming,
            outgoing,
        } => format!(
            "TimerIntermediateCatchEvent({id}{name:?}{timer_type:?}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::BoundaryEvent {
            id,
            name,
            attached_to_ref,
            event_type,
            is_interrupting,
            incoming,
            outgoing,
        } => format!(
            "BoundaryEvent({id}{name:?}{attached_to_ref}{event_type:?}{is_interrupting}\
             {incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::CallActivity {
            id,
            name,
            called_element,
            incoming,
            outgoing,
        } => format!("CallActivity({id}{name:?}{called_element}{incoming:?}{outgoing:?})"),
        BpmnFlowNode::MessageIntermediateCatchEvent {
            id,
            name,
            message_name,
            incoming,
            outgoing,
        } => format!(
            "MessageIntermediateCatchEvent({id}{name:?}{message_name}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::MessageIntermediateThrowEvent {
            id,
            name,
            message_name,
            incoming,
            outgoing,
        } => format!(
            "MessageIntermediateThrowEvent({id}{name:?}{message_name}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::SignalIntermediateThrowEvent {
            id,
            name,
            signal_name,
            incoming,
            outgoing,
        } => format!(
            "SignalIntermediateThrowEvent({id}{name:?}{signal_name}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::SignalIntermediateCatchEvent {
            id,
            name,
            signal_name,
            incoming,
            outgoing,
        } => format!(
            "SignalIntermediateCatchEvent({id}{name:?}{signal_name}{incoming:?}{outgoing:?})"
        ),
        BpmnFlowNode::TerminateEndEvent {
            id,
            name,
            incoming,
            outgoing,
        } => format!("TerminateEndEvent({id}{name:?}{incoming:?}{outgoing:?})"),
    }
}

/// The BPMN error enums, exhaustively matched: `ErrorCode` (10 variants),
/// `ParseError` (5 variants, tuple fields bound), `CompileError` (2
/// variants, payloads bound).
fn pin_bpmn_error_variants() -> String {
    let error_code = match ErrorCode::NoStartEvent {
        ErrorCode::NoStartEvent => "NoStartEvent",
        ErrorCode::MultipleStartEvents => "MultipleStartEvents",
        ErrorCode::NoEndEvent => "NoEndEvent",
        ErrorCode::OrphanNode => "OrphanNode",
        ErrorCode::DeadEnd => "DeadEnd",
        ErrorCode::ExclusiveGatewayNoDefault => "ExclusiveGatewayNoDefault",
        ErrorCode::ParallelGatewayInvalidShape => "ParallelGatewayInvalidShape",
        ErrorCode::SequenceFlowSourceNotFound => "SequenceFlowSourceNotFound",
        ErrorCode::SequenceFlowTargetNotFound => "SequenceFlowTargetNotFound",
        ErrorCode::UnsupportedElement => "UnsupportedElement",
    };
    let parse_error = match ParseError::NoProcess {
        ParseError::InvalidXml(xml) => format!("InvalidXml({xml:?})"),
        ParseError::UnknownElement(element) => format!("UnknownElement({element})"),
        ParseError::MissingAttribute(attribute) => format!("MissingAttribute({attribute})"),
        ParseError::NoProcess => "NoProcess".to_string(),
        ParseError::MultipleProcesses => "MultipleProcesses".to_string(),
    };
    let compile_error = match CompileError::Parse(ParseError::NoProcess) {
        CompileError::Parse(error) => format!("Parse({error:?})"),
        CompileError::Compile(errors) => format!("Compile({errors:?})"),
    };
    format!("{error_code}{parse_error}{compile_error}")
}

/// `FormFieldSpec` (builder): all 6 public fields read (id, label,
/// field_type, required, default_value, options).
fn pin_form_field_spec_fields() -> String {
    let spec = FormFieldSpec {
        id: "amount".to_string(),
        label: Some("Amount".to_string()),
        field_type: FormFieldType::Number,
        required: true,
        default_value: Some("0".to_string()),
        options: Some(vec!["high".to_string()]),
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}",
        spec.id, spec.label, spec.field_type, spec.required, spec.default_value, spec.options
    )
}

// ---------------------------------------------------------------------------
// DSL: field- and variant-level pins
// ---------------------------------------------------------------------------

/// Representative `DslNode` (pure data; the literal pins the field set).
fn sample_dsl_node() -> DslNode {
    DslNode {
        id: "start".to_string(),
        name: None,
        node_type: DslNodeType::Start,
        task_type: None,
        retries: None,
        timeout_secs: None,
        form_key: None,
        form_fields: None,
        timer: None,
    }
}

/// `DslDocument`: all 4 public fields read (schema_version, process, nodes,
/// edges). The inline `DslProcess`/`DslNode`/`DslEdge` literals pin those
/// helpers' field sets on the way.
fn pin_dsl_document_fields() -> String {
    let document = DslDocument {
        schema_version: 1,
        process: DslProcess {
            id: "surface".to_string(),
            name: Some("Surface".to_string()),
        },
        nodes: vec![sample_dsl_node()],
        edges: vec![DslEdge {
            id: Some("edge_0".to_string()),
            source: "start".to_string(),
            target: "end".to_string(),
            condition: Some(DslCondition::Expression {
                expression: "amount > 100".to_string(),
            }),
            default: None,
        }],
    };
    format!(
        "{:?}{:?}{:?}{:?}",
        document.schema_version, document.process, document.nodes, document.edges
    )
}

/// `DslNode`: all 9 public fields read (id, name, node_type, task_type,
/// retries, timeout_secs, form_key, form_fields, timer).
fn pin_dsl_node_fields() -> String {
    let node = DslNode {
        id: "approval".to_string(),
        name: Some("Approval".to_string()),
        node_type: DslNodeType::UserTask,
        task_type: None,
        retries: None,
        timeout_secs: None,
        form_key: Some("approval-form".to_string()),
        form_fields: None,
        timer: None,
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        node.id,
        node.name,
        node.node_type,
        node.task_type,
        node.retries,
        node.timeout_secs,
        node.form_key,
        node.form_fields,
        node.timer
    )
}

/// `DslEdge`: all 5 public fields read (id, source, target, condition,
/// default).
fn pin_dsl_edge_fields() -> String {
    let edge = DslEdge {
        id: Some("edge_0".to_string()),
        source: "xor".to_string(),
        target: "end".to_string(),
        condition: Some(DslCondition::VariableEq {
            variable: "amount".to_string(),
            value: "100".to_string(),
        }),
        default: None,
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?}",
        edge.id, edge.source, edge.target, edge.condition, edge.default
    )
}

/// `DslError` (code, path, message, hint, source_code) and
/// `DslCompileReport` (errors): every public field read.
fn pin_dsl_error_fields() -> String {
    let error = DslError {
        code: DslErrorCode::UnknownField,
        path: "/nodes/0".to_string(),
        message: "unknown field".to_string(),
        hint: Some("remove the field".to_string()),
        source_code: Some("NoEndEvent".to_string()),
    };
    let report = DslCompileReport {
        errors: vec![error.clone()],
    };
    format!(
        "{:?}{:?}{:?}{:?}{:?} {:?}",
        error.code, error.path, error.message, error.hint, error.source_code, report.errors
    )
}

/// `DslCondition`: both variants with their fields bound. (The DSL has no
/// `Default` condition variant — the default flow is `DslEdge.default`.)
fn pin_dsl_condition_variants() -> String {
    let condition = DslCondition::VariableEq {
        variable: "amount".to_string(),
        value: "100".to_string(),
    };
    match condition {
        DslCondition::VariableEq { variable, value } => {
            format!("VariableEq({variable}{value})")
        }
        DslCondition::Expression { expression } => format!("Expression({expression})"),
    }
}

/// `DslErrorCode`: all 10 variants exhaustively matched.
fn pin_dsl_error_code_variants() -> String {
    match DslErrorCode::JsonParse {
        DslErrorCode::JsonParse => "json_parse".to_string(),
        DslErrorCode::SchemaVersionUnsupported => "schema_version_unsupported".to_string(),
        DslErrorCode::UnknownField => "unknown_field".to_string(),
        DslErrorCode::MissingField => "missing_field".to_string(),
        DslErrorCode::InvalidType => "invalid_type".to_string(),
        DslErrorCode::DuplicateNodeId => "duplicate_node_id".to_string(),
        DslErrorCode::DuplicateEdgeId => "duplicate_edge_id".to_string(),
        DslErrorCode::UnknownNodeRef => "unknown_node_ref".to_string(),
        DslErrorCode::EmptyNodeList => "empty_node_list".to_string(),
        DslErrorCode::Compile => "compile".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Storage / runtime: pins
// ---------------------------------------------------------------------------

/// `DefinitionStatus`: both variants exhaustively matched.
fn pin_definition_status_variants() -> String {
    match DefinitionStatus::Active {
        DefinitionStatus::Active => "active".to_string(),
        DefinitionStatus::Deprecated => "deprecated".to_string(),
    }
}

/// `EngineContext`: constructed through `EngineContext::builder(...).build()`
/// (pure assembly over the memory stores — no I/O), then all 11 public
/// fields read (process_store, token_store, process_def_store,
/// parallel_join_repo, timer_store, compensation_repo, outbox_repo,
/// external_task_store, history_repo, history_redaction_policy, tenant_id).
fn pin_engine_context_fields() -> String {
    let context: EngineContext = EngineContext::builder(
        Arc::new(MemoryRepo::new()),
        Arc::new(MemoryRepo::new()),
        Arc::new(ProcessDefStore::new()),
    )
    .build();
    format!(
        "{:p}{:p}{:p}{}{}{}{}{}{}{:?}{:?}",
        context.process_store,
        context.token_store,
        context.process_def_store,
        context.parallel_join_repo.is_some(),
        context.timer_store.is_some(),
        context.compensation_repo.is_some(),
        context.outbox_repo.is_some(),
        context.external_task_store.is_some(),
        context.history_repo.is_some(),
        context.history_redaction_policy,
        context.tenant_id
    )
}

// ---------------------------------------------------------------------------
// Worker SDK (feature `worker-sdk`, default on)
// ---------------------------------------------------------------------------

/// Worker SDK surface: client + worker plumbing, gated behind the
/// `worker-sdk` facade feature exactly like the re-export itself.
#[cfg(feature = "worker-sdk")]
fn pin_worker_sdk_surface() {
    use bpm_engine::bpm_engine_worker_sdk::worker::WorkerBuilder;
    use bpm_engine::bpm_engine_worker_sdk::{
        EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
    };

    // Constructors (generic over `impl Into<String>` — pinned by calling).
    let _: EngineClient = EngineClient::new("http://127.0.0.1:3000");
    let worker_builder: fn() -> WorkerBuilder = Worker::builder;
    let _ = worker_builder;

    // `WorkerConfig`: constructed, all 8 public fields read (worker_id,
    // max_tasks, lock_duration, poll_interval, concurrency_limit,
    // fetch_retry_max, fetch_retry_backoff, shutdown_grace).
    let config = WorkerConfig::new("surface-worker");
    let _ = (
        config.worker_id.clone(),
        config.max_tasks,
        config.lock_duration,
        config.poll_interval,
        config.concurrency_limit,
        config.fetch_retry_max,
        config.fetch_retry_backoff,
        config.shutdown_grace,
    );

    // `ExternalTask` (the worker view): struct literal, every public field
    // read (token_id / process_instance_id are debug-only fields).
    let task = ExternalTask {
        task_id: "task-1".to_string(),
        task_type: "payment".to_string(),
        variables: HashMap::from([("amount".to_string(), "100".to_string())]),
        lock_expire_at: Some("2026-01-01T00:00:00Z".to_string()),
        retries: 3,
        #[cfg(debug_assertions)]
        token_id: Some("token-1".to_string()),
        #[cfg(debug_assertions)]
        process_instance_id: Some("inst-1".to_string()),
    };
    let _ = format!(
        "{:?}{:?}{:?}{:?}{:?}",
        task.task_id, task.task_type, task.variables, task.lock_expire_at, task.retries
    );
    #[cfg(debug_assertions)]
    {
        let _ = format!("{:?}{:?}", task.token_id, task.process_instance_id);
    }

    // `TaskResult`: both variants with their fields bound (E3-2 adds the
    // `category` field to `Fail`).
    let task_result = TaskResult::Complete {
        variables: HashMap::new(),
    };
    let _ = match &task_result {
        TaskResult::Complete { variables } => format!("Complete({variables:?})"),
        TaskResult::Fail {
            error,
            retry_after,
            category,
        } => format!("Fail({error}{retry_after:?}{category:?})"),
    };

    // E3-2 worker-context surface: spawn handle, cancellation, renewal,
    // timeout, idempotency guard (pinned by name / by construction).
    use bpm_engine::bpm_engine_worker_sdk::worker::WorkerHandle;
    use bpm_engine::bpm_engine_worker_sdk::{
        derive_key, with_timeout, IdempotencyGuard, LeaseRenewalGuard,
    };
    let _ = WorkerConfig::new("surface-worker").shutdown_grace(std::time::Duration::from_secs(5));
    let _: Option<WorkerHandle> = None;
    let _: Option<LeaseRenewalGuard> = None;
    let _ = derive_key("task-1", "input");
    let _guard = IdempotencyGuard::with_capacity_bound(16);
    let _fut = with_timeout(std::time::Duration::from_secs(1), async {});
    let mut guard = IdempotencyGuard::new();
    let _ = guard.check_and_mark("k");
    let _ = guard.unmark("k");
    let _ = guard.is_marked("k");

    // Type names for items whose fields are not public (pinned by name).
    let _: Option<Worker> = None;
    let _: Option<Box<dyn TaskHandler>> = None;
    let _: Option<TaskContext> = None;
}
