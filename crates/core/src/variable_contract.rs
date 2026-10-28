//! Structured variable value contract (E2-3): versioned envelopes, artifact
//! URI rules, and history redaction.
//!
//! The storage and wire representation of process variables remains
//! `HashMap<String, String>` (instances, event payloads, external task
//! variables, adapters, DB columns). This module layers a **self-describing
//! value format** on top of that representation instead of changing it:
//!
//! - A value carrying the [`ENVELOPE_PREFIX`] (`__var:1:<compact JSON>`) is a
//!   versioned envelope holding either an arbitrary JSON value or an external
//!   artifact reference.
//! - A value carrying the legacy [`ENCRYPTED_PREFIX`](crate::ENCRYPTED_PREFIX)
//!   is ciphertext, exactly as before.
//! - Any other value is a legacy plain string — forever valid, unchanged
//!   semantics. Gateway conditions and the EL only ever see plain strings or
//!   opaque envelope strings.
//!
//! The engine **never dereferences artifact URIs**; workers and clients own
//! resolution (see `docs/variables-and-artifacts.md` and ADR-007).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Prefix shared by every envelope version (`__var:`).
///
/// Any value starting with this prefix is envelope-family input: it is either
/// parsed as a supported envelope or reported as an unsupported version.
pub const ENVELOPE_FAMILY_PREFIX: &str = "__var:";

/// Prefix of envelope version 1: values are stored as
/// `__var:1:<compact JSON>`.
pub const ENVELOPE_PREFIX: &str = "__var:1:";

/// Marker replacing oversized variable values in event history:
/// `__truncated:<orig_len>:<sha256-first-12-hex>:`.
pub const TRUNCATED_PREFIX: &str = "__truncated:";

/// Marker replacing values with an unknown envelope version in event history.
pub const OPAQUE_PREFIX: &str = "__opaque:";

/// Maximum total byte length of an artifact URI.
pub const MAX_ARTIFACT_URI_BYTES: usize = 2048;

/// Number of leading hex characters of the SHA-256 digest kept in a
/// [`TRUNCATED_PREFIX`] marker.
pub const TRUNCATED_DIGEST_HEX_CHARS: usize = 12;

/// Default per-value cap for variables written into event history (bytes).
pub const DEFAULT_HISTORY_VARIABLE_MAX_BYTES: usize = 1024;

/// Errors from the variable value contract (envelope parsing, artifact URI
/// validation).
///
/// Every variant carries a stable machine-readable [`code`](Self::code) that
/// REST boundary errors reuse.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VariableContractError {
    /// The value carries an envelope version this build cannot interpret
    /// (e.g. a future `__var:2:`). History and output must treat such values
    /// as opaque — redact, never interpret.
    #[error("unsupported envelope version {version:?} (supported: 1)")]
    UnsupportedVersion {
        /// The version segment as written in the storage string.
        version: String,
    },
    /// The value carries the v1 envelope prefix but the JSON body is invalid
    /// (not JSON, wrong shape, missing required field, unknown kind, or a
    /// negative `size`).
    #[error("malformed envelope: {reason}")]
    MalformedEnvelope {
        /// Human-readable reason (included in REST error bodies).
        reason: String,
    },
    /// An artifact envelope failed URI/metadata validation (userinfo, fragment,
    /// length, scheme, or sha256 shape).
    #[error("invalid artifact URI: {reason}")]
    InvalidArtifactUri {
        /// Human-readable reason (included in REST error bodies).
        reason: String,
    },
}

impl VariableContractError {
    /// Stable machine-readable error code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            VariableContractError::UnsupportedVersion { .. } => {
                "variable_envelope_unsupported_version"
            }
            VariableContractError::MalformedEnvelope { .. } => "variable_invalid_envelope",
            VariableContractError::InvalidArtifactUri { .. } => "variable_invalid_artifact_uri",
        }
    }
}

