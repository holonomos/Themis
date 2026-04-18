//! hub-spoke — Hub + N branches, optional branch redundancy.
//!
//! Enterprise multi-site WAN pattern. One or two hub routers act as the
//! organization's core; each branch site has one (or two, for local
//! redundancy) branch routers and a small number of servers.
//!
//! ## Schema
//! | parameter          | type    | default | range  |
//! |--------------------|---------|---------|--------|
//! | hub_count          | integer | 1       | 1–2    |
//! | branch_count       | integer | 4       | 1–16   |
//! | branch_redundant   | boolean | false   | —      |
//! | servers_per_branch | integer | 2       | 1–4    |
//!
//! ## Default node count (hub_count=1, branch_count=4, branch_redundant=false,
//!    servers_per_branch=2):
//!   1 hub + 4 branch-a routers + 8 servers + 2 control-plane = **15 nodes**.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use ipnet::Ipv4Net;

use themis_core::{
    Addressing, BgpNeighbor, Bootstrap, Error, Interface, Link, Management, Node, ParameterDef,
    ParameterSchema, ParameterType, Parameters, Result, Role, Template, Topology,
};

// ──────────────────────────────────────────────────────────────────────────────
// Public struct
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct HubSpoke;

// ──────────────────────────────────────────────────────────────────────────────
// Schema
// ──────────────────────────────────────────────────────────────────────────────

fn build_schema() -> ParameterSchema {
    ParameterSchema::new()
        .with(
            "hub_count",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(1)),
                min: Some(1),
                max: Some(2),
                description: Some(
                    "Number of hub routers (1 = single hub, 2 = redundant pair)".into(),
                ),
            },
        )
        .with(
            "branch_count",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(4)),
                min: Some(1),
                max: Some(16),
                description: Some("Number of branch sites".into()),
            },
        )
        .with(
            "branch_redundant",
            ParameterDef {
                ty: ParameterType::Boolean,
                default: Some(serde_json::json!(false)),
                min: None,
                max: None,
                description: Some(
                    "If true, each branch has two routers for local redundancy".into(),
                ),
            },
        )
        .with(
            "servers_per_branch",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(2)),
                min: Some(1),
                max: Some(4),
                description: Some("Number of server VMs per branch site".into()),
            },
        )
}

use std::sync::OnceLock;
static SCHEMA: OnceLock<ParameterSchema> = OnceLock::new();

fn get_schema() -> &'static ParameterSchema {
    SCHEMA.get_or_init(build_schema)
}

// ──────────────────────────────────────────────────────────────────────────────
// Free helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Build a MAC in the `02:4E:57:<tier>:<peer>:<role_index>` pattern.
///
/// - `02`   — locally administered, unicast.
/// - `4E:57` — "NW" brand bytes.
/// - `tier`  — `Role::tier_code()` of the owning node.
/// - `peer`  — link-global monotone counter (wraps at 255).
/// - `idx`   — 0 for the "a" side, 1 for the "b" side of each link.
fn make_mac(tier: u8, peer: u8, idx: u8) -> String {
    format!("02:4E:57:{tier:02X}:{peer:02X}:{idx:02X}")
}

/// Return the two /30 host addresses (.1, .2) for a subnet.
fn p2p_hosts(subnet: Ipv4Net) -> (Ipv4Addr, Ipv4Addr) {
    let base = u32::from(subnet.network());
    (Ipv4Addr::from(base + 1), Ipv4Addr::from(base + 2))
}

/// Build a loopback /32 from explicit octets.
fn make_loopback(o1: u8, o2: u8, o3: u8, o4: u8) -> Result<ipnet::IpNet> {
    let addr = Ipv4Addr::new(o1, o2, o3, o4);
    Ipv4Net::new(addr, 32)
        .map(ipnet::IpNet::V4)
        .map_err(|e| Error::Template(format!("loopback alloc: {e}")))
}

// ──────────────────────────────────────────────────────────────────────────────
// Wiring state (passed by &mut to wire())
// ──────────────────────────────────────────────────────────────────────────────

struct WireState {
    /// Base of the 172.16.0.0/16 space; allocates /30s sequentially.
    p2p_base: u32,
    p2p_offset: u32,
    bridge_counter: u32,
    /// Per-node next-eth-index.
    iface_counters: HashMap<String, u32>,
    /// Global link MAC peer counter.
    mac_peer: u8,
    /// Fabric name, used to namespace per-link bridges so two labs
    /// running on the same host don't collide.
    fabric_name: String,
}

