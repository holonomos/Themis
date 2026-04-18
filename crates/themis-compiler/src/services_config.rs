//! Services-node configuration generators.
//!
//! The services VM runs dnsmasq for DHCP (MAC-pinned reservations, one per
//! DHCP-mode node) and recursive DNS (fabric-local hostnames). This module
//! builds the config file content; the runtime writes it to the services
//! node via cloud-init `write_files` during deployment.

use themis_core::{Bootstrap, Role, Topology};

/// Return the dnsmasq configuration string for the given topology's services
/// node. One `dhcp-host=<mac>,<ip>,<hostname>` entry per DHCP-mode node.
///
/// Empty topologies (no DHCP nodes) produce a valid, minimal config that
/// just sets the DHCP range and DNS domain.
pub fn generate_dnsmasq_config(topology: &Topology) -> String {
    let mgmt_cidr = topology.management.cidr.to_string();
    let mgmt_gateway = topology.management.gateway.to_string();
    let dns_domain = &topology.management.dns_domain;

    // Pick a conservative DHCP range inside the management network.
    // We don't need this for MAC-pinned DHCP hosts, but dnsmasq requires a
    // `dhcp-range` line to enable DHCP at all.
    let dhcp_range = dhcp_range_for(&mgmt_cidr);

    let mut cfg = String::new();
    cfg.push_str("# Themis — dnsmasq configuration\n");
    cfg.push_str("# Generated from topology — DO NOT HAND-EDIT\n");
    cfg.push_str("\n");
    cfg.push_str("# Act as DHCP and DNS authority for the management network.\n");
    cfg.push_str(&format!("interface=eth-mgmt\n"));
    cfg.push_str(&format!("bind-interfaces\n"));
    cfg.push_str(&format!("domain={}\n", dns_domain));
    cfg.push_str(&format!("expand-hosts\n"));
    cfg.push_str("\n");

    cfg.push_str("# DHCP range (required for dnsmasq DHCP; MAC-pinned hosts\n");
    cfg.push_str("# below override and bypass the pool).\n");
    cfg.push_str(&format!("dhcp-range={dhcp_range},static\n"));
    cfg.push_str(&format!("dhcp-option=3,{}\n", mgmt_gateway));
    cfg.push_str(&format!(
        "dhcp-option=6,{}\n",
        mgmt_gateway
    ));
    cfg.push_str(&format!("dhcp-option=15,{}\n", dns_domain));
    cfg.push_str("\n");

    // Sort nodes by name for deterministic output.
    let mut dhcp_nodes: Vec<_> = topology
        .nodes
        .values()
        .filter(|n| matches!(n.bootstrap, Bootstrap::Dhcp))
        .collect();
    dhcp_nodes.sort_by(|a, b| a.name.cmp(&b.name));

    if !dhcp_nodes.is_empty() {
        cfg.push_str("# MAC-pinned reservations — one per DHCP-mode node.\n");
        for node in &dhcp_nodes {
            cfg.push_str(&format!(
                "dhcp-host={mac},{ip},{name}\n",
                mac = node.mgmt_mac.to_lowercase(),
                ip = node.mgmt_ip,
                name = node.name,
            ));
        }
        cfg.push_str("\n");
    }

    // Static A records for control-plane (seed) nodes so they resolve even
    // before they lease anything (they don't lease — they are seed-mode).
    let mut seed_nodes: Vec<_> = topology
        .nodes
        .values()
        .filter(|n| matches!(n.bootstrap, Bootstrap::Seed))
        .collect();
    seed_nodes.sort_by(|a, b| a.name.cmp(&b.name));

    if !seed_nodes.is_empty() {
        cfg.push_str("# Static A records for seed-mode (control-plane) nodes.\n");
        for node in &seed_nodes {
            cfg.push_str(&format!(
                "host-record={name},{name}.{dns_domain},{ip}\n",
                name = node.name,
                dns_domain = dns_domain,
                ip = node.mgmt_ip,
            ));
        }
    }

    cfg
}

/// Returns true if the given node is the services node of this topology.
/// A topology may have zero or one services node.
pub fn is_services_node(topology: &Topology, node_name: &str) -> bool {
    topology
        .nodes
        .get(node_name)
        .map(|n| n.role == Role::Services)
        .unwrap_or(false)
}

