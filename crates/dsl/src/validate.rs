//! Structural validation.
//!
//! Two layers (ADR-006):
//!
//! 1. JSON shape: deserialization via serde with `deny_unknown_fields`, wrapped
//!    in `serde_path_to_error` so every serde error carries a JSON-pointer
//!    path. Streaming deserialization stops at the first error, so this layer
//!    reports at most one [`DslError`].
//! 2. DSL structure: duplicate ids, dangling references, per-node-type
//!    required/forbidden fields, empty node list, condition value safety.
//!    All errors are collected.

use std::collections::HashMap;

use serde_path_to_error::{Path as ErrorPath, Segment};

use crate::error::{DslError, DslErrorCode};
use crate::lower::resolved_edge_id;
use crate::types::{DslCondition, DslDocument, DslFormField, DslNode, DslNodeType};
use crate::SUPPORTED_SCHEMA_VERSION;

/// Deserialize the JSON document, mapping any serde failure to a
/// path-carrying [`DslError`].
pub(crate) fn parse_document(dsl_json: &str) -> Result<DslDocument, DslError> {
    let mut de = serde_json::Deserializer::from_str(dsl_json);
    match serde_path_to_error::deserialize(&mut de) {
        Ok(doc) => Ok(doc),
        Err(err) => Err(classify_deserialize_error(err)),
    }
}

/// Convert a serde error into a coded, path-carrying diagnostic.
fn classify_deserialize_error(err: serde_path_to_error::Error<serde_json::Error>) -> DslError {
    use serde_json::error::Category;

    let inner = err.inner();
    if !matches!(inner.classify(), Category::Data) {
        // Syntax / EOF / IO problems: the document is not parseable JSON.
        return DslError::new(DslErrorCode::JsonParse, "/", inner.to_string());
    }

    let message = strip_position(&inner.to_string(), inner.line(), inner.column());
    let base = json_pointer(err.path());

    // `unknown field \`x\``, `missing field \`x\`` and `duplicate field \`x\``
    // name a field. serde_path_to_error already includes the map key for
    // unknown fields, but stops at the containing object for missing and
    // duplicate fields, so append the field only when it is not there yet.
    if let Some(field) = extract_field_name(&message, "unknown field `") {
        let path = if pointer_ends_with(&base, &field) {
            base
        } else {
            append_pointer(&base, &field)
        };
        return DslError::new(DslErrorCode::UnknownField, path, message);
    }
    if let Some(field) = extract_field_name(&message, "missing field `") {
        return DslError::new(
            DslErrorCode::MissingField,
            append_pointer(&base, &field),
            message,
        );
    }
    if let Some(field) = extract_field_name(&message, "duplicate field `") {
        return DslError::new(
            DslErrorCode::InvalidType,
            append_pointer(&base, &field),
            format!(
                "field `{}` appears more than once in the same object",
                field
            ),
        );
    }
    // Everything else (invalid type, unknown variant, invalid value, ...) is a
    // value-shape problem at the path serde already tracked.
    DslError::new(DslErrorCode::InvalidType, base, message)
}

/// Remove serde_json's trailing ` at line L column C` from a data error.
fn strip_position(message: &str, line: usize, column: usize) -> String {
    let suffix = format!(" at line {} column {}", line, column);
    message.strip_suffix(&suffix).unwrap_or(message).to_string()
}

