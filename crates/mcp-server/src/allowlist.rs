//! The MCP v1 tool surface and the minimal-permission allowlist model
//! (issue #20: 按最小权限授权 — minimal permission by allowlist).
//!
//! [`V1_SURFACE`] is the DECIDED, documented v1 subset of the 35-tool E4-1
//! catalog: definition validation/dry-run/deploy/version listing, instance
//! start/query/history/steps, the external-task lease lifecycle, user-task
//! list/complete, and dead-letter list/get/requeue. Tools outside the
//! surface (replay sessions, definition view/activate/deprecate, invariant
//! checks, health) are never exposed in v1 regardless of configuration.
//!
//! Resolution of the effective allowlist (fail closed on typos and unknown
//! names):
//!
//! | `BPM_MCP_ROLE` | `BPM_MCP_ALLOWED_TOOLS` | Effective tools |
//! | --- | --- | --- |
//! | unset | unset | [`DEFAULT_TOOLS`] — the read-only surface (reads + validate/dry-run) |
//! | set | unset | the E5-1 role preset |
//! | unset | set | the explicit list (intersected with the v1 surface) |
//! | set | set | role preset ∩ explicit list (pure down-scope) |
//!
//! The E5-1 role presets mirror the engine's role→permission matrix
//! (`docs/rbac.md`): the mutating tools of the surface are only exposed
//! when the configured credential's role may actually use them — an
//! `operator` preset never lists `fetch_and_lock_external_tasks`, a
//! `worker` preset never lists `deploy_process_definition`.
//!
//! This is the FIRST boundary; the engine remains the authority: every
//! tool call still authenticates with the configured credential and is
//! authorized + tenant-scoped engine-side (E5-1), so an allowlist mistake
//! degrades to engine-side 403/404, never to unauthorized access.

/// The MCP v1 tool surface: the tools this server can ever expose.
///
/// Order: read-only tools first, then the mutating ones (documentation
/// order, not behavior).
pub const V1_SURFACE: &[&str] = &[
    // read-only (12)
    "validate_process_definition",
    "dry_run_process_definition",
    "list_process_definitions",
    "list_definition_versions",
    "list_process_instances",
    "get_process_instance",
    "get_instance_history",
    "get_instance_steps",
    "list_tasks",
    "list_dead_letters",
    "get_dead_letter",
    "get_capabilities",
    // mutating (8)
    "deploy_process_definition",
    "start_process_instance",
    "fetch_and_lock_external_tasks",
    "complete_external_task",
    "fail_external_task",
    "extend_external_task_lock",
    "complete_user_task",
    "requeue_dead_letter",
];

/// The default exposure: the read-only tools of the v1 surface — every
/// read, plus the side-effect-free `validate` and `dry-run` POSTs (E2-2).
/// Mutating tools require an explicit opt-in ([`BPM_MCP_ROLE`] preset or
/// [`BPM_MCP_ALLOWED_TOOLS`]).
///
/// [`BPM_MCP_ROLE`]: crate::config#environment-variables
/// [`BPM_MCP_ALLOWED_TOOLS`]: crate::config#environment-variables
pub const DEFAULT_TOOLS: &[&str] = &[
    "validate_process_definition",
    "dry_run_process_definition",
    "list_process_definitions",
    "list_definition_versions",
    "list_process_instances",
    "get_process_instance",
    "get_instance_history",
    "get_instance_steps",
    "list_tasks",
    "list_dead_letters",
    "get_dead_letter",
    "get_capabilities",
];

/// The E5-1 roles with MCP presets.
pub const ROLES: &[&str] = &["auditor", "deployer", "operator", "worker"];

/// The tool preset for an E5-1 role (reads + the role's mutating tools,
/// all within the v1 surface).
pub fn role_preset(role: &str) -> Option<Vec<&'static str>> {
    let preset: Vec<&'static str> = match role {
        "auditor" => DEFAULT_TOOLS.to_vec(),
        "deployer" => {
            let mut tools = DEFAULT_TOOLS.to_vec();
            tools.push("deploy_process_definition");
            tools
        }
        "operator" => {
            let mut tools = DEFAULT_TOOLS.to_vec();
            tools.extend([
                "start_process_instance",
                "complete_user_task",
                "requeue_dead_letter",
            ]);
            tools
        }
        "worker" => {
            let mut tools = DEFAULT_TOOLS.to_vec();
            tools.extend([
                "fetch_and_lock_external_tasks",
                "complete_external_task",
                "fail_external_task",
                "extend_external_task_lock",
            ]);
            tools
        }
        _ => return None,
    };
    Some(preset)
}