impl WireState {
    fn new(p2p_base: Ipv4Addr, fabric_name: &str) -> Self {
        Self {
            p2p_base: u32::from(p2p_base),
            p2p_offset: 0,
            bridge_counter: 0,
            iface_counters: HashMap::new(),
            mac_peer: 0,
            fabric_name: fabric_name.to_string(),
        }
    }

    fn next_p2p(&mut self) -> Result<Ipv4Net> {
        let addr = Ipv4Addr::from(self.p2p_base + self.p2p_offset);
        self.p2p_offset += 4;
        Ipv4Net::new(addr, 30)
            .map_err(|e| Error::Template(format!("p2p alloc: {e}")))
    }

    fn next_bridge(&mut self) -> String {
        let name = format!("br-{}-{:04}", self.fabric_name, self.bridge_counter);
        self.bridge_counter += 1;
        name
    }

    fn next_ifname(&mut self, node: &str) -> String {
        let idx = self.iface_counters.entry(node.to_string()).or_insert(0);
        let name = format!("eth{idx}");
        *idx += 1;
        name
    }

    fn bump_mac(&mut self) -> u8 {
        let v = self.mac_peer;
        self.mac_peer = self.mac_peer.wrapping_add(1);
        v
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// wire() — connect two nodes with a p2p link
// ──────────────────────────────────────────────────────────────────────────────

/// Wire two named nodes together on a fabric p2p link.
///
/// Allocates a /30, bridge, and two MACs; appends `Link` to `links`;
/// pushes `Interface` entries and BGP neighbor records to both nodes.
fn wire(
    nodes: &mut HashMap<String, Node>,
    links: &mut Vec<Link>,
    ws: &mut WireState,
    a_name: &str,
    b_name: &str,
    tier: &str,
) -> Result<()> {
    let subnet = ws.next_p2p()?;
    let (a_ip, b_ip) = p2p_hosts(subnet);
    let bridge = ws.next_bridge();
    let a_ifname = ws.next_ifname(a_name);
    let b_ifname = ws.next_ifname(b_name);
    let peer = ws.bump_mac();

    // Look up role tier codes (need immutable borrow before mutating below).
    let a_tier = nodes
        .get(a_name)
        .map(|n| n.role.tier_code())
        .unwrap_or(0xFF);
    let b_tier = nodes
        .get(b_name)
        .map(|n| n.role.tier_code())
        .unwrap_or(0xFF);
    let a_asn = nodes.get(a_name).and_then(|n| n.asn);
    let b_asn = nodes.get(b_name).and_then(|n| n.asn);
    let a_routed = nodes.get(a_name).map(|n| n.role.is_routed()).unwrap_or(false);
    let b_routed = nodes.get(b_name).map(|n| n.role.is_routed()).unwrap_or(false);

    let a_mac = make_mac(a_tier, peer, 0);
    let b_mac = make_mac(b_tier, peer, 1);

    let a_ip_net = Ipv4Net::new(a_ip, 30)
        .map_err(|e| Error::Template(format!("link ip: {e}")))?;
    let b_ip_net = Ipv4Net::new(b_ip, 30)
        .map_err(|e| Error::Template(format!("link ip: {e}")))?;

    // Push link record.
    links.push(Link {
        bridge: bridge.clone(),
        a: a_name.to_string(),
        b: b_name.to_string(),
        a_ip: Some(ipnet::IpNet::V4(a_ip_net)),
        b_ip: Some(ipnet::IpNet::V4(b_ip_net)),
        subnet: Some(ipnet::IpNet::V4(subnet)),
        a_ifname: a_ifname.clone(),
        b_ifname: b_ifname.clone(),
        a_mac: a_mac.clone(),
        b_mac: b_mac.clone(),
        tier: tier.to_string(),
    });

    // Mutate node-a.
    if let Some(node) = nodes.get_mut(a_name) {
        node.interfaces.push(Interface {
            name: a_ifname.clone(),
            ip: Some(ipnet::IpNet::V4(a_ip_net)),
            peer_ip: Some(IpAddr::V4(b_ip)),
            subnet: Some(ipnet::IpNet::V4(subnet)),
            peer: b_name.to_string(),
            bridge: bridge.clone(),
            mac: a_mac,
            role: Some(tier.to_string()),
        });
        if a_routed {
            if let Some(remote_asn) = b_asn {
                node.bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(b_ip),
                    remote_asn,
                    name: b_name.to_string(),
                    interface: Some(a_ifname),
                });
            }
        }
    }

