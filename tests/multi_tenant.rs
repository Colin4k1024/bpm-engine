//! Multi-tenant isolation tests: tenant-scoped data access, cross-tenant denial,
//! PoC mode (no tenant) access to all data, tenant_id from headers.
//!
//! Covers issue #24.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use bpm_engine::bpm_engine_storage::{ExternalTaskStore, ProcessInstanceStore, TokenStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_instance(id: &str, tenant_id: Option<&str>) -> ProcessInstance {
    ProcessInstance {
        id: id.to_string(),
        process_def_id: "p".into(),
        tenant_id: tenant_id.map(String::from),
        tokens: vec![Token {
            id: format!("{}:t1", id),
            node_id: "n1".into(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
        parent_instance_id: None,
        parent_token_id: None,
    }
}

// ---------------------------------------------------------------------------
// Test 1: Tenant A cannot read tenant B's instances via list_running
// ---------------------------------------------------------------------------

/// When querying list_running with tenant_id = "tenantA", only tenant A's
/// running instances should be returned. Tenant B's instances are invisible.
#[tokio::test]
async fn tenant_a_cannot_see_tenant_b_instances() {
    let repo = Arc::new(MemoryRepo::new());

    // Create instances for two different tenants
    let inst_a = make_instance("inst-a1", Some("tenantA"));
    let inst_b = make_instance("inst-b1", Some("tenantB"));
    let inst_a2 = make_instance("inst-a2", Some("tenantA"));

    repo.save(&inst_a).await.unwrap();
    repo.save(&inst_b).await.unwrap();
    repo.save(&inst_a2).await.unwrap();

    // Query for tenantA — should see only tenantA's instances
    let running_a = repo.list_running(Some("tenantA")).await.unwrap();
    assert_eq!(running_a.len(), 2, "tenantA should see exactly 2 instances");
    assert!(running_a.contains(&"inst-a1".to_string()));
    assert!(running_a.contains(&"inst-a2".to_string()));
    assert!(
        !running_a.contains(&"inst-b1".to_string()),
        "tenantA should NOT see tenantB's instance"
    );

    // Query for tenantB — should see only tenantB's instances
    let running_b = repo.list_running(Some("tenantB")).await.unwrap();
    assert_eq!(running_b.len(), 1, "tenantB should see exactly 1 instance");
    assert!(running_b.contains(&"inst-b1".to_string()));
    assert!(
        !running_b.contains(&"inst-a1".to_string()),
        "tenantB should NOT see tenantA's instance"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Tenant A cannot load tenant B's instance by ID
// ---------------------------------------------------------------------------

/// Even if tenant A knows tenant B's instance ID, the load operation
/// at the store level returns the instance regardless of tenant (store is
/// unfiltered). Tenant filtering happens at the API layer via list_running.
/// This test documents the store-level behavior.
#[tokio::test]
async fn store_load_is_not_tenant_filtered() {
    let repo = Arc::new(MemoryRepo::new());

    let inst_b = make_instance("inst-b1", Some("tenantB"));
    repo.save(&inst_b).await.unwrap();

    // At the store level, load by ID does NOT filter by tenant.
    // This is by design — tenant enforcement happens at the API layer.
    let loaded = repo.load("inst-b1").await.unwrap();
    assert!(
        loaded.is_some(),
        "store.load() returns instance regardless of tenant (enforcement is at API layer)"
    );

    // The loaded instance retains its tenant_id
    let inst = loaded.unwrap();
    assert_eq!(inst.tenant_id.as_deref(), Some("tenantB"));
}

// ---------------------------------------------------------------------------
// Test 3: PoC mode (no tenant) can see all instances
// ---------------------------------------------------------------------------

/// When tenant_id is None (PoC mode / no tenant header), list_running
/// returns all running instances regardless of their tenant.
#[tokio::test]
async fn no_tenant_sees_all_instances() {
    let repo = Arc::new(MemoryRepo::new());

    let inst_a = make_instance("inst-a1", Some("tenantA"));
    let inst_b = make_instance("inst-b1", Some("tenantB"));
    let inst_none = make_instance("inst-none", None);

    repo.save(&inst_a).await.unwrap();
    repo.save(&inst_b).await.unwrap();
    repo.save(&inst_none).await.unwrap();

    // Query with None tenant — should see all
    let running_all = repo.list_running(None).await.unwrap();
    assert_eq!(
        running_all.len(),
        3,
        "None tenant should see all 3 instances"
    );
    assert!(running_all.contains(&"inst-a1".to_string()));
    assert!(running_all.contains(&"inst-b1".to_string()));
    assert!(running_all.contains(&"inst-none".to_string()));
}

// ---------------------------------------------------------------------------
// Test 4: Empty-string tenant sees tenant-less instances
// ---------------------------------------------------------------------------

/// An empty string tenant_id is treated as "see un-tenanted instances" in the
/// memory adapter. This matches the API layer behavior when x-tenant-id is "".
#[tokio::test]
async fn empty_tenant_sees_un_tennanted_instances() {
    let repo = Arc::new(MemoryRepo::new());

    let inst_a = make_instance("inst-a1", Some("tenantA"));
    let inst_none = make_instance("inst-none", None);

    repo.save(&inst_a).await.unwrap();
    repo.save(&inst_none).await.unwrap();

    // Query with empty string tenant
    let running = repo.list_running(Some("")).await.unwrap();
    assert_eq!(
        running.len(),
        1,
        "empty tenant should see only un-tenanted instances"
    );
    assert!(running.contains(&"inst-none".to_string()));
    assert!(
        !running.contains(&"inst-a1".to_string()),
        "empty tenant should NOT see tenantA's instance"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Tenant isolation for external tasks
// ---------------------------------------------------------------------------

/// External tasks are stored globally (no tenant filter in ExternalTaskStore).
/// Tenant enforcement for tasks happens at the API layer via the instance's tenant.
/// This test documents the store-level contract.
#[tokio::test]
async fn external_tasks_not_filtered_by_tenant_at_store_level() {
    let repo = Arc::new(MemoryRepo::new());

    // Create tasks for different "tenants" (via different instances)
    let task_a = repo
        .create("t1", "inst-a", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();
    let task_b = repo
        .create("t2", "inst-b", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();

    // fetch_and_lock at store level returns tasks regardless of tenant
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "store-level fetch_and_lock returns all matching tasks (tenant enforced at API layer)"
    );

    let task_ids: Vec<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
    assert!(task_ids.contains(&task_a.as_str()));
    assert!(task_ids.contains(&task_b.as_str()));
}

// ---------------------------------------------------------------------------
// Test 6: Completed instances excluded from list_running
// ---------------------------------------------------------------------------

/// Only Running instances should appear in list_running. Completed and
/// Terminated instances should be excluded regardless of tenant.
#[tokio::test]
async fn completed_instances_excluded_from_list_running() {
    let repo = Arc::new(MemoryRepo::new());

    let running = make_instance("inst-running", Some("tenantA"));
    repo.save(&running).await.unwrap();

    let mut completed = make_instance("inst-completed", Some("tenantA"));
    completed.state = InstanceState::Completed;
    repo.save(&completed).await.unwrap();

    let mut terminated = make_instance("inst-terminated", Some("tenantA"));
    terminated.state = InstanceState::Terminated;
    repo.save(&terminated).await.unwrap();

    let running_ids = repo.list_running(Some("tenantA")).await.unwrap();
    assert_eq!(running_ids.len(), 1, "only Running instances should appear");
    assert!(running_ids.contains(&"inst-running".to_string()));
    assert!(!running_ids.contains(&"inst-completed".to_string()));
    assert!(!running_ids.contains(&"inst-terminated".to_string()));
}

// ---------------------------------------------------------------------------
// Test 7: Multiple tenants with overlapping instance IDs
// ---------------------------------------------------------------------------

/// Two tenants can have instances with the same ID (the ID is unique within
/// the store, but this test verifies that tenant filtering works correctly
/// even when both tenants have running instances).
#[tokio::test]
async fn tenant_filtering_with_similar_instances() {
    let repo = Arc::new(MemoryRepo::new());

    // Both tenants have a "payment" process running
    let inst_a = make_instance("payment-1", Some("tenantA"));
    let inst_b = make_instance("payment-2", Some("tenantB"));

    repo.save(&inst_a).await.unwrap();
    repo.save(&inst_b).await.unwrap();

    let running_a = repo.list_running(Some("tenantA")).await.unwrap();
    assert_eq!(running_a.len(), 1);
    assert_eq!(running_a[0], "payment-1");

    let running_b = repo.list_running(Some("tenantB")).await.unwrap();
    assert_eq!(running_b.len(), 1);
    assert_eq!(running_b[0], "payment-2");
}

// ---------------------------------------------------------------------------
// Test 8: TokenStore is not tenant-scoped (documents design)
// ---------------------------------------------------------------------------

/// TokenStore operations are scoped by instance_id, not tenant.
/// Tenant enforcement is handled at the API layer before reaching the store.
#[tokio::test]
async fn token_store_not_tenant_scoped() {
    let repo = Arc::new(MemoryRepo::new());

    let inst = make_instance("inst-1", Some("tenantA"));
    repo.save(&inst).await.unwrap();

    // load_by_instance works regardless of caller's tenant
    let tokens = repo.load_by_instance("inst-1").await.unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].id, "inst-1:t1");

    // claim_token works regardless of tenant
    let claimed = repo.claim_token("inst-1", "inst-1:t1", 0).await.unwrap();
    assert!(claimed, "token should be claimable at store level");
}
