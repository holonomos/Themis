//! Lab destroy flow.
//!
//! Sequence:
//!  1. Acquire per-lab mutex.
//!  2. Load lab; validate state (Running | Paused | Failed | Destroying).
//!  3. Transition → Destroying.
//!  4. For each node: destroy_domain (ignore "not running").
//!  5. For each node: undefine_domain (ignore "not defined").
//!  6. Delete per-node qcow2 disk files from cache.
//!  7. Delete seed ISO files from cache.
//!  8. Re-expand topology (from themisfile) for fabric::tear_down.
//!  9. tear_down host fabric (bridges, forwarding, NAT).
//! 10. Transition → Destroyed.
//!
//! On error: transition → Failed, emit ERROR event.

use tracing::{info, instrument, warn};

use themis_compiler::expander;
use themis_runtime::{fabric, libvirt};

use crate::daemon_deps::DaemonDeps;
use crate::events::EventKind;
use crate::lifecycle::LabState;
use crate::state;

/// Run the full destroy sequence for `lab_name`.
///
/// Acquires the per-lab mutex for the entire operation.
#[instrument(skip(deps), fields(lab = lab_name))]
pub async fn run(lab_name: &str, deps: &DaemonDeps) -> Result<(), tonic::Status> {
    // 1. Acquire per-lab mutex.
    let _guard = deps.locks.lock_lab(lab_name).await;

    let result = destroy_inner(lab_name, deps).await;

    if let Err(ref e) = result {
        let msg = e.message().to_string();
        if let Ok(Some(row)) = state::get_lab(&deps.db, lab_name).await {
            if row.state.can_transition_to(LabState::Failed) {
                let _ = state::update_lab_state(&deps.db, lab_name, LabState::Failed).await;
            }
        }
        let ts = now_ns();
        deps.hub
            .publish(lab_name, EventKind::Error, lab_name, &msg, vec![])
            .await;
        let _ = state::insert_event(&deps.db, lab_name, "ERROR", lab_name, &msg, None, ts).await;
    }

    result
}

async fn destroy_inner(lab_name: &str, deps: &DaemonDeps) -> Result<(), tonic::Status> {
    // 2. Load lab; validate state.
    let row = state::get_lab(&deps.db, lab_name)
        .await
        .map_err(|e| tonic::Status::internal(format!("db get_lab: {e}")))?
        .ok_or_else(|| tonic::Status::not_found(format!("lab '{lab_name}' not found")))?;

    let acceptable = matches!(
        row.state,
        LabState::Running | LabState::Paused | LabState::Failed | LabState::Destroying
    );
    if !acceptable {
        return Err(tonic::Status::failed_precondition(format!(
            "lab '{lab_name}' is in state {:?}; must be Running, Paused, Failed, or Destroying to destroy",
            row.state
        )));
    }

    // 3. Transition → Destroying (idempotent if already Destroying).
    if row.state != LabState::Destroying {
        crate::lifecycle::transition(
            &deps.db,
            &deps.hub,
            lab_name,
            row.state,
            LabState::Destroying,
            "destroy started",
        )
        .await?;
    }

    // Load nodes.
    let nodes = state::get_nodes(&deps.db, lab_name)
        .await
        .map_err(|e| tonic::Status::internal(format!("db get_nodes: {e}")))?;

    // 4. Destroy VM instances (ignore "not running" errors).
    for node in &nodes {
        let domain_name = libvirt::domain_name(lab_name, &node.name);
        match libvirt::destroy_domain(&domain_name).await {
            Ok(()) => info!(lab = lab_name, node = %node.name, "domain destroyed"),
            Err(e) => warn!(
                lab = lab_name,
                node = %node.name,
                "destroy_domain failed (ignoring): {e}"
            ),
        }
    }

    // 5. Undefine domains (ignore "not defined" errors).
    for node in &nodes {
        let domain_name = libvirt::domain_name(lab_name, &node.name);
        match libvirt::undefine_domain(&domain_name).await {
            Ok(()) => info!(lab = lab_name, node = %node.name, "domain undefined"),
            Err(e) => warn!(
                lab = lab_name,
                node = %node.name,
                "undefine_domain failed (ignoring): {e}"
            ),
        }
    }

    // 6. Delete per-node disk files.
    let disk_dir = deps.paths.lab_disk_dir(lab_name);
    for node in &nodes {
        let disk = disk_dir.join(format!("{}.qcow2", node.name));
        if disk.exists() {
            match tokio::fs::remove_file(&disk).await {
                Ok(()) => info!(lab = lab_name, node = %node.name, "disk deleted"),
                Err(e) => warn!(
                    lab = lab_name,
                    node = %node.name,
                    path = %disk.display(),
                    "remove disk failed (ignoring): {e}"
                ),
            }
        }
    }

    // 7. Delete seed ISOs.
    let seed_dir = deps.paths.lab_seed_dir(lab_name);
    for node in &nodes {
        let iso = seed_dir.join(format!("{}.iso", node.name));
        if iso.exists() {
            match tokio::fs::remove_file(&iso).await {
                Ok(()) => info!(lab = lab_name, node = %node.name, "seed ISO deleted"),
                Err(e) => warn!(
                    lab = lab_name,
                    node = %node.name,
                    path = %iso.display(),
                    "remove seed ISO failed (ignoring): {e}"
                ),
            }
        }
    }

    // 8. Re-expand topology for tear_down.
    // This may fail if the Themisfile is somehow corrupt; still proceed and
    // skip tear_down rather than leaving the lab in a Destroying state forever.
    let doc = themis_compiler::loader::parse_themisfile(&row.themisfile);
    match doc {
        Ok(doc) => {
            let wan_interface = doc.wan_interface.as_deref().unwrap_or("");
            match expander::expand_with_builtins(
                &doc.name,
                &doc.template,
                &doc.platform,
                wan_interface,
                &doc.parameters,
            ) {
                Ok(topology) => {
                    // 9. Tear down host fabric (best-effort).
                    if let Err(e) = fabric::tear_down(&topology).await {
                        warn!(lab = lab_name, "fabric tear_down failed (ignoring): {e}");
                    } else {
                        info!(lab = lab_name, "host fabric torn down");
                    }
                }
                Err(e) => {
                    warn!(lab = lab_name, "topology re-expand failed; skipping tear_down: {e}");
                }
            }
        }
        Err(e) => {
            warn!(lab = lab_name, "themisfile parse failed; skipping tear_down: {e}");
        }
    }

    // 10. Transition → Destroyed.
    crate::lifecycle::transition(
        &deps.db,
        &deps.hub,
        lab_name,
        LabState::Destroying,
        LabState::Destroyed,
        "destroy complete",
    )
    .await?;

    info!(lab = lab_name, "lab Destroyed");
    Ok(())
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}