/// A versioned variable envelope: a self-describing structured value stored
/// inside the plain string variable map.
///
/// # Example
///
/// ```
/// use bpm_engine_core::{parse_envelope, VariableEnvelope};
///
/// let env = VariableEnvelope::Json {
///     val: serde_json::json!({"items": [1, 2, 3]}),
/// };
/// let storage = env.to_storage_string();
/// assert!(storage.starts_with("__var:1:"));
///
/// let parsed = parse_envelope(&storage).unwrap().unwrap();
/// assert_eq!(parsed, env);
///
/// // Plain strings are not envelopes
/// assert!(parse_envelope("just a string").is_none());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum VariableEnvelope {
    /// An arbitrary JSON value.
    Json {
        /// The embedded JSON value (any type, including null).
        val: serde_json::Value,
    },
    /// A reference to an external artifact stored outside the engine.
    ///
    /// The engine never dereferences `uri`; workers and clients own
    /// resolution. `artifact://` is reserved for a future built-in object
    /// store — valid but opaque.
    Artifact {
        /// Absolute URI of the artifact (no userinfo, no fragment, ≤ 2048
        /// bytes).
        uri: String,
        /// Artifact byte size when known (≥ 0; negatives fail parsing).
        size: Option<u64>,
        /// SHA-256 digest of the artifact content as exactly 64 hex chars
        /// when known.
        sha256: Option<String>,
        /// IANA media type of the artifact content when known.
        media_type: Option<String>,
    },
}

/// Internal DTO for the compact envelope JSON.
///
/// Field order fixes the serialized form: `t`, `val`, `uri`, `size`,
/// `sha256`, `media_type` (optional fields are skipped when absent).
/// Unknown fields are ignored on read (lenient within v1).
#[derive(Serialize, Deserialize)]
struct EnvelopeDto {
    t: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_val"
    )]
    val: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

/// Deserialize `val` so that an explicit JSON `null` stays
/// `Some(Value::Null)` (a plain `Option<Value>` would map JSON `null` to
/// `None`, losing the distinction between "absent" and "null").
fn deserialize_val<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

impl From<&VariableEnvelope> for EnvelopeDto {
    fn from(env: &VariableEnvelope) -> Self {
        match env {
            VariableEnvelope::Json { val } => EnvelopeDto {
                t: "json".to_string(),
                val: Some(val.clone()),
                uri: None,
                size: None,
                sha256: None,
                media_type: None,
            },
            VariableEnvelope::Artifact {
                uri,
                size,
                sha256,
                media_type,
            } => EnvelopeDto {
                t: "artifact".to_string(),
                val: None,
                uri: Some(uri.clone()),
                size: *size,
                sha256: sha256.clone(),
                media_type: media_type.clone(),
            },
        }
    }
}

impl VariableEnvelope {
    /// Encode into the storage string form `__var:1:<compact JSON>`.
    ///
    /// Infallible in practice: JSON values cannot hold non-finite floats or
    /// non-string object keys, so compact serialization cannot fail.
    pub fn to_storage_string(&self) -> String {
        let dto = EnvelopeDto::from(self);
        let body = serde_json::to_string(&dto)
            .expect("envelope JSON serialization is infallible for Value payloads");
        format!("{}{}", ENVELOPE_PREFIX, body)
    }

    /// Validate this envelope's contract invariants.
    ///
    /// JSON envelopes are always valid once parsed. Artifact envelopes must
    /// pass [`validate_artifact_uri`].
    pub fn validate(&self) -> Result<(), VariableContractError> {
        match self {
            VariableEnvelope::Json { .. } => Ok(()),
            VariableEnvelope::Artifact {
                uri, size, sha256, ..
            } => validate_artifact_uri(uri, *size, sha256.as_deref()),
        }
    }
}

/// Parse a storage string into a [`VariableEnvelope`].
///
/// Returns:
/// - `None` when the value is not envelope-family input (plain legacy string
///   or `__encrypted:` ciphertext) — such values stay opaque to this contract.
/// - `Some(Err(UnsupportedVersion))` for a future envelope version (e.g.
///   `__var:2:`) — treat as opaque, never interpret.
/// - `Some(Err(MalformedEnvelope))` when the v1 body is invalid.
/// - `Some(Ok(envelope))` on success.
///
/// Artifact URI/metadata validation is **not** performed here; call
/// [`validate_artifact_uri`] or [`VariableEnvelope::validate`] at write
/// boundaries.
pub fn parse_envelope(s: &str) -> Option<Result<VariableEnvelope, VariableContractError>> {
    let rest = s.strip_prefix(ENVELOPE_FAMILY_PREFIX)?;
    let (version, body) = match rest.split_once(':') {
        Some(parts) => parts,
        None => {
            return Some(Err(VariableContractError::UnsupportedVersion {
                version: rest.to_string(),
            }))
        }
    };
    if version != "1" {
        return Some(Err(VariableContractError::UnsupportedVersion {
            version: version.to_string(),
        }));
    }
    Some(parse_v1_body(body))
}

