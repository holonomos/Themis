//! Lab deploy flow.
//!
//! The full deploy sequence:
//!  1. Acquire per-lab mutex.
//!  2. Load lab from DB; validate state (Defined or Failed).
//!  3. Transition → Provisioning.
//!  4. Parse Themisfile → ThemisfileDoc.
//!  5. Expand topology via template.
//!  6. Upsert node rows (state = Provisioning).
//!  7. Ensure SSH keypair for the lab.
//!  8. Verify golden image exists.
//!  9. Build inventory (domain XML + cloud-init content).
//! 10. bring_up host fabric (bridges, forwarding, NAT).
//! 11. For each Seed node: build_seed_iso.
//! 12. For each node: clone_golden_image.
//! 13. For each node: define_domain.
//! 14. For each node: start_domain.
//! 15. Wait for SSH reachability (parallelised).
//! 16. Update node rows → Running.
//! 17. Render NOS configs.
//! 18. Push configs via SSH + reload.
//! 19. Transition lab → Running.
//!
//! On any error: transition lab → Failed, emit ERROR event.

use std::time::Duration;

use tokio::task::JoinSet;
use tracing::{error, info, instrument, warn};

use themis_compiler::{expander, inventory as inv_builder, renderer};
use themis_runtime::{fabric, iso, keys, libvirt, ssh};

use crate::daemon_deps::DaemonDeps;
use crate::events::EventKind;
use crate::lifecycle::LabState;
use crate::state;

const SSH_TIMEOUT_PER_NODE: Duration = Duration::from_secs(120);
const SSH_USER: &str = "themis";

