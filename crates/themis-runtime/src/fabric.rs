//! L1 substrate orchestration: bring up / tear down the host-side network
//! plumbing a lab needs before any VMs start.
//!
//! Composes [`crate::host`] primitives into two operations:
//!
//! - [`bring_up`] creates the management bridge, data bridge, every per-link
//!   bridge, enables IP forwarding, and adds a MASQUERADE rule if the lab's
//!   `wan_interface` is set.
//! - [`tear_down`] reverses all of the above (best-effort — missing bridges
//!   and rules are not errors).
//!
//! The daemon calls `bring_up` in the `provisioning` → `running` transition
//! and `tear_down` in `destroying` → `destroyed`.

use std::collections::BTreeSet;

use themis_core::{Result, Topology};
use tracing::{debug, instrument, warn};

use crate::host;

/// Bring up the host-side L1 substrate for the given topology.
///
/// Order:
///   1. Enable IPv4 forwarding on the host.
///   2. Create the management bridge (`topology.management.bridge`).
///   3. Create the data bridge (`topology.management.data_bridge`).
///   4. Create every per-link bridge referenced by `topology.links`.
///   5. Bring all created bridges up.
///   6. If `topology.wan_interface` is `Some`, add MASQUERADE for the data
///      subnet out the WAN interface.
///
/// Idempotent against already-existing bridges: if a bridge exists, creation
/// is skipped. Any other error is returned.
#[instrument(skip(topology), fields(lab = %topology.name))]
pub async fn bring_up(topology: &Topology) -> Result<()> {
    host::enable_ip_forward().await?;

    let mgmt = &topology.management.bridge;
    let data = &topology.management.data_bridge;

    create_if_absent(mgmt).await?;
    create_if_absent(data).await?;

    // Deduplicate: multiple Links may share a bridge name in some
    // topologies (they shouldn't in ours, but be defensive).
    let per_link: BTreeSet<&str> = topology.links.iter().map(|l| l.bridge.as_str()).collect();
    for br in &per_link {
        create_if_absent(br).await?;
    }

    host::set_link_up(mgmt).await?;
    host::set_link_up(data).await?;
    for br in &per_link {
        host::set_link_up(br).await?;
    }

    if let Some(wan) = topology.wan_interface.as_deref() {
        let data_cidr = topology.management.data_cidr.to_string();
        debug!(wan, cidr = %data_cidr, "installing MASQUERADE");
        host::add_masquerade(wan, &data_cidr).await?;
    }

    Ok(())
}

/// Tear down everything `bring_up` created for the given topology.
///
/// Best-effort — missing bridges or MASQUERADE rules are logged at WARN and
/// do not cause an error. Use this on lab `destroy` to leave the host clean.
#[instrument(skip(topology), fields(lab = %topology.name))]
pub async fn tear_down(topology: &Topology) -> Result<()> {
    // 1. Remove MASQUERADE.
    if let Some(wan) = topology.wan_interface.as_deref() {
        let data_cidr = topology.management.data_cidr.to_string();
        if let Err(e) = host::remove_masquerade(wan, &data_cidr).await {
            warn!(error = ?e, "remove MASQUERADE failed — probably already gone");
        }
    }

    // 2. Delete per-link bridges.
    let per_link: BTreeSet<&str> = topology.links.iter().map(|l| l.bridge.as_str()).collect();
    for br in &per_link {
        delete_if_present(br).await;
    }

    // 3. Delete management + data bridges.
    delete_if_present(&topology.management.data_bridge).await;
    delete_if_present(&topology.management.bridge).await;

    Ok(())
}

/// Count the set of L1 resources a topology needs. Useful for preflight
/// display and diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct L1Inventory {
    pub management_bridges: u32,
    pub per_link_bridges: u32,
    pub masquerade_rules: u32,
}

pub fn l1_inventory(topology: &Topology) -> L1Inventory {
    let per_link: BTreeSet<&str> = topology.links.iter().map(|l| l.bridge.as_str()).collect();
    L1Inventory {
        management_bridges: 2, // mgmt + data
        per_link_bridges: per_link.len() as u32,
        masquerade_rules: if topology.wan_interface.is_some() { 1 } else { 0 },
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn create_if_absent(name: &str) -> Result<()> {
    if host::bridge_exists(name).await? {
        debug!(bridge = name, "bridge already exists; skip create");
        return Ok(());
    }
    host::create_bridge(name).await
}

async fn delete_if_present(name: &str) {
    match host::bridge_exists(name).await {
        Ok(true) => {
            if let Err(e) = host::delete_bridge(name).await {
                warn!(bridge = name, error = ?e, "delete_bridge failed");
            }
        }
        Ok(false) => debug!(bridge = name, "bridge already gone"),
        Err(e) => warn!(bridge = name, error = ?e, "bridge_exists check failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use themis_core::{Addressing, Link, Management, Topology};

    fn topology_with(wan: Option<&str>, link_bridges: Vec<&str>) -> Topology {
        Topology {
            name: "lab".into(),
            template: "clos-3tier".into(),
            platform: "frr-fedora".into(),
            wan_interface: wan.map(String::from),
            nodes: HashMap::new(),
            links: link_bridges
                .into_iter()
                .enumerate()
                .map(|(i, br)| Link {
                    bridge: br.into(),
                    a: format!("a{i}"),
                    b: format!("b{i}"),
                    a_ip: None,
                    b_ip: None,
                    subnet: None,
                    a_ifname: "eth0".into(),
                    b_ifname: "eth0".into(),
                    a_mac: "aa:bb:cc:dd:ee:ff".into(),
                    b_mac: "aa:bb:cc:dd:ee:ff".into(),
                    tier: "x".into(),
                })
                .collect(),
            management: Management {
                cidr: "192.168.0.0/24".parse().unwrap(),
                gateway: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
                bridge: "br-mgmt-lab".into(),
                data_cidr: "10.100.0.0/30".parse().unwrap(),
                data_gateway: IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1)),
                data_bridge: "br-data-lab".into(),
                dns_domain: "themis.local".into(),
            },
            addressing: Addressing {
                loopback_cidr: "10.0.0.0/16".parse().unwrap(),
                fabric_p2p_cidr: "172.16.0.0/16".parse().unwrap(),
            },
        }
    }

    #[test]
    fn l1_inventory_counts_bridges() {
        let t = topology_with(None, vec!["br-lab-000", "br-lab-001", "br-lab-002"]);
        let inv = l1_inventory(&t);
        assert_eq!(inv.management_bridges, 2);
        assert_eq!(inv.per_link_bridges, 3);
        assert_eq!(inv.masquerade_rules, 0);
    }

    #[test]
    fn l1_inventory_sees_wan() {
        let t = topology_with(Some("eth0"), vec!["br-lab-000"]);
        let inv = l1_inventory(&t);
        assert_eq!(inv.masquerade_rules, 1);
        assert_eq!(inv.per_link_bridges, 1);
    }

    #[test]
    fn l1_inventory_deduplicates_shared_bridges() {
        // Two links on the same bridge (wouldn't happen in practice, but
        // l1_inventory should still be conservative).
        let t = topology_with(None, vec!["br-lab-000", "br-lab-000", "br-lab-001"]);
        let inv = l1_inventory(&t);
        assert_eq!(inv.per_link_bridges, 2);
    }
}