fn parse_v1_body(body: &str) -> Result<VariableEnvelope, VariableContractError> {
    let malformed = |reason: String| VariableContractError::MalformedEnvelope { reason };
    let dto: EnvelopeDto =
        serde_json::from_str(body).map_err(|e| malformed(format!("invalid JSON: {e}")))?;
    match dto.t.as_str() {
        "json" => {
            let val = dto
                .val
                .ok_or_else(|| malformed("json envelope requires \"val\"".to_string()))?;
            Ok(VariableEnvelope::Json { val })
        }
        "artifact" => {
            let uri = dto
                .uri
                .ok_or_else(|| malformed("artifact envelope requires \"uri\"".to_string()))?;
            Ok(VariableEnvelope::Artifact {
                uri,
                size: dto.size,
                sha256: dto.sha256,
                media_type: dto.media_type,
            })
        }
        other => Err(malformed(format!(
            "unknown envelope kind {other:?} (expected \"json\" or \"artifact\")"
        ))),
    }
}

/// Validate the URI and metadata of an artifact envelope value.
///
/// Rules (ADR-007):
/// - absolute URI: scheme must be non-empty and syntactically valid
///   (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`)
/// - no userinfo (`@` in the authority component)
/// - no fragment (`#` anywhere)
/// - total length ≤ [`MAX_ARTIFACT_URI_BYTES`] (2048)
/// - `size` ≥ 0 when present — trivially true for `u64`; a negative value
///   fails envelope parsing before this check runs
/// - `sha256` must be exactly 64 hex characters when present
///
/// `artifact://` is a **reserved** scheme for a future built-in object store:
/// valid but opaque — the engine never dereferences any URI.
pub fn validate_artifact_uri(
    uri: &str,
    size: Option<u64>,
    sha256: Option<&str>,
) -> Result<(), VariableContractError> {
    let invalid = |reason: String| VariableContractError::InvalidArtifactUri { reason };
    if uri.is_empty() {
        return Err(invalid("artifact URI must not be empty".to_string()));
    }
    if uri.len() > MAX_ARTIFACT_URI_BYTES {
        return Err(invalid(format!(
            "artifact URI is {} bytes, exceeding the {} byte limit",
            uri.len(),
            MAX_ARTIFACT_URI_BYTES
        )));
    }
    if uri.contains('#') {
        return Err(invalid(
            "artifact URI must not contain a fragment".to_string(),
        ));
    }
    let colon = uri.find(':').ok_or_else(|| {
        invalid("artifact URI must be absolute (non-empty scheme followed by ':')".to_string())
    })?;
    let scheme = &uri[..colon];
    if scheme.is_empty() {
        return Err(invalid("artifact URI scheme must not be empty".to_string()));
    }
    if !is_valid_scheme(scheme) {
        return Err(invalid(format!(
            "artifact URI has an invalid scheme {scheme:?}"
        )));
    }
    // Userinfo is only meaningful inside an authority ("scheme://..."), so
    // check the authority component when the URI has one.
    if let Some(after_scheme) = uri[colon + 1..].strip_prefix("//") {
        let authority_end = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
        if after_scheme[..authority_end].contains('@') {
            return Err(invalid(
                "artifact URI must not contain userinfo".to_string(),
            ));
        }
    }
    // `size` is u64: negative values are rejected during envelope parsing,
    // so "size >= 0 when present" holds by construction here.
    let _ = size;
    if let Some(digest) = sha256 {
        if !is_sha256_hex(digest) {
            return Err(invalid(format!(
                "sha256 must be exactly 64 hex characters, got {} characters",
                digest.len()
            )));
        }
    }
    Ok(())
}

/// A scheme is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` (RFC 3986).
fn is_valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Exactly 64 hex characters (upper or lower case).
fn is_sha256_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// History redaction
// ---------------------------------------------------------------------------