/// Extract a field name from messages like ``missing field `id```.
fn extract_field_name(message: &str, prefix: &str) -> Option<String> {
    let rest = message.strip_prefix(prefix)?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Render a serde_path_to_error path as a JSON pointer (RFC 6901).
fn json_pointer(path: &ErrorPath) -> String {
    let mut out = String::new();
    for segment in path.iter() {
        match segment {
            Segment::Seq { index } => {
                out.push('/');
                out.push_str(&index.to_string());
            }
            Segment::Map { key } => {
                out.push('/');
                out.push_str(&escape_pointer_token(key));
            }
            Segment::Enum { variant } => {
                out.push('/');
                out.push_str(&escape_pointer_token(variant));
            }
            Segment::Unknown => out.push_str("/?"),
        }
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        out
    }
}

/// Whether a JSON pointer's last segment equals the (escaped) field token.
fn pointer_ends_with(base: &str, field: &str) -> bool {
    let token = escape_pointer_token(field);
    base.rsplit('/').next() == Some(token.as_str())
}

/// Append a field token to an existing JSON pointer.
fn append_pointer(base: &str, field: &str) -> String {
    if field.is_empty() {
        return base.to_string();
    }
    if base == "/" || base.is_empty() {
        format!("/{}", escape_pointer_token(field))
    } else {
        format!("{}/{}", base, escape_pointer_token(field))
    }
}

/// Escape a JSON pointer token (`~` -> `~0`, `/` -> `~1`).
fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Run all structural checks and collect every error.
pub(crate) fn validate_structure(doc: &DslDocument) -> Vec<DslError> {
    let mut errors = Vec::new();

    if doc.schema_version != SUPPORTED_SCHEMA_VERSION {
        errors.push(
            DslError::new(
                DslErrorCode::SchemaVersionUnsupported,
                "/schema_version",
                format!(
                    "unsupported schema_version {}; this compiler supports version {}",
                    doc.schema_version, SUPPORTED_SCHEMA_VERSION
                ),
            )
            .with_hint(format!(
                "set \"schema_version\": {}",
                SUPPORTED_SCHEMA_VERSION
            )),
        );
    }

    if doc.process.id.trim().is_empty() {
        errors.push(DslError::new(
            DslErrorCode::InvalidType,
            "/process/id",
            "process id must be a non-empty string",
        ));
    }

    if doc.nodes.is_empty() {
        errors.push(
            DslError::new(
                DslErrorCode::EmptyNodeList,
                "/nodes",
                "the nodes list must not be empty",
            )
            .with_hint("a process needs at least a start and an end node"),
        );
    }

    let mut node_ids: HashMap<String, usize> = HashMap::new();
    for (i, node) in doc.nodes.iter().enumerate() {
        let base = format!("/nodes/{}", i);
        if node.id.trim().is_empty() {
            errors.push(DslError::new(
                DslErrorCode::InvalidType,
                format!("{}/id", base),
                "node id must be a non-empty string",
            ));
        } else if let Some(&first) = node_ids.get(&node.id) {
            errors.push(
                DslError::new(
                    DslErrorCode::DuplicateNodeId,
                    format!("{}/id", base),
                    format!(
                        "duplicate node id `{}` (first defined at /nodes/{})",
                        node.id, first
                    ),
                )
                .with_hint("node ids must be unique"),
            );
        } else {
            node_ids.insert(node.id.clone(), i);
        }
        errors.extend(check_node_fields(node, &base));
    }

    let mut edge_ids: HashMap<String, usize> = HashMap::new();
    for (i, edge) in doc.edges.iter().enumerate() {
        let base = format!("/edges/{}", i);
        if let Some(id) = &edge.id {
            if id.trim().is_empty() {
                errors.push(DslError::new(
                    DslErrorCode::InvalidType,
                    format!("{}/id", base),
                    "edge id must be a non-empty string when provided",
                ));
            }
        }
        let resolved = resolved_edge_id(edge, i);
        if let Some(&first) = edge_ids.get(&resolved) {
            errors.push(
                DslError::new(
                    DslErrorCode::DuplicateEdgeId,
                    format!("{}/id", base),
                    format!(
                        "duplicate edge id `{}` (first defined at /edges/{})",
                        resolved, first
                    ),
                )
                .with_hint("edge ids must be unique; omit `id` to auto-generate one"),
            );
        } else {
            edge_ids.insert(resolved, i);
        }

        if edge.source.trim().is_empty() {
            errors.push(DslError::new(
                DslErrorCode::InvalidType,
                format!("{}/source", base),
                "edge source must be a non-empty node id",
            ));
        } else if !node_ids.contains_key(&edge.source) {
            errors.push(
                DslError::new(
                    DslErrorCode::UnknownNodeRef,
                    format!("{}/source", base),
                    format!("edge source `{}` does not match any node id", edge.source),
                )
                .with_hint("check the `id` of the node you meant to connect from"),
            );
        }
        if edge.target.trim().is_empty() {
            errors.push(DslError::new(
                DslErrorCode::InvalidType,
                format!("{}/target", base),
                "edge target must be a non-empty node id",
            ));
        } else if !node_ids.contains_key(&edge.target) {
            errors.push(
                DslError::new(
                    DslErrorCode::UnknownNodeRef,
                    format!("{}/target", base),
                    format!("edge target `{}` does not match any node id", edge.target),
                )
                .with_hint("check the `id` of the node you meant to connect to"),
            );
        }

        if edge.condition.is_some() && edge.default == Some(true) {
            errors.push(
                DslError::new(
                    DslErrorCode::InvalidType,
                    format!("{}/condition", base),
                    "edge cannot set both `condition` and `default: true`",
                )
                .with_hint("a default flow takes no condition; remove one of them"),
            );
        }
        if let Some(condition) = &edge.condition {
            errors.extend(check_condition(condition, &format!("{}/condition", base)));
        }
    }

    errors
}

/// Per-node-type required/forbidden field checks.
fn check_node_fields(node: &DslNode, base: &str) -> Vec<DslError> {
    let mut errors = Vec::new();

    let fields: [(&str, bool); 6] = [
        ("task_type", node.task_type.is_some()),
        ("retries", node.retries.is_some()),
        ("timeout_secs", node.timeout_secs.is_some()),
        ("form_key", node.form_key.is_some()),
        ("form_fields", node.form_fields.is_some()),
        ("timer", node.timer.is_some()),
    ];
    for (field, is_present) in fields {
        if !is_present {
            continue;
        }
        let valid_for_type = match field {
            "task_type" | "retries" | "timeout_secs" => {
                matches!(node.node_type, DslNodeType::ServiceTask)
            }
            "form_key" | "form_fields" => matches!(node.node_type, DslNodeType::UserTask),
            "timer" => matches!(node.node_type, DslNodeType::TimerIntermediateCatch),
            _ => false,
        };
        if !valid_for_type {
            let hint = match field {
                "task_type" | "retries" | "timeout_secs" => {
                    "this field only applies to `serviceTask` nodes"
                }
                "form_key" | "form_fields" => "this field only applies to `userTask` nodes",
                _ => "this field only applies to `timerIntermediateCatch` nodes",
            };
            errors.push(
                DslError::new(
                    DslErrorCode::InvalidType,
                    format!("{}/{}", base, field),
                    format!(
                        "field `{}` is not valid for node type `{}`",
                        field, node.node_type
                    ),
                )
                .with_hint(hint),
            );
        }
    }

    match node.node_type {
        DslNodeType::ServiceTask => match &node.task_type {
            None => errors.push(
                DslError::new(
                    DslErrorCode::MissingField,
                    format!("{}/task_type", base),
                    "serviceTask nodes require `task_type`",
                )
                .with_hint("set the worker topic, e.g. \"task_type\": \"payment\""),
            ),
            Some(t) if t.trim().is_empty() => errors.push(DslError::new(
                DslErrorCode::InvalidType,
                format!("{}/task_type", base),
                "task_type must be a non-empty string",
            )),
            Some(_) => {}
        },
        DslNodeType::UserTask => {
            if let Some(form_fields) = &node.form_fields {
                for (j, field) in form_fields.iter().enumerate() {
                    errors.extend(check_form_field(
                        field,
                        &format!("{}/form_fields/{}", base, j),
                    ));
                }
            }
        }
        DslNodeType::TimerIntermediateCatch => match &node.timer {
            None => errors.push(
                DslError::new(
                    DslErrorCode::MissingField,
                    format!("{}/timer", base),
                    "timerIntermediateCatch nodes require `timer`",
                )
                .with_hint("set a duration, e.g. \"timer\": { \"duration\": \"PT1H\" }"),
            ),
            Some(timer) => {
                if timer.duration.trim().is_empty() {
                    errors.push(DslError::new(
                        DslErrorCode::InvalidType,
                        format!("{}/timer/duration", base),
                        "timer duration must be a non-empty string",
                    ));
                }
            }
        },
        _ => {}
    }

    errors
}

/// Per-form-field checks.
fn check_form_field(field: &DslFormField, base: &str) -> Vec<DslError> {
    let mut errors = Vec::new();
    if field.id.trim().is_empty() {
        errors.push(DslError::new(
            DslErrorCode::InvalidType,
            format!("{}/id", base),
            "form field id must be a non-empty string",
        ));
    }
    errors
}

/// Condition value safety checks.
fn check_condition(condition: &DslCondition, base: &str) -> Vec<DslError> {
    let mut errors = Vec::new();
    match condition {
        DslCondition::VariableEq { variable, value } => {
            if variable.trim().is_empty() {
                errors.push(DslError::new(
                    DslErrorCode::InvalidType,
                    format!("{}/variable", base),
                    "condition variable must be a non-empty string",
                ));
            } else if variable.contains('"')
                || variable.contains('=')
                || variable.chars().any(char::is_whitespace)
            {
                errors.push(
                    DslError::new(
                        DslErrorCode::InvalidType,
                        format!("{}/variable", base),
                        "condition variable must not contain double quotes, `=`, or whitespace",
                    )
                    .with_hint("use an `expression` condition for advanced variable names"),
                );
            }
            if value.contains('"') {
                errors.push(
                    DslError::new(
                        DslErrorCode::InvalidType,
                        format!("{}/value", base),
                        "condition value must not contain double quotes",
                    )
                    .with_hint("use an `expression` condition for values with quotes"),
                );
            }
        }
        DslCondition::Expression { expression } => {
            let trimmed = expression.trim();
            let inner = trimmed
                .strip_prefix("${")
                .and_then(|t| t.strip_suffix('}'))
                .map(str::trim)
                .unwrap_or(trimmed);
            if inner.is_empty() {
                errors.push(DslError::new(
                    DslErrorCode::InvalidType,
                    format!("{}/expression", base),
                    "condition expression must not be empty",
                ));
            }
        }
    }
    errors
}
