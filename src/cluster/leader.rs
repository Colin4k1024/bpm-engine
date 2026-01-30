//! v3: Leader election for outbox_publisher and timer_poller.
//! Uses LeaderLeaseRepo (DB-backed lease) to ensure only one node runs each role.

use crate::persistence::LeaderLeaseRepo;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Role names for single-point tasks (plan v3 §4).
pub const ROLE_OUTBOX_PUBLISHER: &str = "outbox_publisher";
pub const ROLE_TIMER_POLLER: &str = "timer_poller";

/// Leader election: try to acquire or renew lease for a role.
/// Call try_acquire once; if true, call renew periodically while holding the lease.
pub struct LeaderElection {
    lease_repo: Arc<dyn LeaderLeaseRepo + Send + Sync>,
    role: String,
    worker_id: String,
    ttl_secs: u64,
}

impl LeaderElection {
    pub fn new(
        lease_repo: Arc<dyn LeaderLeaseRepo + Send + Sync>,
        role: &str,
        worker_id: &str,
        ttl_secs: u64,
    ) -> Self {
        LeaderElection {
            lease_repo,
            role: role.to_string(),
            worker_id: worker_id.to_string(),
            ttl_secs,
        }
    }

    /// Try to become leader for this role. Returns true iff this worker holds the lease.
    pub fn try_acquire(&self) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let ok = self.lease_repo.try_acquire(&self.role, &self.worker_id, self.ttl_secs)?;
        if ok {
            debug!(role = %self.role, worker_id = %self.worker_id, "leader acquired");
        }
        Ok(ok)
    }

    /// Renew the lease. Call periodically (e.g. every ttl_secs/2). Returns true iff renewal succeeded.
    pub fn renew(&self) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let ok = self.lease_repo.renew(&self.role, &self.worker_id, self.ttl_secs)?;
        if !ok {
            warn!(role = %self.role, worker_id = %self.worker_id, "leader renewal failed");
        }
        Ok(ok)
    }

    /// Suggested interval for renewal (half of TTL).
    pub fn renew_interval(&self) -> Duration {
        Duration::from_secs(self.ttl_secs / 2)
    }
}
