---
layout: default
---

# Variables & Artifact URIs — Contract Reference (E2-3)

> This is the **contract reference for AI agents and external workers** that
> write or read process variables. The engine-side decision record is
> `docs/adr/ADR-007-structured-variables-and-artifact-uris.md`.

---

## 1. The variable map is `HashMap<String, String>` — and stays that way

Process variables are stored and transported as a plain string map
(`POST /api/v1/process-instances`, `variables` in task completion,
fetch-and-lock delivery, `GET /api/v1/process-instances/:id`). Structured
values are layered on top as a **self-describing value format**:

| Value form | Meaning | Who may produce it |
|------------|---------|--------------------|
| *plain string* (any text not starting with `__var:` or `__encrypted:`) | Legacy string variable. Semantics unchanged forever; gateway conditions and the expression language see exactly this string. | anyone |
| `__var:1:<compact JSON>` | Versioned envelope (§2) holding a JSON value or an artifact reference. | anyone |
| `__encrypted:<b64 nonce>:<b64 ciphertext>` | AES-256-GCM ciphertext (existing format, unchanged). | the server (via `encrypted` list + `BPM_VARIABLE_ENCRYPTION_KEY`) |
| `__truncated:<len>:<12 hex>:` | History-only redaction marker (§5). Never a legal input value. | engine (history writes) |
| `__opaque:` | History-only marker for unknown envelope versions (§5). Never a legal input value. | engine (history writes) |

The `__`-prefixed namespace is reserved by the engine. Do not invent new
`__`-prefixed value formats.

## 2. Envelope format (version 1)

A value carrying the prefix `__var:1:` is compact JSON with a `t` (type) tag:

| `t` | Fields | Example |
|-----|--------|---------|
| `json` | `val`: any JSON value (object, array, string, number, bool, null) | `__var:1:{"t":"json","val":{"items":[1,2,3]}}` |
| `artifact` | `uri` (required); `size` (optional, bytes, ≥ 0); `sha256` (optional, exactly 64 hex chars); `media_type` (optional) | `__var:1:{"t":"artifact","uri":"https://storage.example.com/reports/q3.pdf","size":102400,"sha256":"a1b2...8f90","media_type":"application/pdf"}` |

Rules:

- Serialization is compact (no spaces); field order is `t, val, uri, size,
  sha256, media_type`; optional fields are omitted when absent.
- Unknown fields in a v1 envelope are ignored on read (forward-compatible
  within v1).
- **Unknown envelope versions** (e.g. a future `__var:2:`) parse to an
  `UnsupportedVersion` error. Treat such values as opaque: never interpret,
  never rewrite. The engine redacts them to `__opaque:` in history.
- **Envelope values are opaque to conditions**: gateway conditions and the
  EL compare plain strings. Route on a plain variable, not on an envelope.

## 3. Artifact URI rules

The engine **never dereferences artifact URIs** — resolution is owned by
workers/clients. A submitted artifact envelope must pass validation:

1. Absolute URI: non-empty scheme (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`).
2. No userinfo (`@` inside the authority, i.e. `scheme://user@host/...`).
3. No fragment (no `#` anywhere).
4. Total length ≤ 2048 bytes.
5. `size` ≥ 0 when present (negatives fail envelope parsing).
6. `sha256` exactly 64 hex characters when present (upper or lower case).

`artifact://…` is a **reserved** scheme for a future built-in object store:
currently valid but opaque — the engine does not resolve it either.

## 4. Size caps (enforced at REST write boundaries)

| Limit | Default | Config | Env |
|-------|---------|--------|-----|
| max stored variable value (final stored string, envelope included) | 65_536 bytes | `[engine] variable_max_bytes` | `BPM_ENGINE__VARIABLE_MAX_BYTES` |
| max variable value kept verbatim in event history | 1024 bytes | `[engine] history_variable_max_bytes` | `BPM_ENGINE__HISTORY_VARIABLE_MAX_BYTES` |

Enforced at **start-instance**, **user-task complete**, and
**external-task complete**. At start-instance the check runs on the submitted
value and again after encryption (ciphertext inflation counts).

Oversized payloads: store them externally (your object storage) and submit
an artifact envelope referencing them.

## 5. Sensitive variables must be encrypted

`POST /api/v1/process-instances` accepts a `sensitive` name list. Every
sensitive name must (a) exist in the submitted variables and (b) be listed in
`encrypted` — otherwise 4xx `sensitive_requires_encryption`. Rationale:
encrypted values are ciphertext at rest, so event history and logs never
contain the plaintext. GET decrypts for callers holding
`BPM_VARIABLE_ENCRYPTION_KEY` (display masking is a UI concern).

Envelope + encryption compose: encryption applies to the final stored string
(an envelope may be encrypted as a whole; decrypt first, then parse the
envelope).

## 6. History redaction (what lands in event history)

For **new** history writes, each variable value is redacted before append:

- longer than `history_variable_max_bytes` → `__truncated:<orig_len>:<sha256-first-12-hex>:`
  (e.g. `__truncated:2048:1d1801f753cc:`)
- unknown envelope version → `__opaque:`
- everything else (plain strings, valid envelopes, `__encrypted:` ciphertext)
  → verbatim

Existing history rows are never rewritten. Instance variables are never
truncated — `GET /api/v1/process-instances/:id` always returns the full
value; only event history is capped.

## 7. Workers: fetch-and-lock delivers envelope strings verbatim

`POST /api/v1/external-tasks/fetch-and-lock` returns the task's variable map
exactly as stored: envelope strings appear **as-is** in `variables`. Parse
them per this contract:

```text
if value starts with "__var:1:"  -> parse the JSON body (this document)
if value starts with "__encrypted:" -> ciphertext (server-side key; workers do not decrypt)
else                               -> plain string
```

When completing a task, submit envelope strings under the same rules
(valid envelope, valid artifact URI, within the size cap).

## 8. Error codes (stable, REST 4xx)

| Code | Boundary | Meaning |
|------|----------|---------|
| `variable_too_large` | all three write boundaries | value exceeds `variable_max_bytes`; message names the variable, the limit, and the hint (store payloads externally, reference via artifact envelope) |
| `variable_invalid_envelope` | all three write boundaries | envelope failed to parse (malformed JSON, wrong shape, unknown kind, or unsupported version — details in the message) |
| `variable_invalid_artifact_uri` | all three write boundaries | artifact URI/metadata failed validation (userinfo / fragment / length / scheme / sha256 shape) |
| `sensitive_requires_encryption` | start-instance | a `sensitive` name is missing from the variables or not listed in `encrypted` |

Error body shape (extends the existing `ErrorResponse`; `code` is omitted on
legacy errors):

```json
{
  "error": "variable 'big' is 71680 bytes, exceeding the 65536 byte limit; store large payloads externally and reference them via an artifact envelope",
  "code": "variable_too_large"
}
```

## 9. Reference client behavior (summary)

| Operation | Behavior |
|-----------|----------|
| start instance with envelope variable | accepted; stored verbatim; visible verbatim on GET; delivered verbatim to workers |
| start instance with plain variable | unchanged legacy behavior (drives conditions) |
| GET instance | envelope strings verbatim; `__encrypted:` decrypted when key configured |
| GET history | redacted per §6 (new writes) |
| fetch-and-lock | envelope strings verbatim in `variables` |
| complete (user/external task) | same validation as start (envelope, artifact URI, size cap) |