    // Mutate node-b.
    if let Some(node) = nodes.get_mut(b_name) {
        node.interfaces.push(Interface {
            name: b_ifname.clone(),
            ip: Some(ipnet::IpNet::V4(b_ip_net)),
            peer_ip: Some(IpAddr::V4(a_ip)),
            subnet: Some(ipnet::IpNet::V4(subnet)),
            peer: a_name.to_string(),
            bridge,
            mac: b_mac,
            role: Some(tier.to_string()),
        });
        if b_routed {
            if let Some(remote_asn) = a_asn {
                node.bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(a_ip),
                    remote_asn,
                    name: a_name.to_string(),
                    interface: Some(b_ifname),
                });
            }
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Template impl
// ──────────────────────────────────────────────────────────────────────────────

impl Template for HubSpoke {
    fn name(&self) -> &str {
        "hub-spoke"
    }

    fn display_name(&self) -> &str {
        "Hub and Spoke"
    }

    fn schema(&self) -> &ParameterSchema {
        get_schema()
    }

    fn expand(
        &self,
        fabric_name: &str,
        platform: &str,
        wan_interface: &str,
        params: &Parameters,
    ) -> Result<Topology> {
        // ── 1. Extract & validate parameters ──────────────────────────────
        let hub_count = params.get_u32("hub_count").unwrap_or(1);
        let branch_count = params.get_u32("branch_count").unwrap_or(4);
        let branch_redundant = params.get_bool("branch_redundant").unwrap_or(false);
        let servers_per_branch = params.get_u32("servers_per_branch").unwrap_or(2);

        if !(1..=2).contains(&hub_count) {
            return Err(Error::InvalidParameter(format!(
                "hub_count must be 1-2, got {hub_count}"
            )));
        }
        if !(1..=16).contains(&branch_count) {
            return Err(Error::InvalidParameter(format!(
                "branch_count must be 1-16, got {branch_count}"
            )));
        }
        if !(1..=4).contains(&servers_per_branch) {
            return Err(Error::InvalidParameter(format!(
                "servers_per_branch must be 1-4, got {servers_per_branch}"
            )));
        }

        // ── 2. State ───────────────────────────────────────────────────────
        let mgmt_base = Ipv4Addr::new(192, 168, 0, 0);
        let p2p_base = Ipv4Addr::new(172, 16, 0, 0);

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut links: Vec<Link> = Vec::new();
        let mut ws = WireState::new(p2p_base, fabric_name);

        // ── 3. Control-plane nodes (Seed bootstrap) ────────────────────────
        // bastion → mgmt .2
        nodes.insert(
            "bastion".into(),
            Node {
                name: "bastion".into(),
                role: Role::Bastion,
                nos_type: None,
                asn: None,
                loopback: None,
                mgmt_ip: IpAddr::V4(Ipv4Addr::new(
                    mgmt_base.octets()[0],
                    mgmt_base.octets()[1],
                    mgmt_base.octets()[2],
                    2,
                )),
                mgmt_mac: make_mac(Role::Bastion.tier_code(), 0, 0),
                vcpu: 1,
                memory_mb: 512,
                disk_gb: 5,
                interfaces: vec![],
                bgp_neighbors: vec![],
                bootstrap: Bootstrap::Seed,
            },
        );
        // services → mgmt .3
        nodes.insert(
            "services".into(),
            Node {
                name: "services".into(),
                role: Role::Services,
                nos_type: None,
                asn: None,
                loopback: None,
                mgmt_ip: IpAddr::V4(Ipv4Addr::new(
                    mgmt_base.octets()[0],
                    mgmt_base.octets()[1],
                    mgmt_base.octets()[2],
                    3,
                )),
                mgmt_mac: make_mac(Role::Services.tier_code(), 0, 0),
                vcpu: 1,
                memory_mb: 512,
                disk_gb: 5,
                interfaces: vec![],
                bgp_neighbors: vec![],
                bootstrap: Bootstrap::Seed,
            },
        );

        // ── 4. Hub nodes ───────────────────────────────────────────────────
        // mgmt: .10 (hub-1), .11 (hub-2)
        // loopback: 10.0.0.1, 10.0.0.2
        // ASN: 65000 (shared)
        for i in 1u32..=hub_count {
            let name = format!("hub-{i}");
            let mgmt_octet = 9 + i as u8; // .10, .11
            nodes.insert(
                name.clone(),
                Node {
                    name: name.clone(),
                    role: Role::Hub,
                    nos_type: None,
                    asn: Some(65000),
                    loopback: Some(make_loopback(10, 0, 0, i as u8)?),
                    mgmt_ip: IpAddr::V4(Ipv4Addr::new(
                        mgmt_base.octets()[0],
                        mgmt_base.octets()[1],
                        mgmt_base.octets()[2],
                        mgmt_octet,
                    )),
                    mgmt_mac: make_mac(Role::Hub.tier_code(), 0, i as u8),
                    vcpu: 1,
                    memory_mb: 512,
                    disk_gb: 3,
                    interfaces: vec![],
                    bgp_neighbors: vec![],
                    bootstrap: Bootstrap::Dhcp,
                },
            );
        }

        // ── 5. Branch router nodes ─────────────────────────────────────────
        // mgmt IPs start at .20.
        //   non-redundant: .20 = b1a, .21 = b2a, ...
        //   redundant:     .20 = b1a, .21 = b1b, .22 = b2a, .23 = b2b, ...
        // ASN: 65100 + (b-1); both routers in same branch share ASN.
        // loopback: 10.0.1.{2*b-1} for 'a', 10.0.1.{2*b} for 'b'
        let routers_per_branch: u32 = if branch_redundant { 2 } else { 1 };
        for b in 1u32..=branch_count {
            let branch_asn = 65100 + b - 1;
            let suffixes: &[(&str, u8)] = if branch_redundant {
                &[("a", 0), ("b", 1)]
            } else {
                &[("a", 0)]
            };
            for &(suf, ri) in suffixes {
                let name = format!("branch-{b}{suf}");
                let mgmt_octet = 20u32 + (b - 1) * routers_per_branch + ri as u32;
                if mgmt_octet > 254 {
                    return Err(Error::Template(
                        "mgmt IP space exhausted for branch routers".into(),
                    ));
                }
                // loopback: 10.0.1.<b*2 - 1 + ri>
                let lb_lo = ((b - 1) * 2 + ri as u32 + 1) as u8;
                nodes.insert(
                    name.clone(),
                    Node {
                        name: name.clone(),
                        role: Role::Branch,
                        nos_type: None,
                        asn: Some(branch_asn),
                        loopback: Some(make_loopback(10, 0, 1, lb_lo)?),
                        mgmt_ip: IpAddr::V4(Ipv4Addr::new(
                            mgmt_base.octets()[0],
                            mgmt_base.octets()[1],
                            mgmt_base.octets()[2],
                            mgmt_octet as u8,
                        )),
                        mgmt_mac: make_mac(Role::Branch.tier_code(), b as u8, ri),
                        vcpu: 1,
                        memory_mb: 256,
                        disk_gb: 3,
                        interfaces: vec![],
                        bgp_neighbors: vec![],
                        bootstrap: Bootstrap::Dhcp,
                    },
                );
            }
        }

        // ── 6. Server nodes ────────────────────────────────────────────────
        // mgmt IPs: .100 onwards, branch-major, server-minor ordering.
        let mut srv_offset = 0u32;
        for b in 1u32..=branch_count {
            for s in 1u32..=servers_per_branch {
                let name = format!("srv-{b}-{s}");
                let mgmt_octet = 100u32 + srv_offset;
                if mgmt_octet > 254 {
                    return Err(Error::Template(
                        "mgmt IP space exhausted for servers".into(),
                    ));
                }
                nodes.insert(
                    name.clone(),
                    Node {
                        name: name.clone(),
                        role: Role::Server,
                        nos_type: None,
                        asn: None,
                        loopback: None,
                        mgmt_ip: IpAddr::V4(Ipv4Addr::new(
                            mgmt_base.octets()[0],
                            mgmt_base.octets()[1],
                            mgmt_base.octets()[2],
                            mgmt_octet as u8,
                        )),
                        mgmt_mac: make_mac(Role::Server.tier_code(), b as u8, s as u8),
                        vcpu: 1,
                        memory_mb: 768,
                        disk_gb: 5,
                        interfaces: vec![],
                        bgp_neighbors: vec![],
                        bootstrap: Bootstrap::Dhcp,
                    },
                );
                srv_offset += 1;
            }
        }

        // ── 7. Wiring ──────────────────────────────────────────────────────

        // 7.1  Hub inter-connect (only when hub_count == 2).
        if hub_count == 2 {
            wire(&mut nodes, &mut links, &mut ws, "hub-1", "hub-2", "hub-redundancy")?;
        }

        // 7.2  Every hub ↔ every branch router.
        for i in 1u32..=hub_count {
            let hub_name = format!("hub-{i}");
            for b in 1u32..=branch_count {
                let suffixes: &[&str] = if branch_redundant { &["a", "b"] } else { &["a"] };
                for &suf in suffixes {
                    let branch_name = format!("branch-{b}{suf}");
                    wire(&mut nodes, &mut links, &mut ws, &hub_name, &branch_name, "wan")?;
                }
            }
        }

        // 7.3  Branch local redundancy link: branch-{b}a ↔ branch-{b}b.
        if branch_redundant {
            for b in 1u32..=branch_count {
                wire(
                    &mut nodes,
                    &mut links,
                    &mut ws,
                    &format!("branch-{b}a"),
                    &format!("branch-{b}b"),
                    "branch-redundancy",
                )?;
            }
        }

        // 7.4  Servers ↔ branch routers.
        for b in 1u32..=branch_count {
            for s in 1u32..=servers_per_branch {
                let srv = format!("srv-{b}-{s}");
                // Primary uplink: branch-{b}a ↔ server.
                wire(
                    &mut nodes,
                    &mut links,
                    &mut ws,
                    &format!("branch-{b}a"),
                    &srv,
                    "access",
                )?;
                // Secondary uplink (dual-home): branch-{b}b ↔ server.
                if branch_redundant {
                    wire(
                        &mut nodes,
                        &mut links,
                        &mut ws,
                        &format!("branch-{b}b"),
                        &srv,
                        "access",
                    )?;
                }
            }
        }

        // 7.5  Northbound exit: hub-1 ↔ bastion.
        wire(&mut nodes, &mut links, &mut ws, "hub-1", "bastion", "northbound")?;

        // ── 8. Assemble & return topology ─────────────────────────────────
        let mgmt_cidr = Ipv4Net::new(mgmt_base, 24)
            .map_err(|e| Error::Template(format!("mgmt net: {e}")))?;
        let data_cidr = Ipv4Net::new(Ipv4Addr::new(10, 100, 0, 0), 30)
            .map_err(|e| Error::Template(format!("data net: {e}")))?;
        let loopback_cidr = Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 16)
            .map_err(|e| Error::Template(format!("loopback net: {e}")))?;
        let p2p_cidr = Ipv4Net::new(p2p_base, 16)
            .map_err(|e| Error::Template(format!("p2p net: {e}")))?;

