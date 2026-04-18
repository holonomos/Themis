//! clos-3tier — Border + Spine + Leaf + Server 3-stage Clos.
//!
//! Populated by Phase 5a agent. Port-reference: former Python `templates/clos-3tier/expander.py`.
//!
//! # Topology shape (defaults: 2 borders, 2 spines, 4 racks × 2 leafs, 4 servers/rack)
//!
//! ```text
//!  bastion
//!    |  \----------\
//! border-1      border-2
//!  / \            / \
//! sp1 sp2       sp1 sp2
//!  |\ /|         ...
//! leaf-1a/1b  leaf-2a/2b  ...
//!  |   |
//! srv-1-1 ... srv-1-4
//! ```

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use ipnet::{IpNet, Ipv4Net};

use themis_core::{
    Addressing, BgpNeighbor, Bootstrap, Interface, Link, Management, Node, ParameterDef,
    ParameterSchema, ParameterType, Parameters, Result, Role, Template, Topology,
};

// ---------------------------------------------------------------------------
// Public type
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Clos3Tier;

// ---------------------------------------------------------------------------
// Schema (lazy-static via OnceCell-style approach — just a static ref)
// ---------------------------------------------------------------------------

static SCHEMA: std::sync::OnceLock<ParameterSchema> = std::sync::OnceLock::new();

fn schema() -> &'static ParameterSchema {
    SCHEMA.get_or_init(|| {
        ParameterSchema::new()
            .with(
                "border_count",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: Some(serde_json::json!(2)),
                    min: Some(1),
                    max: Some(4),
                    description: Some("Number of border (exit) routers".into()),
                },
            )
            .with(
                "spine_count",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: Some(serde_json::json!(2)),
                    min: Some(1),
                    max: Some(4),
                    description: Some("Number of spine routers".into()),
                },
            )
            .with(
                "rack_count",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: Some(serde_json::json!(4)),
                    min: Some(1),
                    max: Some(8),
                    description: Some("Number of server racks".into()),
                },
            )
            .with(
                "servers_per_rack",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: Some(serde_json::json!(4)),
                    min: Some(1),
                    max: Some(8),
                    description: Some("Number of servers per rack".into()),
                },
            )
    })
}

// ---------------------------------------------------------------------------
// Internal helpers — addressing state
// ---------------------------------------------------------------------------

/// Tracks the rolling `/30` allocation cursor for fabric point-to-point links.
struct P2pAllocator {
    /// Current base (the `.0` of the next /30 to hand out).
    next: u32,
}

impl P2pAllocator {
    fn new() -> Self {
        // Base: 172.16.0.0/16 → first /30 starts at 172.16.0.0
        let base = u32::from(Ipv4Addr::new(172, 16, 0, 0));
        Self { next: base }
    }

    /// Allocate the next `/30`. Returns `(subnet, a_ip, b_ip)` as `Ipv4Net` / `Ipv4Addr`.
    fn alloc(&mut self) -> (Ipv4Net, Ipv4Addr, Ipv4Addr) {
        let base = self.next;
        self.next += 4;
        let subnet = Ipv4Net::new(Ipv4Addr::from(base), 30).unwrap();
        let a_ip = Ipv4Addr::from(base + 1);
        let b_ip = Ipv4Addr::from(base + 2);
        (subnet, a_ip, b_ip)
    }
}

// ---------------------------------------------------------------------------
// MAC generation helpers
// ---------------------------------------------------------------------------

/// OUI: `02:4E:57` (locally-administered; "NW" in ASCII)
fn mgmt_mac(tier_byte: u8, role_index: u8) -> String {
    format!("02:4E:57:{:02X}:00:{:02X}", tier_byte, role_index)
}

fn iface_mac(tier_byte: u8, peer_counter: u8, role_index: u8) -> String {
    format!("02:4E:57:{:02X}:{:02X}:{:02X}", tier_byte, peer_counter, role_index)
}

// ---------------------------------------------------------------------------
// Template impl
// ---------------------------------------------------------------------------

impl Template for Clos3Tier {
    fn name(&self) -> &str {
        "clos-3tier"
    }

    fn display_name(&self) -> &str {
        "3-Stage Clos (Border / Spine / Leaf / Server)"
    }

    fn schema(&self) -> &ParameterSchema {
        self::schema()
    }

