//! Startup reconciliation — SQLite state vs `virsh list`.
//!
//! Runs once at daemon startup, before the gRPC server begins accepting
//! connections. Reconciliation ensures the in-database state reflects what
//! is actually running in libvirt.
//!
//! Strategy:
//!   - Any lab in Provisioning or Destroying: the daemon crashed mid-op.
//!     Mark it Failed.
//!   - Any lab in Running: verify each expected node domain exists and is
//!     running in libvirt. If any are missing or not running, mark the lab
//!     Failed.
//!   - Paused labs: mark Failed (we can't guarantee the pause completed).
//!   - All other states (Defined, Destroyed, Failed): left as-is.

use tracing::{info, warn};

use themis_runtime::libvirt;

use crate::events::{EventHub, EventKind};
use crate::lifecycle::LabState;
use crate::state::{self, DbPool};

/// Run startup reconciliation.
///
/// Must be called after the DB is open and before the gRPC server starts.
pub async fn reconcile_all(db: &DbPool, hub: &EventHub) {
    info!("starting reconciliation");

    let labs = match state::list_labs(db).await {
        Ok(l) => l,
        Err(e) => {
            warn!("reconciliation: could not list labs: {e}");
            return;
        }
    };

    // Fetch the live libvirt domain list once, to avoid repeated shell-outs.
    let live_domains = match libvirt::list_domains().await {
        Ok(d) => d,
        Err(e) => {
            warn!("reconciliation: virsh list failed: {e}; marking all Running/Paused labs Failed");
            // Can't verify — fail all in-flight labs defensively.
            for lab in &labs {
                if matches!(
                    lab.state,
                    LabState::Provisioning | LabState::Destroying | LabState::Running | LabState::Paused
                ) {
                    mark_failed(db, hub, &lab.name, "reconciliation: virsh unavailable").await;
                }
            }
            return;
        }
    };

    for lab in &labs {
        match lab.state {
            // Crashed mid-op — cannot know if it completed.
            LabState::Provisioning | LabState::Destroying => {
                warn!(lab = %lab.name, state = ?lab.state, "crashed mid-op; marking Failed");
                mark_failed(
                    db,
                    hub,
                    &lab.name,
                    &format!("daemon restarted while lab was in {:?}", lab.state),
                )
                .await;
            }

            // Paused — we can't safely resume, mark Failed.
            LabState::Paused => {
                warn!(lab = %lab.name, "lab was Paused at restart; marking Failed");
                mark_failed(
                    db,
                    hub,
                    &lab.name,
                    "daemon restarted while lab was Paused",
                )
                .await;
            }

            // Running — verify all expected node domains are alive.
            LabState::Running => {
                reconcile_running_lab(db, hub, &lab.name, &live_domains).await;
            }

            // These don't need reconciliation.
            LabState::Defined | LabState::Destroyed | LabState::Failed => {}
        }
    }

    info!("reconciliation complete");
}

/// Verify that every node domain for a Running lab is present and running.
/// If any are missing or not running, mark the lab Failed.
async fn reconcile_running_lab(
    db: &DbPool,
    hub: &EventHub,
    lab_name: &str,
    live_domains: &[libvirt::DomainSummary],
) {
    let nodes = match state::get_nodes(db, lab_name).await {
        Ok(n) => n,
        Err(e) => {
            warn!(lab = lab_name, "could not load nodes: {e}");
            mark_failed(db, hub, lab_name, "reconciliation: could not load nodes").await;
            return;
        }
    };

    for node in &nodes {
        let domain_name = libvirt::domain_name(lab_name, &node.name);
        let found = live_domains
            .iter()
            .find(|d| d.name == domain_name);

        match found {
            Some(d) if d.state == libvirt::DomainState::Running => {
                // OK — domain is running as expected.
            }
            Some(d) => {
                warn!(
                    lab = lab_name,
                    node = %node.name,
                    domain_state = %d.state,
                    "domain not in Running state at reconciliation"
                );
                mark_failed(
                    db,
                    hub,
                    lab_name,
                    &format!(
                        "node '{}' domain '{}' is '{}', expected running",
                        node.name, domain_name, d.state
                    ),
                )
                .await;
                return;
            }
            None => {
                warn!(
                    lab = lab_name,
                    node = %node.name,
                    "domain missing from libvirt at reconciliation"
                );
                mark_failed(
                    db,
                    hub,
                    lab_name,
                    &format!(
                        "node '{}' domain '{}' not found in libvirt",
                        node.name, domain_name
                    ),
                )
                .await;
                return;
            }
        }
    }

    info!(lab = lab_name, "reconciliation confirmed running");
}

/// Mark a lab as Failed in both the DB and the event hub.
async fn mark_failed(db: &DbPool, hub: &EventHub, lab_name: &str, reason: &str) {
    if let Err(e) = state::update_lab_state(db, lab_name, LabState::Failed).await {
        warn!(lab = lab_name, "mark_failed: db update failed: {e}");
    }

    let message = format!("reconciliation: {reason}");
    hub.publish(lab_name, EventKind::Error, lab_name, &message, vec![])
        .await;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;
    let _ = state::insert_event(db, lab_name, "ERROR", lab_name, &message, None, ts).await;
}