/// Run the full deploy sequence for `lab_name`.
///
/// Acquires the per-lab mutex for the entire operation.
/// Returns Ok(()) on success; the lab state is set to Running.
/// On error the lab state is set to Failed; the error is returned.
#[instrument(skip(deps), fields(lab = lab_name))]
pub async fn run(lab_name: &str, deps: &DaemonDeps) -> Result<(), tonic::Status> {
    // 1. Acquire per-lab mutex.
    let _guard = deps.locks.lock_lab(lab_name).await;

    let result = deploy_inner(lab_name, deps).await;

    if let Err(ref e) = result {
        error!(lab = lab_name, error = %e, "deploy failed; marking lab Failed");
        let msg = e.message().to_string();
        // Transition to Failed (best-effort — ignore if it fails).
        if let Ok(Some(row)) = state::get_lab(&deps.db, lab_name).await {
            let current = row.state;
            if current.can_transition_to(LabState::Failed) {
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

async fn deploy_inner(lab_name: &str, deps: &DaemonDeps) -> Result<(), tonic::Status> {
    // 2. Load lab row; validate state.
    let row = state::get_lab(&deps.db, lab_name)
        .await
        .map_err(|e| tonic::Status::internal(format!("db get_lab: {e}")))?
        .ok_or_else(|| tonic::Status::not_found(format!("lab '{lab_name}' not found")))?;

    if !matches!(row.state, LabState::Defined | LabState::Failed) {
        return Err(tonic::Status::failed_precondition(format!(
            "lab '{lab_name}' is in state {:?}; must be Defined or Failed to deploy",
            row.state
        )));
    }

    // 3. Transition → Provisioning.
    crate::lifecycle::transition(
        &deps.db,
        &deps.hub,
        lab_name,
        row.state,
        LabState::Provisioning,
        "deploy started",
    )
    .await?;

    // 4. Parse Themisfile.
    let doc = themis_compiler::loader::parse_themisfile(&row.themisfile)
        .map_err(|e| tonic::Status::invalid_argument(format!("themisfile parse error: {e}")))?;

    // 5. Expand topology.
    let wan_interface = doc.wan_interface.as_deref().unwrap_or("");
    let topology = expander::expand_with_builtins(
        &doc.name,
        &doc.template,
        &doc.platform,
        wan_interface,
        &doc.parameters,
    )
    .map_err(|e| tonic::Status::internal(format!("template expansion: {e}")))?;

    info!(
        lab = lab_name,
        nodes = topology.nodes.len(),
        "topology expanded"
    );

    // 6. Upsert node rows.
    for node in topology.nodes.values() {
        state::upsert_node(
            &deps.db,
            lab_name,
            &node.name,
            node.role.as_str(),
            "Provisioning",
            Some(&node.mgmt_ip.to_string()),
        )
        .await
        .map_err(|e| tonic::Status::internal(format!("db upsert_node: {e}")))?;
    }

    // 7. Ensure SSH keypair.
    let keypair = keys::ensure_lab_keypair(&deps.paths.keys_dir, lab_name)
        .await
        .map_err(|e| tonic::Status::internal(format!("ensure_lab_keypair: {e}")))?;

    // 8. Verify golden image exists.
    let golden_image = deps.paths.golden_image(&row.platform);
    if !golden_image.exists() {
        return Err(tonic::Status::failed_precondition(format!(
            "golden image not found at '{}'. \
             Place the '{platform}' golden image at $XDG_DATA_HOME/themisd/golden-images/{platform}.qcow2 \
             (typically ~/.local/share/themisd/golden-images/{platform}.qcow2). \
             Build it with golden-bootstrap/bake.sh.",
            golden_image.display(),
            platform = row.platform,
        )));
    }

    // 9. Build inventory.
    let seed_iso_dir = deps.paths.lab_seed_dir(lab_name);
    tokio::fs::create_dir_all(&seed_iso_dir)
        .await
        .map_err(|e| tonic::Status::internal(format!("create seed dir: {e}")))?;

    let disk_dir = deps.paths.lab_disk_dir(lab_name);
    tokio::fs::create_dir_all(&disk_dir)
        .await
        .map_err(|e| tonic::Status::internal(format!("create disk dir: {e}")))?;

    // Inventory records the exact per-node disk and seed-iso paths that are
    // embedded in the domain XML. We must write to the SAME paths below so
    // the XML matches reality.
    let inventory = inv_builder::build_inventory(
        &topology,
        &disk_dir,
        &seed_iso_dir,
        Some(&keypair.public_key),
    )
    .map_err(|e| tonic::Status::internal(format!("build_inventory: {e}")))?;

    // 10. Bring up host fabric.
    fabric::bring_up(&topology)
        .await
        .map_err(|e| tonic::Status::internal(format!("fabric bring_up: {e}")))?;

    info!(lab = lab_name, "host fabric brought up");

    // 11. Build seed ISOs for Seed-mode nodes, at the exact path recorded in
    // `artifact.seed_iso_path` (which the domain XML already references).
    for artifact in &inventory.artifacts {
        if let Some(iso_path) = &artifact.seed_iso_path {
            iso::build_seed_iso(iso_path, &artifact.cloud_init)
                .await
                .map_err(|e| {
                    tonic::Status::internal(format!(
                        "build_seed_iso for '{}': {e}",
                        artifact.node_name
                    ))
                })?;
            info!(lab = lab_name, node = %artifact.node_name, "seed ISO built");
        }
    }

    // 12. Clone golden image to the exact per-node path the XML references.
    for artifact in &inventory.artifacts {
        libvirt::clone_golden_image(&golden_image, &artifact.disk_path)
            .await
            .map_err(|e| {
                tonic::Status::internal(format!(
                    "clone_golden_image for '{}': {e}",
                    artifact.node_name
                ))
            })?;
        info!(lab = lab_name, node = %artifact.node_name, "disk cloned");
    }

    // 13. Define domains.
    for artifact in &inventory.artifacts {
        let domain_name = libvirt::domain_name(lab_name, &artifact.node_name);
        libvirt::define_domain(&domain_name, &artifact.domain_xml)
            .await
            .map_err(|e| {
                tonic::Status::internal(format!("define_domain '{}': {e}", domain_name))
            })?;
        info!(lab = lab_name, node = %artifact.node_name, "domain defined");
    }

    // 14. Start domains.
    for artifact in &inventory.artifacts {
        let domain_name = libvirt::domain_name(lab_name, &artifact.node_name);
        libvirt::start_domain(&domain_name)
            .await
            .map_err(|e| {
                tonic::Status::internal(format!("start_domain '{}': {e}", domain_name))
            })?;
        info!(lab = lab_name, node = %artifact.node_name, "domain started");
    }

    // 15. Wait for SSH reachability (parallel).
    let mut join_set: JoinSet<(String, Result<(), String>)> = JoinSet::new();

    for node in topology.nodes.values() {
        let host = node.mgmt_ip.to_string();
        let node_name = node.name.clone();
        let key_path = keypair.private_key_path.clone();
        let lab = lab_name.to_string();

        join_set.spawn(async move {
            let ssh_config = ssh::SshConfig {
                host: host.clone(),
                port: 22,
                user: SSH_USER.to_string(),
                auth: ssh::SshAuth::KeyFile(key_path),
                connect_timeout: Duration::from_secs(10),
            };
            info!(lab = %lab, node = %node_name, host = %host, "waiting for SSH");
            let result = ssh::wait_for_reachable(&ssh_config, SSH_TIMEOUT_PER_NODE).await;
            (node_name, result.map_err(|e| e.to_string()))
        });
    }

    let mut ssh_errors: Vec<String> = Vec::new();
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok((node_name, Ok(()))) => {
                info!(lab = lab_name, node = %node_name, "SSH reachable");
            }
            Ok((node_name, Err(e))) => {
                warn!(lab = lab_name, node = %node_name, error = %e, "SSH wait failed");
                ssh_errors.push(format!("node '{node_name}': {e}"));
            }
            Err(e) => {
                warn!(lab = lab_name, "JoinSet error: {e}");
                ssh_errors.push(format!("join error: {e}"));
            }
        }
    }

    if !ssh_errors.is_empty() {
        return Err(tonic::Status::unavailable(format!(
            "SSH reachability failed for some nodes: {}",
            ssh_errors.join("; ")
        )));
    }

    // 16. Update node rows → Running.
    for node in topology.nodes.values() {
        state::update_node_state(&deps.db, lab_name, &node.name, "Running")
            .await
            .map_err(|e| tonic::Status::internal(format!("db update_node_state: {e}")))?;
        deps.hub
            .publish(
                lab_name,
                EventKind::NodeState,
                &node.name,
                "Running",
                vec![],
            )
            .await;
    }

    // 17. Render NOS configs.
    let rendered = renderer::render_with_builtin_platforms(&topology, &row.platform)
        .map_err(|e| tonic::Status::internal(format!("render_with_builtin_platforms: {e}")))?;

    // 18. Push configs via SSH + reload.
    for node_config in &rendered.nodes {
        let node_name = &node_config.node_name;

        // Look up the node's management IP.
        let node = match topology.nodes.get(node_name) {
            Some(n) => n,
            None => {
                warn!(lab = lab_name, node = %node_name, "rendered config for unknown node; skipping");
                continue;
            }
        };

        let host = node.mgmt_ip.to_string();
        let ssh_config = ssh::SshConfig {
            host: host.clone(),
            port: 22,
            user: SSH_USER.to_string(),
            auth: ssh::SshAuth::KeyFile(keypair.private_key_path.clone()),
            connect_timeout: Duration::from_secs(30),
        };

        let push_result = push_configs_to_node(&ssh_config, node_config, &row.platform).await;
        match push_result {
            Ok(()) => {
                info!(lab = lab_name, node = %node_name, "configs pushed");
            }
            Err(e) => {
                // Mark this specific node Failed but keep deploying others.
                warn!(
                    lab = lab_name,
                    node = %node_name,
                    error = %e,
                    "config push failed for node; marking node Failed"
                );
                let _ = state::update_node_state(
                    &deps.db, lab_name, node_name, "Failed"
                ).await;
                deps.hub
                    .publish(lab_name, EventKind::Error, node_name, &e, vec![])
                    .await;
                // Not fatal for the lab overall — continue.
            }
        }
    }

    // 19. Transition lab → Running.
    crate::lifecycle::transition(
        &deps.db,
        &deps.hub,
        lab_name,
        LabState::Provisioning,
        LabState::Running,
        "deploy complete",
    )
    .await?;

    info!(lab = lab_name, "lab is Running");
    Ok(())
}

/// Push all config files to a node via SSH and run the platform reload command.
async fn push_configs_to_node(
    ssh_config: &ssh::SshConfig,
    node_config: &renderer::NodeConfig,
    platform_name: &str,
) -> Result<(), String> {
    let mut client = ssh::SshClient::connect(ssh_config)
        .await
        .map_err(|e| e.to_string())?;

    // Write each config file.
    for (path, content) in &node_config.files {
        client
            .write_file(path, content.as_bytes())
            .await
            .map_err(|e| format!("write_file {}: {e}", path.display()))?;
    }

    // Run the platform reload command.
    let reload_cmd = find_reload_command(platform_name);
    let result = client
        .exec(&reload_cmd)
        .await
        .map_err(|e| format!("exec reload: {e}"))?;

    if result.exit_code != 0 {
        return Err(format!(
            "reload command '{}' exited with {}: {}",
            reload_cmd,
            result.exit_code,
            result.stderr.trim()
        ));
    }

    client.disconnect().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Look up the reload command for a platform by name.
fn find_reload_command(platform_name: &str) -> String {
    let platforms = themis_platforms::builtin();
    platforms
        .iter()
        .find(|p| p.name() == platform_name)
        .map(|p| p.reload_command().to_string())
        .unwrap_or_else(|| "true".to_string())
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