/// Resolve the effective allowlist from (optional) env-shaped inputs.
///
/// See the module docs for the resolution table. Unknown role names and
/// `BPM_MCP_ALLOWED_TOOLS` entries that are not v1 surface tools fail
/// with an error naming the valid values — a security boundary fails
/// closed on typos instead of silently exposing nothing (or worse,
/// guessing).
pub fn resolve(role: Option<&str>, allowed: Option<&str>) -> Result<Vec<String>, String> {
    let base: Vec<&'static str> = match role {
        Some(r) => role_preset(r)
            .ok_or_else(|| format!("unknown BPM_MCP_ROLE {r:?}: expected one of {ROLES:?}"))?,
        None => DEFAULT_TOOLS.to_vec(),
    };

    let effective: Vec<String> = match allowed {
        None => base.iter().map(|s| (*s).to_string()).collect(),
        Some(list) => {
            let names: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            for name in &names {
                if !V1_SURFACE.contains(&name.as_str()) {
                    return Err(format!(
                        "BPM_MCP_ALLOWED_TOOLS entry {name:?} is not part of the MCP v1 tool \
                         surface; valid names: {V1_SURFACE:?}"
                    ));
                }
            }
            if role.is_some() {
                // Both set: the explicit list can only NARROW the preset.
                base.into_iter()
                    .filter(|b| names.iter().any(|n| n == b))
                    .map(str::to_string)
                    .collect()
            } else {
                names
            }
        }
    };

    // Re-order (and de-duplicate) into the stable v1 surface order.
    let ordered = V1_SURFACE
        .iter()
        .filter(|name| effective.iter().any(|e| e == *name))
        .map(|name| (*name).to_string())
        .collect();
    Ok(ordered)
}

/// Resolve the effective allowlist from the process environment
/// (`BPM_MCP_ROLE`, `BPM_MCP_ALLOWED_TOOLS`).
pub fn resolve_from_env() -> Result<Vec<String>, String> {
    let role = read_env("BPM_MCP_ROLE");
    let allowed = read_env("BPM_MCP_ALLOWED_TOOLS");
    resolve(role.as_deref(), allowed.as_deref())
}

/// Enforce the v1 surface boundary structurally: every name a caller wants
/// to expose must be a v1 surface tool. Enforced again in
/// `McpServer::new` so no code path can bypass the decided surface.
pub fn ensure_v1(names: &[String]) -> Result<(), String> {
    for name in names {
        if !V1_SURFACE.contains(&name.as_str()) {
            return Err(format!(
                "tool {name:?} is not part of the MCP v1 tool surface; \
                 valid names: {V1_SURFACE:?}"
            ));
        }
    }
    Ok(())
}

/// Read a non-empty env var (trimmed), `None` when unset/blank.
fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_catalog_subset() {
        let catalog = crate::catalog::Catalog::embedded().expect("catalog");
        for name in V1_SURFACE {
            assert!(catalog.find(name).is_some(), "{name} missing from catalog");
        }
    }

    #[test]
    fn presets_stay_within_the_surface() {
        for role in ROLES {
            let preset = role_preset(role).expect("known role");
            for name in &preset {
                assert!(V1_SURFACE.contains(name), "{role} preset leaks {name}");
            }
        }
        assert!(role_preset("admin").is_none());
        assert!(role_preset("").is_none());
    }

    #[test]
    fn presets_split_by_engine_role() {
        let worker = role_preset("worker").unwrap();
        assert!(worker.contains(&"fetch_and_lock_external_tasks"));
        assert!(!worker.contains(&"deploy_process_definition"));
        let deployer = role_preset("deployer").unwrap();
        assert!(deployer.contains(&"deploy_process_definition"));
        assert!(!deployer.contains(&"start_process_instance"));
        let operator = role_preset("operator").unwrap();
        assert!(operator.contains(&"start_process_instance"));
        assert!(!operator.contains(&"fetch_and_lock_external_tasks"));
        assert_eq!(role_preset("auditor").unwrap(), DEFAULT_TOOLS.to_vec());
    }

    #[test]
    fn default_is_read_only() {
        let default = resolve(None, None).expect("resolves");
        let catalog = crate::catalog::Catalog::embedded().expect("catalog");
        assert_eq!(default.len(), DEFAULT_TOOLS.len());
        for name in &default {
            let tool = catalog.find(name).unwrap();
            assert!(
                tool.classification.is_read_only(),
                "{name} is not read_only — the default surface must be read-heavy"
            );
        }
    }

    #[test]
    fn explicit_allowlist_enables_mutating_tools() {
        // Surface order: validate before deploy.
        let tools = resolve(
            None,
            Some("deploy_process_definition,validate_process_definition,deploy_process_definition"),
        )
        .expect("resolves");
        assert_eq!(
            tools,
            vec![
                "validate_process_definition".to_string(),
                "deploy_process_definition".to_string(),
            ]
        );
    }

    #[test]
    fn explicit_allowlist_down_scopes_role_presets() {
        let tools = resolve(
            Some("worker"),
            Some("fetch_and_lock_external_tasks,list_process_instances"),
        )
        .expect("resolves");
        assert_eq!(
            tools,
            vec![
                "list_process_instances".to_string(),
                "fetch_and_lock_external_tasks".to_string(),
            ]
        );
    }

    #[test]
    fn typos_and_out_of_surface_names_fail_closed() {
        let err = resolve(None, Some("deploy_process_defintion")).expect_err("typo fails");
        assert!(err.contains("not part of the MCP v1 tool surface"), "{err}");
        // A real catalog tool OUTSIDE the v1 surface is refused too.
        let err = resolve(None, Some("activate_definition")).expect_err("out of surface fails");
        assert!(err.contains("not part of the MCP v1 tool surface"), "{err}");
        let err = resolve(Some("root"), None).expect_err("unknown role fails");
        assert!(err.contains("BPM_MCP_ROLE"), "{err}");
    }

    #[test]
    fn ensure_v1_guards_the_boundary() {
        assert!(ensure_v1(&["get_capabilities".to_string()]).is_ok());
        let err = ensure_v1(&["delete_dead_letter".to_string()]).expect_err("outside v1");
        assert!(err.contains("v1 tool surface"), "{err}");
    }
}