/// Policy for redacting variable values written into event history.
///
/// Redaction is **on by default** ([`Default`]) with the
/// [`DEFAULT_HISTORY_VARIABLE_MAX_BYTES`] cap, so large payloads never land
/// in event history; values at or below the cap are kept verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRedactionPolicy {
    /// Maximum byte length of a variable value kept verbatim in history.
    /// Longer values are replaced with a `__truncated:` marker.
    pub max_variable_bytes: usize,
}

impl Default for HistoryRedactionPolicy {
    fn default() -> Self {
        Self {
            max_variable_bytes: DEFAULT_HISTORY_VARIABLE_MAX_BYTES,
        }
    }
}

/// Redact one variable value for event history.
///
/// - Longer than the policy cap → `__truncated:<orig_len>:<sha256-first-12-hex>:`
/// - Unknown envelope version (e.g. `__var:2:…`) → `__opaque:`
/// - Everything else (plain strings, valid envelopes, `__encrypted:`
///   ciphertext) → verbatim.
///
/// Pure function; the runtime applies it to the known variable-carrying
/// payload fields (`variables`, `initial_variables`) before appending events
/// to history. Existing history rows are never rewritten.
pub fn redact_history_variable_value(value: &str, policy: &HistoryRedactionPolicy) -> String {
    if value.len() > policy.max_variable_bytes {
        let digest = sha256_hex(value);
        return format!(
            "{}{}:{}:",
            TRUNCATED_PREFIX,
            value.len(),
            &digest[..TRUNCATED_DIGEST_HEX_CHARS]
        );
    }
    if matches!(
        parse_envelope(value),
        Some(Err(VariableContractError::UnsupportedVersion { .. }))
    ) {
        return OPAQUE_PREFIX.to_string();
    }
    value.to_string()
}