    fn expand(
        &self,
        fabric_name: &str,
        platform: &str,
        wan_interface: &str,
        params: &Parameters,
    ) -> Result<Topology> {
        // ------------------------------------------------------------------
        // 1. Read parameters (fall back to schema defaults)
        // ------------------------------------------------------------------
        let border_count = params
            .get_u32("border_count")
            .unwrap_or(2) as usize;
        let spine_count = params
            .get_u32("spine_count")
            .unwrap_or(2) as usize;
        let rack_count = params
            .get_u32("rack_count")
            .unwrap_or(4) as usize;
        let servers_per_rack = params
            .get_u32("servers_per_rack")
            .unwrap_or(4) as usize;

        // ------------------------------------------------------------------
        // 2. Addressing infrastructure
        // ------------------------------------------------------------------
        let mut p2p = P2pAllocator::new();

        // Loopback allocator:  10.0.<tier_byte>.<idx>  where idx is 1-based
        // We hand them out per role group.
        let mut border_lo_idx: u8 = 0;
        let mut spine_lo_idx: u8 = 0;
        let mut leaf_lo_idx: u8 = 0;

        let loopback_ip = |tier_byte: u8, idx: u8| -> Ipv4Net {
            Ipv4Net::new(Ipv4Addr::new(10, 0, tier_byte, idx), 32).unwrap()
        };

        // ------------------------------------------------------------------
        // 3. Management
        // ------------------------------------------------------------------
        let mgmt_cidr: Ipv4Net = "192.168.0.0/24".parse().unwrap();
        let mgmt_gateway = Ipv4Addr::new(192, 168, 0, 1);
        let mgmt_bridge = format!("br-mgmt-{}", fabric_name);

        let data_cidr: Ipv4Net = "10.100.0.0/30".parse().unwrap();
        let data_gateway = Ipv4Addr::new(10, 100, 0, 1);
        let data_bridge = format!("br-data-{}", fabric_name);

        let management = Management {
            cidr: IpNet::V4(mgmt_cidr),
            gateway: IpAddr::V4(mgmt_gateway),
            bridge: mgmt_bridge,
            data_cidr: IpNet::V4(data_cidr),
            data_gateway: IpAddr::V4(data_gateway),
            data_bridge: data_bridge.clone(),
            dns_domain: "themis.local".into(),
        };

        // ------------------------------------------------------------------
        // 4. Build node map and link vec
        // ------------------------------------------------------------------
        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut links: Vec<Link> = Vec::new();

        // Track per-node peer counters (for deterministic iface MAC generation)
        // key = node_name, value = next peer_counter to use (starts at 1)
        let mut peer_ctr: HashMap<String, u8> = HashMap::new();
        let next_peer_ctr = |peer_ctr: &mut HashMap<String, u8>, name: &str| -> u8 {
            let ctr = peer_ctr.entry(name.to_string()).or_insert(0);
            *ctr += 1;
            *ctr
        };

        // Shared link index for bridge naming br000, br001, ...
        let mut link_idx: usize = 0;

        // ------------------------------------------------------------------
        // 4a. Control-plane nodes
        // ------------------------------------------------------------------
        // bastion, services, orchestrator, telemetry, registry
        // mgmt IPs: .2, .3, .4, .5, .6
        let cp_specs: &[(&str, Role, u8, u8)] = &[
            ("bastion",      Role::Bastion,      0x05, 2),
            ("services",     Role::Services,     0x06, 3),
            ("orchestrator", Role::Orchestrator, 0x08, 4),
            ("telemetry",    Role::Telemetry,    0x07, 5),
            ("registry",     Role::Registry,     0x09, 6),
        ];

        for (node_name, role, _tier_byte, mgmt_last_octet) in cp_specs {
            let tb = role.tier_code();
            let mac = mgmt_mac(tb, *mgmt_last_octet);
            nodes.insert(
                node_name.to_string(),
                Node {
                    name: node_name.to_string(),
                    role: *role,
                    nos_type: None,
                    asn: None,
                    loopback: None,
                    mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, *mgmt_last_octet)),
                    mgmt_mac: mac,
                    vcpu: 1,
                    memory_mb: 256,
                    disk_gb: 3,
                    interfaces: Vec::new(),
                    bgp_neighbors: Vec::new(),
                    bootstrap: Bootstrap::Seed,
                },
            );
        }

        // bastion data interface (eth-data)
        {
            let bastion = nodes.get_mut("bastion").unwrap();
            bastion.interfaces.push(Interface {
                name: "eth-data".into(),
                ip: Some(IpNet::V4("10.100.0.2/30".parse().unwrap())),
                peer_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1))),
                subnet: Some(IpNet::V4("10.100.0.0/30".parse().unwrap())),
                peer: "host".into(),
                bridge: data_bridge.clone(),
                mac: "02:4E:57:05:FE:01".into(),
                role: Some("data".into()),
            });
        }

        // ------------------------------------------------------------------
        // 4b. Border nodes
        // ------------------------------------------------------------------
        // All borders share ASN 65000; mgmt IPs start at .10
        // tier_byte = 0x01, role_index = i (1-based)
        let mut border_names: Vec<String> = Vec::new();
        for i in 1..=border_count {
            let node_name = format!("border-{}", i);
            border_names.push(node_name.clone());
            border_lo_idx += 1;
            let lo = loopback_ip(0x01, border_lo_idx);
            let mgmt_last: u8 = 10 + (i as u8) - 1;
            let tb = Role::Border.tier_code();
            nodes.insert(
                node_name.clone(),
                Node {
                    name: node_name.clone(),
                    role: Role::Border,
                    nos_type: None,
                    asn: Some(65000),
                    loopback: Some(IpNet::V4(lo)),
                    mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, mgmt_last)),
                    mgmt_mac: mgmt_mac(tb, i as u8),
                    vcpu: 1,
                    memory_mb: 256,
                    disk_gb: 3,
                    interfaces: Vec::new(),
                    bgp_neighbors: Vec::new(),
                    bootstrap: Bootstrap::Dhcp,
                },
            );
        }

        // ------------------------------------------------------------------
        // 4c. Spine nodes
        // ------------------------------------------------------------------
        // spine i: ASN 65001 + i - 1; mgmt IPs start at .20
        let mut spine_names: Vec<String> = Vec::new();
        for i in 1..=spine_count {
            let node_name = format!("spine-{}", i);
            spine_names.push(node_name.clone());
            spine_lo_idx += 1;
            let lo = loopback_ip(0x02, spine_lo_idx);
            let mgmt_last: u8 = 20 + (i as u8) - 1;
            let asn = 65001 + (i as u32) - 1;
            let tb = Role::Spine.tier_code();
            nodes.insert(
                node_name.clone(),
                Node {
                    name: node_name.clone(),
                    role: Role::Spine,
                    nos_type: None,
                    asn: Some(asn),
                    loopback: Some(IpNet::V4(lo)),
                    mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, mgmt_last)),
                    mgmt_mac: mgmt_mac(tb, i as u8),
                    vcpu: 1,
                    memory_mb: 256,
                    disk_gb: 3,
                    interfaces: Vec::new(),
                    bgp_neighbors: Vec::new(),
                    bootstrap: Bootstrap::Dhcp,
                },
            );
        }

        // ------------------------------------------------------------------
        // 4d. Leaf nodes (2 per rack: leaf-{r}a, leaf-{r}b)
        // ------------------------------------------------------------------
        // Rack r: both leafs share ASN 65101 + r - 1
        // Leaf mgmt IPs start at .30, incrementing per leaf (not per rack)
        let mut leaf_names: Vec<(String, String)> = Vec::new(); // (leaf_a, leaf_b) per rack
        let mut leaf_mgmt_cursor: u8 = 30;
        for r in 1..=rack_count {
            let leaf_a_name = format!("leaf-{}a", r);
            let leaf_b_name = format!("leaf-{}b", r);
            leaf_names.push((leaf_a_name.clone(), leaf_b_name.clone()));
            let asn = 65101 + (r as u32) - 1;
            let tb = Role::Leaf.tier_code();

            for (leaf_name, suffix_idx) in [(&leaf_a_name, 1u8), (&leaf_b_name, 2u8)] {
                leaf_lo_idx += 1;
                let lo = loopback_ip(0x03, leaf_lo_idx);
                let mgmt_last = leaf_mgmt_cursor;
                leaf_mgmt_cursor += 1;
                // role_index: unique per-leaf — use leaf_lo_idx for mac uniqueness
                nodes.insert(
                    leaf_name.clone(),
                    Node {
                        name: leaf_name.clone(),
                        role: Role::Leaf,
                        nos_type: None,
                        asn: Some(asn),
                        loopback: Some(IpNet::V4(lo)),
                        mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, mgmt_last)),
                        mgmt_mac: mgmt_mac(tb, leaf_lo_idx),
                        vcpu: 1,
                        memory_mb: 256,
                        disk_gb: 3,
                        interfaces: Vec::new(),
                        bgp_neighbors: Vec::new(),
                        bootstrap: Bootstrap::Dhcp,
                    },
                );
                let _ = suffix_idx; // suppress unused warning
            }
        }

        // ------------------------------------------------------------------
        // 4e. Server nodes
        // ------------------------------------------------------------------
        // server-{r}-{s}: mgmt IPs start at .50, incrementing per server
        let mut server_mgmt_cursor: u8 = 50;
        // servers[r][s] = name
        let mut server_names: Vec<Vec<String>> = Vec::new();
        for r in 1..=rack_count {
            let mut rack_servers: Vec<String> = Vec::new();
            for s in 1..=servers_per_rack {
                let node_name = format!("server-{}-{}", r, s);
                rack_servers.push(node_name.clone());
                let mgmt_last = server_mgmt_cursor;
                server_mgmt_cursor += 1;
                // servers are not routed; use a simple sequential role_index
                let role_index = mgmt_last; // reuse mgmt octet as a unique index
                let tb = Role::Server.tier_code();
                nodes.insert(
                    node_name.clone(),
                    Node {
                        name: node_name.clone(),
                        role: Role::Server,
                        nos_type: None,
                        asn: None,
                        loopback: None,
                        mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, mgmt_last)),
                        mgmt_mac: mgmt_mac(tb, role_index),
                        vcpu: 1,
                        memory_mb: 256,
                        disk_gb: 3,
                        interfaces: Vec::new(),
                        bgp_neighbors: Vec::new(),
                        bootstrap: Bootstrap::Dhcp,
                    },
                );
            }
            server_names.push(rack_servers);
        }

        // ------------------------------------------------------------------
        // 5. Wiring
        // ------------------------------------------------------------------
        // Helper: create a link and attach interfaces to both nodes.
        // Returns the newly created Link (already pushed to `links`).
        //
        // We define a closure-like macro-equivalent as a nested fn with explicit args.

        // ---- 5a. border-i ↔ bastion (border's northbound uplink) ----------
        for border_name in &border_names {
            let bridge = format!("br-{}-{:03}", fabric_name, link_idx);
            link_idx += 1;
            let (subnet, a_ip, b_ip) = p2p.alloc();

            let a_node = border_name.clone();
            let b_node = "bastion".to_string();

            let a_tb = Role::Border.tier_code();
            let a_role_idx = a_node
                .strip_prefix("border-")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);
            let b_tb = Role::Bastion.tier_code();
            // bastion role_index for iface MAC: reuse mgmt_last = 2
            let b_role_idx: u8 = 2;

            let a_pc = next_peer_ctr(&mut peer_ctr, &a_node);
            let b_pc = next_peer_ctr(&mut peer_ctr, &b_node);

            let a_mac = iface_mac(a_tb, a_pc, a_role_idx);
            let b_mac = iface_mac(b_tb, b_pc, b_role_idx);

            let a_ifname = format!("eth{}", a_pc);
            let b_ifname = format!("eth{}", b_pc);

            // Attach interface to border (a-side)
            {
                let a_ip_net = IpNet::V4(Ipv4Net::new(a_ip, 30).unwrap());
                let node = nodes.get_mut(&a_node).unwrap();
                node.interfaces.push(Interface {
                    name: a_ifname.clone(),
                    ip: Some(a_ip_net),
                    peer_ip: Some(IpAddr::V4(b_ip)),
                    subnet: Some(IpNet::V4(subnet)),
                    peer: b_node.clone(),
                    bridge: bridge.clone(),
                    mac: a_mac.clone(),
                    role: Some("fabric".into()),
                });
                // BGP: both border and bastion are routed? Bastion is NOT in is_routed()
                // so we skip BGP for this link (border ↔ bastion: bastion not routed)
            }

            // Attach interface to bastion (b-side)
            {
                let b_ip_net = IpNet::V4(Ipv4Net::new(b_ip, 30).unwrap());
                let node = nodes.get_mut(&b_node).unwrap();
                node.interfaces.push(Interface {
                    name: b_ifname.clone(),
                    ip: Some(b_ip_net),
                    peer_ip: Some(IpAddr::V4(a_ip)),
                    subnet: Some(IpNet::V4(subnet)),
                    peer: a_node.clone(),
                    bridge: bridge.clone(),
                    mac: b_mac.clone(),
                    role: Some("fabric".into()),
                });
            }

            links.push(Link {
                bridge: bridge.clone(),
                a: a_node,
                b: b_node,
                a_ip: Some(IpNet::V4(Ipv4Net::new(a_ip, 30).unwrap())),
                b_ip: Some(IpNet::V4(Ipv4Net::new(b_ip, 30).unwrap())),
                subnet: Some(IpNet::V4(subnet)),
                a_ifname,
                b_ifname,
                a_mac,
                b_mac,
                tier: "border-bastion".into(),
            });
        }

        // ---- 5b. border-i ↔ spine-j (full mesh) --------------------------
        for border_name in &border_names {
            for spine_name in &spine_names {
                let bridge = format!("br-{}-{:03}", fabric_name, link_idx);
                link_idx += 1;
                let (subnet, a_ip, b_ip) = p2p.alloc();

                let a_node = border_name.clone();
                let b_node = spine_name.clone();

                let a_tb = Role::Border.tier_code();
                let a_role_idx = a_node
                    .strip_prefix("border-")
                    .and_then(|s| s.parse::<u8>().ok())
                    .unwrap_or(1);
                let b_tb = Role::Spine.tier_code();
                let b_role_idx = b_node
                    .strip_prefix("spine-")
                    .and_then(|s| s.parse::<u8>().ok())
                    .unwrap_or(1);

                let a_pc = next_peer_ctr(&mut peer_ctr, &a_node);
                let b_pc = next_peer_ctr(&mut peer_ctr, &b_node);

                let a_mac = iface_mac(a_tb, a_pc, a_role_idx);
                let b_mac = iface_mac(b_tb, b_pc, b_role_idx);

                let a_ifname = format!("eth{}", a_pc);
                let b_ifname = format!("eth{}", b_pc);

                let a_ip_net = IpNet::V4(Ipv4Net::new(a_ip, 30).unwrap());
                let b_ip_net = IpNet::V4(Ipv4Net::new(b_ip, 30).unwrap());

                // Attach interface to border
                {
                    let node = nodes.get_mut(&a_node).unwrap();
                    node.interfaces.push(Interface {
                        name: a_ifname.clone(),
                        ip: Some(a_ip_net),
                        peer_ip: Some(IpAddr::V4(b_ip)),
                        subnet: Some(IpNet::V4(subnet)),
                        peer: b_node.clone(),
                        bridge: bridge.clone(),
                        mac: a_mac.clone(),
                        role: Some("fabric".into()),
                    });
                }

                // Attach interface to spine
                {
                    let node = nodes.get_mut(&b_node).unwrap();
                    node.interfaces.push(Interface {
                        name: b_ifname.clone(),
                        ip: Some(b_ip_net),
                        peer_ip: Some(IpAddr::V4(a_ip)),
                        subnet: Some(IpNet::V4(subnet)),
                        peer: a_node.clone(),
                        bridge: bridge.clone(),
                        mac: b_mac.clone(),
                        role: Some("fabric".into()),
                    });
                }

                // BGP: both border and spine are routed — exchange neighbors
                let border_asn = nodes[&a_node].asn.unwrap();
                let spine_asn = nodes[&b_node].asn.unwrap();
                nodes.get_mut(&a_node).unwrap().bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(b_ip),
                    remote_asn: spine_asn,
                    name: b_node.clone(),
                    interface: Some(a_ifname.clone()),
                });
                nodes.get_mut(&b_node).unwrap().bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(a_ip),
                    remote_asn: border_asn,
                    name: a_node.clone(),
                    interface: Some(b_ifname.clone()),
                });

                links.push(Link {
                    bridge: bridge.clone(),
                    a: a_node,
                    b: b_node,
                    a_ip: Some(a_ip_net),
                    b_ip: Some(b_ip_net),
                    subnet: Some(IpNet::V4(subnet)),
                    a_ifname,
                    b_ifname,
                    a_mac,
                    b_mac,
                    tier: "border-spine".into(),
                });
            }
        }

        // ---- 5c. spine-j ↔ every leaf (full mesh) ------------------------
        // All leafs flattened
        let all_leaf_names: Vec<String> = leaf_names
            .iter()
            .flat_map(|(a, b)| [a.clone(), b.clone()])
            .collect();

        for spine_name in &spine_names {
            for leaf_name in &all_leaf_names {
                let bridge = format!("br-{}-{:03}", fabric_name, link_idx);
                link_idx += 1;
                let (subnet, a_ip, b_ip) = p2p.alloc();

                let a_node = spine_name.clone();
                let b_node = leaf_name.clone();

                let a_tb = Role::Spine.tier_code();
                let a_role_idx = a_node
                    .strip_prefix("spine-")
                    .and_then(|s| s.parse::<u8>().ok())
                    .unwrap_or(1);
                let b_tb = Role::Leaf.tier_code();
                // leaf role_index: parse the rack number from leaf-{r}a or leaf-{r}b
                let b_role_idx: u8 = leaf_role_index(&b_node);

                let a_pc = next_peer_ctr(&mut peer_ctr, &a_node);
                let b_pc = next_peer_ctr(&mut peer_ctr, &b_node);

                let a_mac = iface_mac(a_tb, a_pc, a_role_idx);
                let b_mac = iface_mac(b_tb, b_pc, b_role_idx);

                let a_ifname = format!("eth{}", a_pc);
                let b_ifname = format!("eth{}", b_pc);

                let a_ip_net = IpNet::V4(Ipv4Net::new(a_ip, 30).unwrap());
                let b_ip_net = IpNet::V4(Ipv4Net::new(b_ip, 30).unwrap());

                // Attach interface to spine
                {
                    let node = nodes.get_mut(&a_node).unwrap();
                    node.interfaces.push(Interface {
                        name: a_ifname.clone(),
                        ip: Some(a_ip_net),
                        peer_ip: Some(IpAddr::V4(b_ip)),
                        subnet: Some(IpNet::V4(subnet)),
                        peer: b_node.clone(),
                        bridge: bridge.clone(),
                        mac: a_mac.clone(),
                        role: Some("fabric".into()),
                    });
                }

                // Attach interface to leaf
                {
                    let node = nodes.get_mut(&b_node).unwrap();
                    node.interfaces.push(Interface {
                        name: b_ifname.clone(),
                        ip: Some(b_ip_net),
                        peer_ip: Some(IpAddr::V4(a_ip)),
                        subnet: Some(IpNet::V4(subnet)),
                        peer: a_node.clone(),
                        bridge: bridge.clone(),
                        mac: b_mac.clone(),
                        role: Some("fabric".into()),
                    });
                }

                // BGP: both spine and leaf are routed
                let spine_asn = nodes[&a_node].asn.unwrap();
                let leaf_asn = nodes[&b_node].asn.unwrap();
                nodes.get_mut(&a_node).unwrap().bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(b_ip),
                    remote_asn: leaf_asn,
                    name: b_node.clone(),
                    interface: Some(a_ifname.clone()),
                });
                nodes.get_mut(&b_node).unwrap().bgp_neighbors.push(BgpNeighbor {
                    ip: IpAddr::V4(a_ip),
                    remote_asn: spine_asn,
                    name: a_node.clone(),
                    interface: Some(b_ifname.clone()),
                });

                links.push(Link {
                    bridge: bridge.clone(),
                    a: a_node,
                    b: b_node,
                    a_ip: Some(a_ip_net),
                    b_ip: Some(b_ip_net),
                    subnet: Some(IpNet::V4(subnet)),
                    a_ifname,
                    b_ifname,
                    a_mac,
                    b_mac,
                    tier: "spine-leaf".into(),
                });
            }
        }

        // ---- 5d. leaf-{r}a & leaf-{r}b ↔ each server in rack r -----------
        for (r_idx, (rack_servers, (leaf_a_name, leaf_b_name))) in
            server_names.iter().zip(leaf_names.iter()).enumerate()
        {
            let r = r_idx + 1;
            for server_name in rack_servers {
                for leaf_name in [leaf_a_name, leaf_b_name] {
                    let bridge = format!("br-{}-{:03}", fabric_name, link_idx);
                    link_idx += 1;
                    let (subnet, a_ip, b_ip) = p2p.alloc();

                    // leaf is a-side, server is b-side
                    let a_node = leaf_name.clone();
                    let b_node = server_name.clone();

                    let a_tb = Role::Leaf.tier_code();
                    let a_role_idx: u8 = leaf_role_index(&a_node);
                    let b_tb = Role::Server.tier_code();
                    // server role_index: use mgmt IP last octet
                    let b_role_idx: u8 = {
                        let node = &nodes[&b_node];
                        if let IpAddr::V4(ip) = node.mgmt_ip {
                            ip.octets()[3]
                        } else {
                            1
                        }
                    };

                    let a_pc = next_peer_ctr(&mut peer_ctr, &a_node);
                    let b_pc = next_peer_ctr(&mut peer_ctr, &b_node);

                    let a_mac = iface_mac(a_tb, a_pc, a_role_idx);
                    let b_mac = iface_mac(b_tb, b_pc, b_role_idx);

                    let a_ifname = format!("eth{}", a_pc);
                    let b_ifname = format!("eth{}", b_pc);

                    let a_ip_net = IpNet::V4(Ipv4Net::new(a_ip, 30).unwrap());
                    let b_ip_net = IpNet::V4(Ipv4Net::new(b_ip, 30).unwrap());

                    // Attach interface to leaf
                    {
                        let node = nodes.get_mut(&a_node).unwrap();
                        node.interfaces.push(Interface {
                            name: a_ifname.clone(),
                            ip: Some(a_ip_net),
                            peer_ip: Some(IpAddr::V4(b_ip)),
                            subnet: Some(IpNet::V4(subnet)),
                            peer: b_node.clone(),
                            bridge: bridge.clone(),
                            mac: a_mac.clone(),
                            role: Some("server".into()),
                        });
                    }

                    // Attach interface to server
                    {
                        let node = nodes.get_mut(&b_node).unwrap();
                        node.interfaces.push(Interface {
                            name: b_ifname.clone(),
                            ip: Some(b_ip_net),
                            peer_ip: Some(IpAddr::V4(a_ip)),
                            subnet: Some(IpNet::V4(subnet)),
                            peer: a_node.clone(),
                            bridge: bridge.clone(),
                            mac: b_mac.clone(),
                            role: Some("server".into()),
                        });
                    }

                    // Leaf is routed; server is not — no BGP for leaf↔server links

                    links.push(Link {
                        bridge: bridge.clone(),
                        a: a_node,
                        b: b_node,
                        a_ip: Some(a_ip_net),
                        b_ip: Some(b_ip_net),
                        subnet: Some(IpNet::V4(subnet)),
                        a_ifname,
                        b_ifname,
                        a_mac,
                        b_mac,
                        tier: format!("leaf-server-rack{}", r),
                    });
                }
            }
        }

        // ------------------------------------------------------------------
        // 6. Assemble and return
        // ------------------------------------------------------------------
        let addressing = Addressing {
            loopback_cidr: IpNet::V4("10.0.0.0/16".parse().unwrap()),
            fabric_p2p_cidr: IpNet::V4("172.16.0.0/16".parse().unwrap()),
        };

        Ok(Topology {
            name: fabric_name.to_string(),
            template: "clos-3tier".into(),
            platform: platform.to_string(),
            wan_interface: Some(wan_interface.to_string()),
            nodes,
            links,
            management,
            addressing,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a small role_index from a leaf name like `leaf-3a` or `leaf-10b`.
/// Uses the rack number (numeric portion) as the index byte.
fn leaf_role_index(name: &str) -> u8 {
    // name is "leaf-{r}a" or "leaf-{r}b"
    // strip "leaf-", then strip trailing 'a' or 'b'
    name.strip_prefix("leaf-")
        .and_then(|s| s.strip_suffix('a').or_else(|| s.strip_suffix('b')))
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_params() -> Parameters {
        Parameters::new()
    }

    fn expand_default() -> Topology {
        let t = Clos3Tier;
        t.expand("test-fabric", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed with default parameters")
    }

    /// Default parameters: 2 borders, 2 spines, 4 racks × 2 leafs, 4 servers/rack
    ///
    /// Expected node count:
    ///   5 control-plane  (bastion, services, orchestrator, telemetry, registry)
    ///   2 borders
    ///   2 spines
    ///   4 racks × 2 leafs = 8 leafs
    ///   4 racks × 4 servers = 16 servers
    ///   Total = 5 + 2 + 2 + 8 + 16 = 33
    #[test]
    fn test_default_node_count() {
        let topo = expand_default();
        assert_eq!(
            topo.nodes.len(),
            33,
            "Expected 33 nodes at default parameters, got {}",
            topo.nodes.len()
        );
    }

    /// Verify named control-plane nodes are present
    #[test]
    fn test_control_plane_nodes_present() {
        let topo = expand_default();
        for name in &["bastion", "services", "orchestrator", "telemetry", "registry"] {
            assert!(topo.nodes.contains_key(*name), "missing control-plane node: {}", name);
        }
    }

    /// Verify borders
    #[test]
    fn test_border_nodes() {
        let topo = expand_default();
        assert!(topo.nodes.contains_key("border-1"));
        assert!(topo.nodes.contains_key("border-2"));
        assert_eq!(topo.nodes["border-1"].asn, Some(65000));
        assert_eq!(topo.nodes["border-2"].asn, Some(65000));
    }

    /// Verify spines
    #[test]
    fn test_spine_nodes() {
        let topo = expand_default();
        assert!(topo.nodes.contains_key("spine-1"));
        assert!(topo.nodes.contains_key("spine-2"));
        assert_eq!(topo.nodes["spine-1"].asn, Some(65001));
        assert_eq!(topo.nodes["spine-2"].asn, Some(65002));
    }

    /// Verify leaf nodes and their shared ASN within a rack
    #[test]
    fn test_leaf_nodes_and_asn() {
        let topo = expand_default();
        for r in 1..=4usize {
            let la = format!("leaf-{}a", r);
            let lb = format!("leaf-{}b", r);
            assert!(topo.nodes.contains_key(&la), "missing {}", la);
            assert!(topo.nodes.contains_key(&lb), "missing {}", lb);
            let expected_asn = 65101 + (r as u32) - 1;
            assert_eq!(topo.nodes[&la].asn, Some(expected_asn));
            assert_eq!(topo.nodes[&lb].asn, Some(expected_asn));
        }
    }

    /// Verify server nodes exist
    #[test]
    fn test_server_nodes() {
        let topo = expand_default();
        for r in 1..=4usize {
            for s in 1..=4usize {
                let name = format!("server-{}-{}", r, s);
                assert!(topo.nodes.contains_key(&name), "missing {}", name);
            }
        }
    }

    /// Verify bootstrap modes
    #[test]
    fn test_bootstrap_modes() {
        let topo = expand_default();
        for name in &["bastion", "services", "orchestrator", "telemetry", "registry"] {
            assert_eq!(
                topo.nodes[*name].bootstrap,
                Bootstrap::Seed,
                "{} should have Seed bootstrap",
                name
            );
        }
        assert_eq!(topo.nodes["border-1"].bootstrap, Bootstrap::Dhcp);
        assert_eq!(topo.nodes["spine-1"].bootstrap, Bootstrap::Dhcp);
        assert_eq!(topo.nodes["leaf-1a"].bootstrap, Bootstrap::Dhcp);
        assert_eq!(topo.nodes["server-1-1"].bootstrap, Bootstrap::Dhcp);
    }

    /// Verify link count at default parameters.
    ///
    /// border↔bastion:   2 borders × 1 = 2
    /// border↔spine:     2 borders × 2 spines = 4
    /// spine↔leaf:       2 spines × 8 leafs = 16
    /// leaf↔server:      8 leafs × 4 servers ← wait, each server connects to BOTH leafs in its rack
    ///                   So: 4 racks × 4 servers × 2 leafs = 32
    ///
    /// Total = 2 + 4 + 16 + 32 = 54
    #[test]
    fn test_default_link_count() {
        let topo = expand_default();
        assert_eq!(
            topo.links.len(),
            54,
            "Expected 54 links at default parameters, got {}",
            topo.links.len()
        );
    }

    /// Verify bastion has the eth-data interface
    #[test]
    fn test_bastion_data_interface() {
        let topo = expand_default();
        let bastion = &topo.nodes["bastion"];
        let data_iface = bastion.interfaces.iter().find(|i| i.name == "eth-data");
        assert!(data_iface.is_some(), "bastion should have eth-data interface");
        let iface = data_iface.unwrap();
        assert_eq!(iface.mac, "02:4E:57:05:FE:01");
        assert_eq!(iface.peer, "host");
        assert_eq!(iface.role, Some("data".into()));
    }

    /// Verify management addresses
    #[test]
    fn test_management_config() {
        let topo = expand_default();
        assert_eq!(
            topo.management.gateway,
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))
        );
        assert_eq!(topo.management.dns_domain, "themis.local");
        assert!(topo.management.bridge.contains("test-fabric"));
        assert_eq!(
            topo.management.data_gateway,
            IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1))
        );
    }

    /// Verify addressing plan
    #[test]
    fn test_addressing() {
        let topo = expand_default();
        assert_eq!(
            topo.addressing.loopback_cidr.to_string(),
            "10.0.0.0/16"
        );
        assert_eq!(
            topo.addressing.fabric_p2p_cidr.to_string(),
            "172.16.0.0/16"
        );
    }

    /// Verify template and platform fields
    #[test]
    fn test_metadata_fields() {
        let topo = expand_default();
        assert_eq!(topo.template, "clos-3tier");
        assert_eq!(topo.platform, "frr-fedora");
        assert_eq!(topo.name, "test-fabric");
    }

    /// Verify BGP neighbors are populated on a border/spine link
    #[test]
    fn test_bgp_neighbors_on_border_spine() {
        let topo = expand_default();
        // border-1 should have BGP neighbors (at minimum: 2 spines)
        let b1_neighbors = &topo.nodes["border-1"].bgp_neighbors;
        assert!(
            !b1_neighbors.is_empty(),
            "border-1 should have BGP neighbors"
        );
        // spine-1 should have BGP neighbors (borders + leafs)
        let s1_neighbors = &topo.nodes["spine-1"].bgp_neighbors;
        assert!(
            !s1_neighbors.is_empty(),
            "spine-1 should have BGP neighbors"
        );
    }

    /// Verify loopback addresses follow the 10.0.<tier_byte>.<idx> plan
    #[test]
    fn test_loopback_plan() {
        let topo = expand_default();
        // border-1 → 10.0.1.1/32
        let b1_lo = topo.nodes["border-1"].loopback.as_ref().unwrap();
        assert_eq!(b1_lo.to_string(), "10.0.1.1/32");
        // spine-1 → 10.0.2.1/32
        let s1_lo = topo.nodes["spine-1"].loopback.as_ref().unwrap();
        assert_eq!(s1_lo.to_string(), "10.0.2.1/32");
        // leaf-1a → 10.0.3.1/32
        let l1a_lo = topo.nodes["leaf-1a"].loopback.as_ref().unwrap();
        assert_eq!(l1a_lo.to_string(), "10.0.3.1/32");
    }

    /// Smoke test: non-default small parameters
    #[test]
    fn test_small_topo_node_count() {
        let t = Clos3Tier;
        let mut params = Parameters::new();
        params.set("border_count", serde_json::json!(1));
        params.set("spine_count", serde_json::json!(1));
        params.set("rack_count", serde_json::json!(1));
        params.set("servers_per_rack", serde_json::json!(1));
        let topo = t
            .expand("small", "frr-fedora", "eth0", &params)
            .expect("small topo should expand");
        // 5 cp + 1 border + 1 spine + 2 leafs + 1 server = 10
        assert_eq!(topo.nodes.len(), 10);
    }
}