        Ok(Topology {
            name: fabric_name.to_string(),
            template: self.name().to_string(),
            platform: platform.to_string(),
            wan_interface: Some(wan_interface.to_string()),
            nodes,
            links,
            management: Management {
                cidr: ipnet::IpNet::V4(mgmt_cidr),
                gateway: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
                bridge: "br-mgmt".to_string(),
                data_cidr: ipnet::IpNet::V4(data_cidr),
                data_gateway: IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1)),
                data_bridge: "br-data".to_string(),
                dns_domain: "themis.local".to_string(),
            },
            addressing: Addressing {
                loopback_cidr: ipnet::IpNet::V4(loopback_cidr),
                fabric_p2p_cidr: ipnet::IpNet::V4(p2p_cidr),
            },
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_params(vals: &[(&str, serde_json::Value)]) -> Parameters {
        let mut p = Parameters::new();
        for (k, v) in vals {
            p.set(*k, v.clone());
        }
        p
    }

    /// Default configuration:
    ///   hub_count=1, branch_count=4, branch_redundant=false, servers_per_branch=2
    ///
    /// Node count:
    ///   1 hub + 4 branch-a + 8 servers + 2 control = 15
    ///
    /// Link count:
    ///   4 (hub-1 ↔ branch-{1..4}a) + 8 (branch access) + 1 (northbound) = 13
    #[test]
    fn test_default_node_and_link_count() {
        let tpl = HubSpoke;
        let params = Parameters::new(); // all defaults
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        assert_eq!(
            topo.nodes.len(),
            15,
            "expected 15 nodes; got {}: {:?}",
            topo.nodes.len(),
            topo.nodes.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            topo.links.len(),
            13,
            "expected 13 links; got {}",
            topo.links.len()
        );

        // Spot-check specific nodes.
        assert!(topo.nodes.contains_key("bastion"));
        assert!(topo.nodes.contains_key("services"));
        assert!(topo.nodes.contains_key("hub-1"));
        assert!(topo.nodes.contains_key("branch-1a"));
        assert!(topo.nodes.contains_key("branch-4a"));
        assert!(topo.nodes.contains_key("srv-1-1"));
        assert!(topo.nodes.contains_key("srv-4-2"));

        // No second hub, no b-side branch routers.
        assert!(!topo.nodes.contains_key("hub-2"));
        assert!(!topo.nodes.contains_key("branch-1b"));
    }