/// SHA-256 digest of the value's UTF-8 bytes as lowercase hex.
fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(hex_digit(byte >> 4));
        hex.push(hex_digit(byte & 0x0f));
    }
    hex
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + nibble - 10) as char,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256("hello") — well-known vector.
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    // --- Round trips -------------------------------------------------------

    #[test]
    fn json_envelope_round_trip_object() {
        let env = VariableEnvelope::Json {
            val: serde_json::json!({"items": [1, 2, 3], "ok": true}),
        };
        let storage = env.to_storage_string();
        assert!(storage.starts_with(ENVELOPE_PREFIX));
        assert_eq!(parse_envelope(&storage), Some(Ok(env)));
    }

    #[test]
    fn json_envelope_round_trip_scalars() {
        for val in [
            serde_json::json!(42),
            serde_json::json!(-1.5),
            serde_json::json!("text"),
            serde_json::json!(true),
            serde_json::json!(null),
            serde_json::json!([1, "two", null]),
        ] {
            let env = VariableEnvelope::Json { val: val.clone() };
            let storage = env.to_storage_string();
            assert_eq!(
                parse_envelope(&storage),
                Some(Ok(env)),
                "round trip failed for {val}"
            );
        }
    }

    #[test]
    fn artifact_envelope_round_trip_full() {
        let env = VariableEnvelope::Artifact {
            uri: "https://storage.example.com/reports/2026.pdf".to_string(),
            size: Some(102_400),
            sha256: Some("a".repeat(64)),
            media_type: Some("application/pdf".to_string()),
        };
        let storage = env.to_storage_string();
        assert!(storage.starts_with(ENVELOPE_PREFIX));
        assert_eq!(parse_envelope(&storage), Some(Ok(env)));
    }

    #[test]
    fn artifact_envelope_round_trip_uri_only() {
        let env = VariableEnvelope::Artifact {
            uri: "artifact://reports/2026/q3".to_string(),
            size: None,
            sha256: None,
            media_type: None,
        };
        let storage = env.to_storage_string();
        assert_eq!(parse_envelope(&storage), Some(Ok(env)));
        // Compact form: optional fields are omitted entirely.
        assert_eq!(
            storage,
            r#"__var:1:{"t":"artifact","uri":"artifact://reports/2026/q3"}"#
        );
    }

    #[test]
    fn json_envelope_compact_form() {
        let env = VariableEnvelope::Json {
            val: serde_json::json!(7),
        };
        assert_eq!(env.to_storage_string(), r#"__var:1:{"t":"json","val":7}"#);
    }

    // --- parse_envelope classification --------------------------------------

    #[test]
    fn plain_strings_are_not_envelopes() {
        assert!(parse_envelope("").is_none());
        assert!(parse_envelope("hello").is_none());
        assert!(parse_envelope("{}").is_none());
        assert!(parse_envelope("__var").is_none());
    }

    #[test]
    fn encrypted_values_are_not_envelopes() {
        assert!(parse_envelope("__encrypted:AAAA:BBBB").is_none());
    }

    #[test]
    fn future_version_is_unsupported() {
        let res = parse_envelope(r#"__var:2:{"t":"json","val":1}"#);
        assert!(matches!(
            res,
            Some(Err(VariableContractError::UnsupportedVersion { .. }))
        ));
        let Some(Err(e)) = res else {
            panic!("expected error");
        };
        assert_eq!(e.code(), "variable_envelope_unsupported_version");
        assert!(e.to_string().contains("2"));
    }

    #[test]
    fn bare_family_prefix_is_unsupported_version() {
        // "__var:" with nothing after it: version segment is empty.
        let res = parse_envelope("__var:");
        assert!(matches!(
            res,
            Some(Err(VariableContractError::UnsupportedVersion { .. }))
        ));
        // "__var" without the trailing colon: not even an envelope attempt.
        assert!(parse_envelope("__var:").is_some());
        assert!(parse_envelope("__var").is_none());
    }

    #[test]
    fn non_numeric_version_is_unsupported() {
        let res = parse_envelope("__var:oops:{}");
        assert!(matches!(
            res,
            Some(Err(VariableContractError::UnsupportedVersion { .. }))
        ));
    }

    #[test]
    fn malformed_json_body() {
        let res = parse_envelope("__var:1:{oops");
        assert!(matches!(
            res,
            Some(Err(VariableContractError::MalformedEnvelope { .. }))
        ));
    }

    #[test]
    fn empty_json_body() {
        let res = parse_envelope("__var:1:");
        assert!(matches!(
            res,
            Some(Err(VariableContractError::MalformedEnvelope { .. }))
        ));
    }

    #[test]
    fn json_envelope_missing_val() {
        let res = parse_envelope(r#"__var:1:{"t":"json"}"#);
        let Some(Err(e)) = res else {
            panic!("expected error");
        };
        assert!(matches!(e, VariableContractError::MalformedEnvelope { .. }));
        assert!(e.to_string().contains("val"));
    }

    #[test]
    fn artifact_envelope_missing_uri() {
        let res = parse_envelope(r#"__var:1:{"t":"artifact"}"#);
        let Some(Err(e)) = res else {
            panic!("expected error");
        };
        assert!(matches!(e, VariableContractError::MalformedEnvelope { .. }));
        assert!(e.to_string().contains("uri"));
    }

    #[test]
    fn unknown_envelope_kind() {
        let res = parse_envelope(r#"__var:1:{"t":"blob","val":1}"#);
        let Some(Err(e)) = res else {
            panic!("expected error");
        };
        assert!(matches!(e, VariableContractError::MalformedEnvelope { .. }));
        assert!(e.to_string().contains("blob"));
    }

    #[test]
    fn negative_size_is_malformed() {
        // "size >= 0 when present" is enforced at the type level: a negative
        // value fails to deserialize into u64.
        let res = parse_envelope(r#"__var:1:{"t":"artifact","uri":"https://x/y","size":-1}"#);
        assert!(matches!(
            res,
            Some(Err(VariableContractError::MalformedEnvelope { .. }))
        ));
    }

    // --- validate_artifact_uri ----------------------------------------------

    #[test]
    fn valid_https_artifact_uri() {
        assert!(
            validate_artifact_uri("https://storage.example.com/bucket/a.pdf", None, None).is_ok()
        );
    }

    #[test]
    fn reserved_artifact_scheme_is_valid_but_opaque() {
        assert!(validate_artifact_uri("artifact://reports/2026/q3", None, None).is_ok());
    }

    #[test]
    fn uri_without_authority_is_valid() {
        // scheme + path (no "//" authority), e.g. an object key style.
        assert!(validate_artifact_uri("s3:reports/2026/q3.bin", None, None).is_ok());
        assert!(validate_artifact_uri("urn:isbn:0451450523", None, None).is_ok());
    }

    #[test]
    fn uri_with_query_is_valid() {
        assert!(
            validate_artifact_uri("https://x.example.com/a?version=2&lang=en", None, None).is_ok()
        );
    }

    #[test]
    fn uri_with_userinfo_rejected() {
        let err = validate_artifact_uri("https://user:pass@x.example.com/a", None, None)
            .expect_err("userinfo must be rejected");
        assert!(err.to_string().contains("userinfo"));
        assert_eq!(err.code(), "variable_invalid_artifact_uri");
    }

    #[test]
    fn userinfo_without_double_slash_is_allowed() {
        // '@' outside an authority (path segment) is not userinfo.
        assert!(validate_artifact_uri("s3:bucket/obj@2x/name", None, None).is_ok());
    }

    #[test]
    fn uri_with_fragment_rejected() {
        let err = validate_artifact_uri("https://x.example.com/a#frag", None, None)
            .expect_err("fragment must be rejected");
        assert!(err.to_string().contains("fragment"));
    }

    #[test]
    fn relative_uri_rejected() {
        let err = validate_artifact_uri("not-a-uri", None, None)
            .expect_err("relative URI must be rejected");
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn empty_uri_rejected() {
        let err = validate_artifact_uri("", None, None).expect_err("empty URI must be rejected");
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn uri_missing_scheme_rejected() {
        let err = validate_artifact_uri("//storage.example.com/a", None, None)
            .expect_err("missing scheme must be rejected");
        assert!(err.to_string().contains("scheme"));
    }

    #[test]
    fn invalid_scheme_rejected() {
        // Scheme must start with ALPHA and contain only scheme characters.
        assert!(validate_artifact_uri("1https://x.example.com/a", None, None).is_err());
        assert!(validate_artifact_uri("ht tp://x.example.com/a", None, None).is_err());
    }

    #[test]
    fn oversized_uri_rejected_at_boundary() {
        let uri = format!("https://x.example.com/{}", "a".repeat(2048));
        let err = validate_artifact_uri(&uri, None, None).expect_err("long URI must be rejected");
        assert!(err.to_string().contains("exceeding"));
        // Exactly at the limit is fine.
        let exact = format!(
            "https://x.example.com/{}",
            "a".repeat(2048 - "https://x.example.com/".len())
        );
        assert_eq!(exact.len(), MAX_ARTIFACT_URI_BYTES);
        assert!(validate_artifact_uri(&exact, None, None).is_ok());
    }

    #[test]
    fn sha256_shape_enforced() {
        let ok_uri = "https://x.example.com/a";
        assert!(validate_artifact_uri(ok_uri, None, Some(&"a".repeat(64))).is_ok());
        // 63 chars
        let err = validate_artifact_uri(ok_uri, None, Some(&"a".repeat(63)))
            .expect_err("63-char sha256 must be rejected");
        assert!(err.to_string().contains("64"));
        // 65 chars
        assert!(validate_artifact_uri(ok_uri, None, Some(&"a".repeat(65))).is_err());
        // 64 chars but not hex
        assert!(validate_artifact_uri(ok_uri, None, Some(&"g".repeat(64))).is_err());
        // Uppercase hex is accepted
        assert!(validate_artifact_uri(ok_uri, None, Some(&"A".repeat(64))).is_ok());
    }

    #[test]
    fn size_zero_is_valid() {
        assert!(validate_artifact_uri("https://x.example.com/a", Some(0), None).is_ok());
    }

    #[test]
    fn envelope_validate_delegates_for_artifact() {
        let env = VariableEnvelope::Artifact {
            uri: "https://user@x.example.com/a".to_string(),
            size: None,
            sha256: None,
            media_type: None,
        };
        assert!(env.validate().is_err());
        let env_ok = VariableEnvelope::Artifact {
            uri: "https://x.example.com/a".to_string(),
            size: Some(1),
            sha256: None,
            media_type: None,
        };
        assert!(env_ok.validate().is_ok());
        let json = VariableEnvelope::Json {
            val: serde_json::json!(1),
        };
        assert!(json.validate().is_ok());
    }

    // --- redaction -----------------------------------------------------------

    #[test]
    fn redact_short_value_verbatim() {
        let policy = HistoryRedactionPolicy::default();
        assert_eq!(redact_history_variable_value("hello", &policy), "hello");
        assert_eq!(redact_history_variable_value("", &policy), "");
    }

    #[test]
    fn redact_value_exactly_at_limit_verbatim() {
        let policy = HistoryRedactionPolicy::default();
        let at_limit = "x".repeat(policy.max_variable_bytes);
        assert_eq!(
            redact_history_variable_value(&at_limit, &policy),
            at_limit,
            "value exactly at the cap must be kept verbatim"
        );
    }

    #[test]
    fn redact_value_one_over_limit_truncated() {
        let policy = HistoryRedactionPolicy::default();
        let value = "x".repeat(policy.max_variable_bytes + 1);
        let redacted = redact_history_variable_value(&value, &policy);
        let expected = format!(
            "{}{}:{}:",
            TRUNCATED_PREFIX,
            value.len(),
            &sha256_hex(&value)[..TRUNCATED_DIGEST_HEX_CHARS]
        );
        assert_eq!(redacted, expected);
        assert!(redacted.starts_with("__truncated:1025:"));
        // Marker carries the original length and a 12-hex digest.
        assert_ne!(redacted, value);
        assert!(redacted.len() < value.len());
    }

    #[test]
    fn redact_truncated_marker_uses_known_digest() {
        // SHA-256("hello") starts 2cf24dba5fb0 — guard the digest derivation
        // against implementation drift.
        let policy = HistoryRedactionPolicy {
            max_variable_bytes: 4,
        };
        assert_eq!(
            redact_history_variable_value("hello", &policy),
            "__truncated:5:2cf24dba5fb0:"
        );
        assert_eq!(HELLO_SHA256.len(), 64);
    }

    #[test]
    fn redact_unknown_envelope_version_opaque() {
        let policy = HistoryRedactionPolicy::default();
        assert_eq!(
            redact_history_variable_value(r#"__var:2:{"t":"json","val":1}"#, &policy),
            OPAQUE_PREFIX
        );
    }

    #[test]
    fn redact_known_envelope_verbatim() {
        let policy = HistoryRedactionPolicy::default();
        let env = VariableEnvelope::Json {
            val: serde_json::json!({"k": "v"}),
        };
        let storage = env.to_storage_string();
        assert_eq!(redact_history_variable_value(&storage, &policy), storage);
    }

    #[test]
    fn redact_encrypted_ciphertext_verbatim() {
        let policy = HistoryRedactionPolicy::default();
        // Ciphertext is opaque and passes through as-is.
        let ciphertext = "__encrypted:QUFB:QkJC";
        assert_eq!(
            redact_history_variable_value(ciphertext, &policy),
            ciphertext
        );
    }

    #[test]
    fn redact_long_unknown_version_still_truncates_first() {
        // Length beats opacity in evaluation order: an oversized
        // unknown-version value gets the compact __truncated: marker.
        let policy = HistoryRedactionPolicy::default();
        let value = format!("__var:2:{}", "x".repeat(2000));
        let redacted = redact_history_variable_value(&value, &policy);
        assert!(redacted.starts_with(&format!("{}{}:", TRUNCATED_PREFIX, value.len())));
    }

    #[test]
    fn redact_zero_cap_policy_truncates_everything_nonempty() {
        let policy = HistoryRedactionPolicy {
            max_variable_bytes: 0,
        };
        // Only the empty value survives a zero cap.
        assert_eq!(redact_history_variable_value("", &policy), "");
        let redacted = redact_history_variable_value("hi", &policy);
        assert!(redacted.starts_with("__truncated:2:"));
    }

    #[test]
    fn default_policy_uses_1024_cap() {
        assert_eq!(
            HistoryRedactionPolicy::default().max_variable_bytes,
            DEFAULT_HISTORY_VARIABLE_MAX_BYTES
        );
        assert_eq!(DEFAULT_HISTORY_VARIABLE_MAX_BYTES, 1024);
    }

    // --- sha256_hex ------------------------------------------------------------

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(sha256_hex("hello"), HELLO_SHA256);
        // Empty input vector.
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