/// Compute a sensible DHCP range for dnsmasq from a CIDR. We lease a small
/// slice in the upper half of the network so static addresses in the lower
/// half don't collide.
fn dhcp_range_for(cidr: &str) -> String {
    // Crude extraction: works for /24s which is what every builtin template
    // currently uses. Good enough for a single-release product; revisit if a
    // future template uses a wider management network.
    if let Some(prefix) = cidr.split('/').next() {
        let parts: Vec<&str> = prefix.rsplitn(2, '.').collect();
        if parts.len() == 2 {
            let tail = parts[0]; // e.g. "0"
            let head = parts[1]; // e.g. "192.168.0"
            let _ = tail;
            return format!("{head}.200,{head}.249,12h");
        }
    }
    // Fallback — safe default.
    "192.168.0.200,192.168.0.249,12h".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use themis_core::{
        Addressing, Bootstrap, Interface, Management, Node, Role, Topology,
    };

    fn make_node(name: &str, role: Role, ip: &str, mac: &str, bootstrap: Bootstrap) -> Node {
        Node {
            name: name.into(),
            role,
            nos_type: None,
            asn: None,
            loopback: None,
            mgmt_ip: ip.parse().unwrap(),
            mgmt_mac: mac.into(),
            vcpu: 1,
            memory_mb: 256,
            disk_gb: 3,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap,
        }
    }

    fn fabric_with(nodes: Vec<Node>) -> Topology {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.name.clone(), n);
        }
        Topology {
            name: "lab".into(),
            template: "clos-3tier".into(),
            platform: "frr-fedora".into(),
            wan_interface: None,
            nodes: map,
            links: vec![],
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
    fn config_has_domain_and_range() {
        let t = fabric_with(vec![]);
        let c = generate_dnsmasq_config(&t);
        assert!(c.contains("domain=themis.local"));
        assert!(c.contains("dhcp-range=192.168.0.200,192.168.0.249,12h"));
    }

    #[test]
    fn one_dhcp_host_per_dhcp_node() {
        let t = fabric_with(vec![
            make_node("border-1", Role::Border, "192.168.0.10", "AA:BB:CC:DD:EE:01", Bootstrap::Dhcp),
            make_node("spine-1", Role::Spine, "192.168.0.20", "AA:BB:CC:DD:EE:02", Bootstrap::Dhcp),
            make_node("bastion", Role::Bastion, "192.168.0.2", "AA:BB:CC:DD:EE:03", Bootstrap::Seed),
        ]);
        let c = generate_dnsmasq_config(&t);
        assert!(c.contains("dhcp-host=aa:bb:cc:dd:ee:01,192.168.0.10,border-1"));
        assert!(c.contains("dhcp-host=aa:bb:cc:dd:ee:02,192.168.0.20,spine-1"));
        // Seed nodes should NOT appear as dhcp-host lines.
        assert!(!c.contains("dhcp-host=aa:bb:cc:dd:ee:03"));
    }

    #[test]
    fn seed_nodes_get_static_records() {
        let t = fabric_with(vec![
            make_node("bastion", Role::Bastion, "192.168.0.2", "AA:BB:CC:DD:EE:03", Bootstrap::Seed),
            make_node("services", Role::Services, "192.168.0.3", "AA:BB:CC:DD:EE:04", Bootstrap::Seed),
        ]);
        let c = generate_dnsmasq_config(&t);
        assert!(c.contains("host-record=bastion,bastion.themis.local,192.168.0.2"));
        assert!(c.contains("host-record=services,services.themis.local,192.168.0.3"));
    }

    #[test]
    fn is_services_node_detects_role() {
        let services = make_node("services", Role::Services, "192.168.0.3", "00:00:00:00:00:01", Bootstrap::Seed);
        let spine = make_node("spine-1", Role::Spine, "192.168.0.20", "00:00:00:00:00:02", Bootstrap::Dhcp);
        let t = fabric_with(vec![services, spine]);
        assert!(is_services_node(&t, "services"));
        assert!(!is_services_node(&t, "spine-1"));
        assert!(!is_services_node(&t, "nonexistent"));
    }

    // Interface field is unused by this module but kept imported to signal
    // that Node's full surface is available if we need to extend.
    #[allow(dead_code)]
    fn _unused_interface(_: Interface) {}
}