    /// Redundant dual-hub, 2 branches, redundant, 2 servers per branch.
    ///
    /// Nodes:
    ///   2 hubs + 4 branch routers (2 branches × 2) + 4 servers + 2 control = 12
    ///
    /// Links:
    ///   1 (hub-1↔hub-2)
    ///   + 8 (2 hubs × 2 branches × 2 routers)
    ///   + 2 (branch-{1,2}a ↔ branch-{1,2}b)
    ///   + 4 (primary: branch-{1,2}a ↔ srv-{1,2}-{1,2})
    ///   + 4 (secondary: branch-{1,2}b ↔ srv-{1,2}-{1,2})
    ///   + 1 (hub-1↔bastion)
    ///   = 20
    #[test]
    fn test_redundant_dual_hub() {
        let tpl = HubSpoke;
        let params = make_params(&[
            ("hub_count", serde_json::json!(2)),
            ("branch_count", serde_json::json!(2)),
            ("branch_redundant", serde_json::json!(true)),
            ("servers_per_branch", serde_json::json!(2)),
        ]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        assert_eq!(
            topo.nodes.len(),
            12,
            "expected 12 nodes; got {}: {:?}",
            topo.nodes.len(),
            topo.nodes.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            topo.links.len(),
            20,
            "expected 20 links; got {}",
            topo.links.len()
        );

        assert!(topo.nodes.contains_key("hub-2"));
        assert!(topo.nodes.contains_key("branch-1b"));
        assert!(topo.nodes.contains_key("branch-2b"));
    }

    /// Minimal configuration: 1 hub, 1 branch, no redundancy, 1 server.
    ///
    /// Nodes: 1 hub + 1 branch-a + 1 server + 2 control = 5
    /// Links: 1 (hub-1↔branch-1a) + 1 (branch-1a↔srv-1-1) + 1 (northbound) = 3
    #[test]
    fn test_minimal() {
        let tpl = HubSpoke;
        let params = make_params(&[
            ("hub_count", serde_json::json!(1)),
            ("branch_count", serde_json::json!(1)),
            ("branch_redundant", serde_json::json!(false)),
            ("servers_per_branch", serde_json::json!(1)),
        ]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        assert_eq!(topo.nodes.len(), 5, "expected 5 nodes; got {}", topo.nodes.len());
        assert_eq!(topo.links.len(), 3, "expected 3 links; got {}", topo.links.len());
    }

    #[test]
    fn test_invalid_hub_count() {
        let tpl = HubSpoke;
        let params = make_params(&[("hub_count", serde_json::json!(3))]);
        assert!(tpl.expand("x", "frr-fedora", "eth0", &params).is_err());
    }

    #[test]
    fn test_invalid_branch_count_too_high() {
        let tpl = HubSpoke;
        let params = make_params(&[("branch_count", serde_json::json!(17))]);
        assert!(tpl.expand("x", "frr-fedora", "eth0", &params).is_err());
    }

    #[test]
    fn test_invalid_servers_per_branch() {
        let tpl = HubSpoke;
        let params = make_params(&[("servers_per_branch", serde_json::json!(5))]);
        assert!(tpl.expand("x", "frr-fedora", "eth0", &params).is_err());
    }

    /// Management IPs must be unique across all nodes.
    #[test]
    fn test_mgmt_ips_unique() {
        let tpl = HubSpoke;
        let params = make_params(&[
            ("hub_count", serde_json::json!(2)),
            ("branch_count", serde_json::json!(4)),
            ("branch_redundant", serde_json::json!(true)),
            ("servers_per_branch", serde_json::json!(4)),
        ]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        let ips: Vec<_> = topo.nodes.values().map(|n| n.mgmt_ip).collect();
        let unique: std::collections::HashSet<_> = ips.iter().cloned().collect();
        assert_eq!(ips.len(), unique.len(), "duplicate mgmt IPs");
    }

    /// Bridge names in the link list must be unique (one bridge per link).
    #[test]
    fn test_bridges_unique() {
        let tpl = HubSpoke;
        let params = make_params(&[
            ("hub_count", serde_json::json!(2)),
            ("branch_count", serde_json::json!(4)),
            ("branch_redundant", serde_json::json!(true)),
            ("servers_per_branch", serde_json::json!(2)),
        ]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        let bridges: Vec<_> = topo.links.iter().map(|l| l.bridge.clone()).collect();
        let unique: std::collections::HashSet<_> = bridges.iter().cloned().collect();
        assert_eq!(bridges.len(), unique.len(), "duplicate bridge names");
    }

    /// Servers must not have BGP neighbors; hubs must have some.
    #[test]
    fn test_bgp_assignments() {
        let tpl = HubSpoke;
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &Parameters::new())
            .expect("expand should succeed");

        for (name, node) in &topo.nodes {
            if node.role == Role::Server {
                assert!(
                    node.bgp_neighbors.is_empty(),
                    "server {name} should have no BGP neighbors"
                );
            }
        }

        let hub1 = topo.nodes.get("hub-1").expect("hub-1 must exist");
        assert!(!hub1.bgp_neighbors.is_empty(), "hub-1 must have BGP neighbors");
    }

    /// Hub ASN is always 65000; both hubs share it.
    #[test]
    fn test_hub_asn() {
        let tpl = HubSpoke;
        let params = make_params(&[("hub_count", serde_json::json!(2))]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        assert_eq!(topo.nodes["hub-1"].asn, Some(65000));
        assert_eq!(topo.nodes["hub-2"].asn, Some(65000));
    }

    /// Branch ASNs: branch-b: 65100+b-1; both a and b suffixes share ASN.
    #[test]
    fn test_branch_asns() {
        let tpl = HubSpoke;
        let params = make_params(&[
            ("branch_count", serde_json::json!(3)),
            ("branch_redundant", serde_json::json!(true)),
        ]);
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &params)
            .expect("expand should succeed");

        for b in 1u32..=3 {
            let expected = 65100 + b - 1;
            for suf in &["a", "b"] {
                let name = format!("branch-{b}{suf}");
                let node = topo.nodes.get(&name).unwrap_or_else(|| panic!("{name} missing"));
                assert_eq!(node.asn, Some(expected), "{name} ASN wrong");
            }
        }
    }

    /// Control-plane nodes use Seed bootstrap; all others use Dhcp.
    #[test]
    fn test_bootstrap_modes() {
        let tpl = HubSpoke;
        let topo = tpl
            .expand("test-lab", "frr-fedora", "eth0", &Parameters::new())
            .expect("expand should succeed");

        for (name, node) in &topo.nodes {
            let expected = if node.role.is_control_plane() {
                Bootstrap::Seed
            } else {
                Bootstrap::Dhcp
            };
            assert_eq!(node.bootstrap, expected, "{name} has wrong bootstrap mode");
        }
    }

    /// Schema must advertise all four parameters.
    #[test]
    fn test_schema_fields() {
        let tpl = HubSpoke;
        let schema = tpl.schema();
        for key in &["hub_count", "branch_count", "branch_redundant", "servers_per_branch"] {
            assert!(schema.fields.contains_key(*key), "schema missing '{key}'");
        }
    }

    /// Template name must be "hub-spoke".
    #[test]
    fn test_template_name() {
        assert_eq!(HubSpoke.name(), "hub-spoke");
    }
}
