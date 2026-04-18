//! three-tier — Core / Distribution / Access enterprise LAN.
//!
//! Classic enterprise topology:
//!   - Core layer: 1–2 redundant core switches (full mesh)
//!   - Distribution layer: 1–4 pairs of switches, each pair MLAG-peered
//!   - Access layer: 1–8 access switches per distribution pair (dual-homed)
//!   - Server layer: 1–8 servers per access switch
//!
//! Control plane: bastion (.2) + services (.3) only.
//! All fabric nodes share ASN 65000 (core), 65100+pair (distribution),
//! 65200+pair*10+access (access).

use std::collections::HashMap;
use std::net::IpAddr;

use ipnet::{IpNet, Ipv4Net};

use themis_core::{
    Addressing, BgpNeighbor, Bootstrap, Error, Interface, Link, Management, Node,
    ParameterDef, ParameterSchema, ParameterType, Parameters, Result, Role, Template, Topology,
};

// ── Public handle ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ThreeTier;

// ── Schema constant ────────────────────────────────────────────────────────────

fn make_schema() -> ParameterSchema {
    ParameterSchema::new()
        .with(
            "core_count",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(2)),
                min: Some(1),
                max: Some(2),
                description: Some("Number of core switches (1–2; 2 = redundant pair)".into()),
            },
        )
        .with(
            "distribution_pair_count",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(2)),
                min: Some(1),
                max: Some(4),
                description: Some("Number of distribution pairs (each pair = 2 MLAG-peered switches)".into()),
            },
        )
        .with(
            "access_per_pair",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(4)),
                min: Some(1),
                max: Some(8),
                description: Some("Access switches per distribution pair".into()),
            },
        )
        .with(
            "servers_per_access",
            ParameterDef {
                ty: ParameterType::Integer,
                default: Some(serde_json::json!(4)),
                min: Some(1),
                max: Some(8),
                description: Some("Server VMs per access switch".into()),
            },
        )
}

// ── Lazy static schema ─────────────────────────────────────────────────────────

use std::sync::OnceLock;

static SCHEMA: OnceLock<ParameterSchema> = OnceLock::new();

fn schema() -> &'static ParameterSchema {
    SCHEMA.get_or_init(make_schema)
}

// ── Template implementation ────────────────────────────────────────────────────

impl Template for ThreeTier {
    fn name(&self) -> &str {
        "three-tier"
    }

    fn display_name(&self) -> &str {
        "Three-Tier Enterprise LAN (Core / Distribution / Access)"
    }

    fn schema(&self) -> &ParameterSchema {
        schema()
    }

    fn expand(
        &self,
        fabric_name: &str,
        platform: &str,
        wan_interface: &str,
        params: &Parameters,
    ) -> Result<Topology> {
        expand_three_tier(fabric_name, platform, wan_interface, params)
    }
}

// ── Main expansion logic ────────────────────────────────────────────────────────

fn expand_three_tier(
    fabric_name: &str,
    platform: &str,
    wan_interface: &str,
    params: &Parameters,
) -> Result<Topology> {
    // ── Read and validate parameters ─────────────────────────────────────────
    let core_count = params
        .get_u32("core_count")
        .unwrap_or(2)
        .clamp(1, 2);

    let dist_pair_count = params
        .get_u32("distribution_pair_count")
        .unwrap_or(2)
        .clamp(1, 4);

    let access_per_pair = params
        .get_u32("access_per_pair")
        .unwrap_or(4)
        .clamp(1, 8);

    let servers_per_access = params
        .get_u32("servers_per_access")
        .unwrap_or(4)
        .clamp(1, 8);

    // ── Address space ─────────────────────────────────────────────────────────
    let loopback_cidr: IpNet = "10.0.0.0/16".parse()?;
    let fabric_p2p_cidr: IpNet = "172.16.0.0/16".parse()?;
    let mgmt_cidr: IpNet = "192.168.0.0/24".parse()?;
    let data_cidr: IpNet = "10.100.0.0/30".parse()?;

    // ── P2P subnet allocator — one /30 per link ────────────────────────────────
    // Start at the first /30 within 172.16.0.0/16: 172.16.0.0/30
    let mut p2p_alloc = P2pAllocator::new("172.16.0.0")?;

    // ── Loopback allocator — one /32 per routed node ──────────────────────────
    // Start at 10.0.0.1
    let mut lo_alloc = LoopbackAllocator::new("10.0.0.1");

    // ── Link counter (for deterministic bridge naming) ─────────────────────────
    let mut link_seq: u32 = 0;

    let mut nodes: HashMap<String, Node> = HashMap::new();
    let mut links: Vec<Link> = Vec::new();

    // ── MAC helpers ───────────────────────────────────────────────────────────
    // fabric MAC: 02:4E:57:<tier_byte>:<peer_counter>:<role_index>
    // mgmt  MAC:  02:4E:57:<tier_byte>:00:<role_index>
    let fabric_mac = |role: Role, peer_ctr: u8, role_idx: u8| -> String {
        format!(
            "02:4e:57:{:02x}:{:02x}:{:02x}",
            role.tier_code(),
            peer_ctr,
            role_idx
        )
    };
    let mgmt_mac = |role: Role, role_idx: u8| -> String {
        format!("02:4e:57:{:02x}:00:{:02x}", role.tier_code(), role_idx)
    };

    // ── Mgmt IP helper ────────────────────────────────────────────────────────
    // 192.168.0.X — returns IpAddr
    let mgmt_ip = |last_octet: u8| -> IpAddr {
        IpAddr::V4(format!("192.168.0.{last_octet}").parse().expect("valid mgmt ip"))
    };

    // ── Bridge name helper ────────────────────────────────────────────────────
    let next_bridge = |seq: &mut u32| -> String {
        let name = format!("br-{fabric_name}-{seq}");
        *seq += 1;
        name
    };

    // ─────────────────────────────────────────────────────────────────────────
    // CONTROL PLANE NODES
    // ─────────────────────────────────────────────────────────────────────────

    // Bastion
    {
        let name = "bastion".to_string();
        nodes.insert(
            name.clone(),
            Node {
                name: name.clone(),
                role: Role::Bastion,
                nos_type: None,
                asn: None,
                loopback: None,
                mgmt_ip: mgmt_ip(2),
                mgmt_mac: mgmt_mac(Role::Bastion, 0x02),
                vcpu: 1,
                memory_mb: 256,
                disk_gb: 3,
                interfaces: vec![Interface {
                    name: "eth-data".to_string(),
                    ip: None,
                    peer_ip: None,
                    subnet: Some(data_cidr),
                    peer: "data-bridge".to_string(),
                    bridge: format!("br-data-{fabric_name}"),
                    mac: mgmt_mac(Role::Bastion, 0x10),
                    role: Some("data".to_string()),
                }],
                bgp_neighbors: vec![],
                bootstrap: Bootstrap::Seed,
            },
        );
    }

    // Services
    {
        let name = "services".to_string();
        nodes.insert(
            name.clone(),
            Node {
                name: name.clone(),
                role: Role::Services,
                nos_type: None,
                asn: None,
                loopback: None,
                mgmt_ip: mgmt_ip(3),
                mgmt_mac: mgmt_mac(Role::Services, 0x03),
                vcpu: 1,
                memory_mb: 256,
                disk_gb: 3,
                interfaces: vec![],
                bgp_neighbors: vec![],
                bootstrap: Bootstrap::Seed,
            },
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CORE SWITCHES
    // ─────────────────────────────────────────────────────────────────────────

    // Build core node names first so we can wire them later.
    let core_names: Vec<String> = (1..=core_count)
        .map(|i| format!("core-{i}"))
        .collect();

    for (idx, name) in core_names.iter().enumerate() {
        let lo_ip = lo_alloc.next();
        let mgmt_octet = 10u8 + idx as u8; // .10, .11

        nodes.insert(
            name.clone(),
            Node {
                name: name.clone(),
                role: Role::Core,
                nos_type: Some(platform.to_string()),
                asn: Some(65000),
                loopback: Some(IpNet::V4(
                    Ipv4Net::new(lo_ip, 32).map_err(|e| Error::Template(e.to_string()))?,
                )),
                mgmt_ip: mgmt_ip(mgmt_octet),
                mgmt_mac: mgmt_mac(Role::Core, mgmt_octet),
                vcpu: 1,
                memory_mb: 256,
                disk_gb: 3,
                interfaces: vec![],
                bgp_neighbors: vec![],
                bootstrap: Bootstrap::Dhcp,
            },
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // DISTRIBUTION SWITCHES
    // ─────────────────────────────────────────────────────────────────────────

    // dist_name[pair_idx][0=a,1=b]
    let mut dist_names: Vec<[String; 2]> = Vec::new();

    for p in 1..=dist_pair_count {
        let name_a = format!("dist-{p}a");
        let name_b = format!("dist-{p}b");
        let pair_idx = (p - 1) as u8;

        for (sub_idx, name) in [&name_a, &name_b].iter().enumerate() {
            let lo_ip = lo_alloc.next();
            // .20, .21 for pair 1; .22, .23 for pair 2; etc.
            let mgmt_octet = 20u8 + pair_idx * 2 + sub_idx as u8;

            nodes.insert(
                name.to_string(),
                Node {
                    name: name.to_string(),
                    role: Role::Distribution,
                    nos_type: Some(platform.to_string()),
                    asn: Some(65100 + (p - 1)),
                    loopback: Some(IpNet::V4(
                        Ipv4Net::new(lo_ip, 32)
                            .map_err(|e| Error::Template(e.to_string()))?,
                    )),
                    mgmt_ip: mgmt_ip(mgmt_octet),
                    mgmt_mac: mgmt_mac(Role::Distribution, mgmt_octet),
                    vcpu: 1,
                    memory_mb: 256,
                    disk_gb: 3,
                    interfaces: vec![],
                    bgp_neighbors: vec![],
                    bootstrap: Bootstrap::Dhcp,
                },
            );
        }

        dist_names.push([name_a, name_b]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ACCESS SWITCHES
    // ─────────────────────────────────────────────────────────────────────────

    // access_name[pair_idx][access_idx]  (0-based internally)
    let mut access_names: Vec<Vec<String>> = Vec::new();

    for p in 1..=dist_pair_count {
        let mut pair_access = Vec::new();
        for n in 1..=access_per_pair {
            let name = format!("access-{p}-{n}");
            let pair_idx = (p - 1) as u8;
            let acc_idx = (n - 1) as u8;
            let lo_ip = lo_alloc.next();
            // .40 onwards; pair 1 gets .40–.47, pair 2 gets .48–.55, etc.
            let mgmt_octet = 40u8 + pair_idx * 8 + acc_idx;

            nodes.insert(
                name.clone(),
                Node {
                    name: name.clone(),
                    role: Role::Access,
                    nos_type: Some(platform.to_string()),
                    asn: Some(65200 + (p - 1) * 10 + (n - 1)),
                    loopback: Some(IpNet::V4(
                        Ipv4Net::new(lo_ip, 32)
                            .map_err(|e| Error::Template(e.to_string()))?,
                    )),
                    mgmt_ip: mgmt_ip(mgmt_octet),
                    mgmt_mac: mgmt_mac(Role::Access, mgmt_octet),
                    vcpu: 1,
                    memory_mb: 256,
                    disk_gb: 3,
                    interfaces: vec![],
                    bgp_neighbors: vec![],
                    bootstrap: Bootstrap::Dhcp,
                },
            );
            pair_access.push(name);
        }
        access_names.push(pair_access);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // SERVER VMs
    // ─────────────────────────────────────────────────────────────────────────

    // srv_name[pair_idx][access_idx][server_idx]  (0-based)
    let mut srv_names: Vec<Vec<Vec<String>>> = Vec::new();

    for p in 1..=dist_pair_count {
        let mut pair_srvs: Vec<Vec<String>> = Vec::new();
        for n in 1..=access_per_pair {
            let mut acc_srvs = Vec::new();
            for s in 1..=servers_per_access {
                let name = format!("srv-{p}-{n}-{s}");
                let pair_idx = (p - 1) as u8;
                let acc_idx = (n - 1) as u8;
                let srv_idx = (s - 1) as u8;
                // .100 onwards — may overflow for large configs, but within
                // realistic defaults (max 4*8*8 = 256, warn-level only)
                let mgmt_octet = 100u8
                    .wrapping_add(pair_idx * access_per_pair as u8 * servers_per_access as u8)
                    .wrapping_add(acc_idx * servers_per_access as u8)
                    .wrapping_add(srv_idx);

                nodes.insert(
                    name.clone(),
                    Node {
                        name: name.clone(),
                        role: Role::Server,
                        nos_type: None,
                        asn: None,
                        loopback: None,
                        mgmt_ip: mgmt_ip(mgmt_octet),
                        mgmt_mac: mgmt_mac(Role::Server, mgmt_octet),
                        vcpu: 1,
                        memory_mb: 768,
                        disk_gb: 5,
                        interfaces: vec![],
                        bgp_neighbors: vec![],
                        bootstrap: Bootstrap::Dhcp,
                    },
                );
                acc_srvs.push(name);
            }
            pair_srvs.push(acc_srvs);
        }
        srv_names.push(pair_srvs);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // WIRING
    // ─────────────────────────────────────────────────────────────────────────

    // Helper: add a P2P link and update both node interface lists.
    //
    // Returns the link, mutating `nodes` to push the Interface on each side.
    let mut peer_ctr: u8 = 0; // monotonic, wraps at 255 — fine for lab scale

    // We need a mutable closure-friendly approach since we can't borrow nodes
    // mutably inside a closure that also reads nodes. We collect link structs
    // first, then apply interface mutations in a second pass.

    struct RawLink {
        bridge: String,
        a: String,
        b: String,
        a_ip: Option<IpNet>,
        b_ip: Option<IpNet>,
        subnet: Option<IpNet>,
        a_ifname: String,
        b_ifname: String,
        a_mac: String,
        b_mac: String,
        tier: String,
    }

    let mut raw_links: Vec<RawLink> = Vec::new();

    // ── Counters for deterministic interface indices ───────────────────────
    // Each node tracks how many fabric interfaces it already has.
    // We use a separate map so we don't fight the borrow checker on `nodes`.
    let mut iface_ctr: HashMap<String, u32> = HashMap::new();

    let connect = |a_name: &str,
                       b_name: &str,
                       tier: &str,
                       p2p: &mut P2pAllocator,
                       seq: &mut u32,
                       pctr: &mut u8,
                       ictr: &mut HashMap<String, u32>|
     -> Result<RawLink> {
        let (a_ip, b_ip, subnet) = p2p.next_p2p()?;

        let a_idx = *ictr.entry(a_name.to_string()).or_insert(0);
        let b_idx = *ictr.entry(b_name.to_string()).or_insert(0);

        let a_ifname = format!("eth{}", a_idx + 1); // eth1, eth2, …
        let b_ifname = format!("eth{}", b_idx + 1);

        *ictr.entry(a_name.to_string()).or_insert(0) += 1;
        *ictr.entry(b_name.to_string()).or_insert(0) += 1;

        let a_role = Role::Core; // placeholder; MACs use peer_ctr to differentiate
        let _ = a_role;

        let a_mac = fabric_mac(
            nodes
                .get(a_name)
                .map(|n| n.role)
                .unwrap_or(Role::Core),
            *pctr,
            a_idx as u8,
        );
        let b_mac = fabric_mac(
            nodes
                .get(b_name)
                .map(|n| n.role)
                .unwrap_or(Role::Access),
            *pctr,
            b_idx as u8,
        );
        *pctr = pctr.wrapping_add(1);

        let bridge = next_bridge(seq);

        Ok(RawLink {
            bridge,
            a: a_name.to_string(),
            b: b_name.to_string(),
            a_ip: Some(a_ip),
            b_ip: Some(b_ip),
            subnet: Some(subnet),
            a_ifname,
            b_ifname,
            a_mac,
            b_mac,
            tier: tier.to_string(),
        })
    };

    // 1. Core ↔ Core full mesh
    for i in 0..core_count as usize {
        for j in (i + 1)..core_count as usize {
            let rl = connect(
                &core_names[i].clone(),
                &core_names[j].clone(),
                "core-mesh",
                &mut p2p_alloc,
                &mut link_seq,
                &mut peer_ctr,
                &mut iface_ctr,
            )?;
            raw_links.push(rl);
        }
    }

    // 2. Core ↔ Distribution (every core to every dist switch)
    for core_name in &core_names.clone() {
        for pair in &dist_names {
            for dist_name in pair {
                let rl = connect(
                    core_name,
                    dist_name,
                    "core-dist",
                    &mut p2p_alloc,
                    &mut link_seq,
                    &mut peer_ctr,
                    &mut iface_ctr,
                )?;
                raw_links.push(rl);
            }
        }
    }

    // 3. Dist-A ↔ Dist-B MLAG peer link per pair
    for pair in &dist_names.clone() {
        let rl = connect(
            &pair[0].clone(),
            &pair[1].clone(),
            "dist-mlag",
            &mut p2p_alloc,
            &mut link_seq,
            &mut peer_ctr,
            &mut iface_ctr,
        )?;
        raw_links.push(rl);
    }

    // 4. Access dual-home: both dist switches ↔ each access switch in the pair
    for (p_idx, pair) in dist_names.iter().enumerate() {
        for acc_name in &access_names[p_idx].clone() {
            for dist_name in pair {
                let rl = connect(
                    dist_name,
                    acc_name,
                    "dist-access",
                    &mut p2p_alloc,
                    &mut link_seq,
                    &mut peer_ctr,
                    &mut iface_ctr,
                )?;
                raw_links.push(rl);
            }
        }
    }

    // 5. Access ↔ Servers
    for (p_idx, pair_acc) in access_names.iter().enumerate() {
        for (a_idx, acc_name) in pair_acc.iter().enumerate() {
            for srv_name in &srv_names[p_idx][a_idx].clone() {
                let rl = connect(
                    acc_name,
                    srv_name,
                    "access-server",
                    &mut p2p_alloc,
                    &mut link_seq,
                    &mut peer_ctr,
                    &mut iface_ctr,
                )?;
                raw_links.push(rl);
            }
        }
    }

    // 6. Bastion ↔ Core-1 (northbound exit)
    {
        let bridge = format!("br-{fabric_name}-wan");
        let bastion_idx = *iface_ctr.entry("bastion".to_string()).or_insert(0);
        let core1_idx = *iface_ctr.entry(core_names[0].clone()).or_insert(0);

        let bastion_mac = fabric_mac(Role::Bastion, peer_ctr, bastion_idx as u8);
        let core1_mac = fabric_mac(Role::Core, peer_ctr, core1_idx as u8);
        let _ = peer_ctr.wrapping_add(1); // consumed; no further links after this

        *iface_ctr.entry("bastion".to_string()).or_insert(0) += 1;
        *iface_ctr.entry(core_names[0].clone()).or_insert(0) += 1;

        raw_links.push(RawLink {
            bridge,
            a: "bastion".to_string(),
            b: core_names[0].clone(),
            a_ip: None,
            b_ip: None,
            subnet: None,
            a_ifname: format!("eth{}", bastion_idx + 1),
            b_ifname: format!("eth{}", core1_idx + 1),
            a_mac: bastion_mac,
            b_mac: core1_mac,
            tier: "wan".to_string(),
        });
    }

    // ── Convert raw links → Link + Interface mutations ───────────────────────
    for rl in raw_links {
        // Push Interface onto each node
        {
            let a_iface = Interface {
                name: rl.a_ifname.clone(),
                ip: rl.a_ip,
                peer_ip: rl.b_ip.map(|n| match n {
                    IpNet::V4(net) => IpAddr::V4(net.addr()),
                    IpNet::V6(net) => IpAddr::V6(net.addr()),
                }),
                subnet: rl.subnet,
                peer: rl.b.clone(),
                bridge: rl.bridge.clone(),
                mac: rl.a_mac.clone(),
                role: Some(rl.tier.clone()),
            };
            if let Some(node) = nodes.get_mut(&rl.a) {
                node.interfaces.push(a_iface);
            }
        }
        {
            let b_iface = Interface {
                name: rl.b_ifname.clone(),
                ip: rl.b_ip,
                peer_ip: rl.a_ip.map(|n| match n {
                    IpNet::V4(net) => IpAddr::V4(net.addr()),
                    IpNet::V6(net) => IpAddr::V6(net.addr()),
                }),
                subnet: rl.subnet,
                peer: rl.a.clone(),
                bridge: rl.bridge.clone(),
                mac: rl.b_mac.clone(),
                role: Some(rl.tier.clone()),
            };
            if let Some(node) = nodes.get_mut(&rl.b) {
                node.interfaces.push(b_iface);
            }
        }

        links.push(Link {
            bridge: rl.bridge,
            a: rl.a,
            b: rl.b,
            a_ip: rl.a_ip,
            b_ip: rl.b_ip,
            subnet: rl.subnet,
            a_ifname: rl.a_ifname,
            b_ifname: rl.b_ifname,
            a_mac: rl.a_mac,
            b_mac: rl.b_mac,
            tier: rl.tier,
        });
    }

    // ── BGP neighbor population ───────────────────────────────────────────────
    // Iterate links; for each routed-to-routed link, add neighbors on both
    // sides. Servers have no BGP.
    for link in &links {
        let a_is_routed = nodes.get(&link.a).map(|n| n.role.is_routed()).unwrap_or(false);
        let b_is_routed = nodes.get(&link.b).map(|n| n.role.is_routed()).unwrap_or(false);

        if !a_is_routed || !b_is_routed {
            continue;
        }

        // Determine IPs and ASNs for neighborship
        let (a_ip_addr, b_ip_addr) = match (link.a_ip, link.b_ip) {
            (Some(a_net), Some(b_net)) => {
                let a_addr = match a_net {
                    IpNet::V4(n) => IpAddr::V4(n.addr()),
                    IpNet::V6(n) => IpAddr::V6(n.addr()),
                };
                let b_addr = match b_net {
                    IpNet::V4(n) => IpAddr::V4(n.addr()),
                    IpNet::V6(n) => IpAddr::V6(n.addr()),
                };
                (a_addr, b_addr)
            }
            _ => continue,
        };

        let a_asn = nodes.get(&link.a).and_then(|n| n.asn).unwrap_or(65000);
        let b_asn = nodes.get(&link.b).and_then(|n| n.asn).unwrap_or(65000);

        // Push to a-side: neighbor is b
        if let Some(node) = nodes.get_mut(&link.a) {
            node.bgp_neighbors.push(BgpNeighbor {
                ip: b_ip_addr,
                remote_asn: b_asn,
                name: link.b.clone(),
                interface: Some(link.a_ifname.clone()),
            });
        }
        // Push to b-side: neighbor is a
        if let Some(node) = nodes.get_mut(&link.b) {
            node.bgp_neighbors.push(BgpNeighbor {
                ip: a_ip_addr,
                remote_asn: a_asn,
                name: link.a.clone(),
                interface: Some(link.b_ifname.clone()),
            });
        }
    }

    // ── Assemble and return ────────────────────────────────────────────────────
    Ok(Topology {
        name: fabric_name.to_string(),
        template: "three-tier".to_string(),
        platform: platform.to_string(),
        wan_interface: Some(wan_interface.to_string()),
        nodes,
        links,
        management: Management {
            cidr: mgmt_cidr,
            gateway: "192.168.0.1".parse()?,
            bridge: format!("br-mgmt-{fabric_name}"),
            data_cidr,
            data_gateway: "10.100.0.1".parse()?,
            data_bridge: format!("br-data-{fabric_name}"),
            dns_domain: "themis.local".to_string(),
        },
        addressing: Addressing {
            loopback_cidr,
            fabric_p2p_cidr,
        },
    })
}

// ── P2P allocator ─────────────────────────────────────────────────────────────

/// Allocates sequential /30 subnets from 172.16.0.0/16.
/// Each /30 yields two host addresses (.1 and .2).
struct P2pAllocator {
    // Current /30 network expressed as a raw u32 (host byte order).
    current: u32,
}

impl P2pAllocator {
    fn new(base: &str) -> Result<Self> {
        let addr: std::net::Ipv4Addr = base.parse()?;
        Ok(Self {
            current: u32::from(addr),
        })
    }

    /// Returns (a_ip/30, b_ip/30, subnet/30).
    fn next_p2p(&mut self) -> Result<(IpNet, IpNet, IpNet)> {
        let base = self.current;
        // /30: base+1 = .a side, base+2 = .b side
        let a_raw = base + 1;
        let b_raw = base + 2;

        let subnet = IpNet::V4(
            Ipv4Net::new(std::net::Ipv4Addr::from(base), 30)
                .map_err(|e| Error::Template(e.to_string()))?,
        );
        let a_ip = IpNet::V4(
            Ipv4Net::new(std::net::Ipv4Addr::from(a_raw), 30)
                .map_err(|e| Error::Template(e.to_string()))?,
        );
        let b_ip = IpNet::V4(
            Ipv4Net::new(std::net::Ipv4Addr::from(b_raw), 30)
                .map_err(|e| Error::Template(e.to_string()))?,
        );

        // Advance to next /30 (stride = 4)
        self.current = base + 4;

        Ok((a_ip, b_ip, subnet))
    }
}

// ── Loopback allocator ─────────────────────────────────────────────────────────

struct LoopbackAllocator {
    current: u32,
}

impl LoopbackAllocator {
    fn new(start: &str) -> Self {
        let addr: std::net::Ipv4Addr = start.parse().expect("valid loopback start IP");
        Self {
            current: u32::from(addr),
        }
    }

    fn next(&mut self) -> std::net::Ipv4Addr {
        let addr = std::net::Ipv4Addr::from(self.current);
        self.current += 1;
        addr
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_params() -> Parameters {
        let mut p = Parameters::new();
        p.set("core_count", serde_json::json!(2));
        p.set("distribution_pair_count", serde_json::json!(2));
        p.set("access_per_pair", serde_json::json!(4));
        p.set("servers_per_access", serde_json::json!(4));
        p
    }

    #[test]
    fn default_node_count() {
        // 2 control + 2 core + 4 dist + 8 access + 32 servers = 48
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");

        assert_eq!(
            topo.nodes.len(),
            48,
            "expected 48 nodes at default params, got {}",
            topo.nodes.len()
        );
    }

    #[test]
    fn default_link_count() {
        // Core mesh:        C(2,2) = 1
        // Core-dist:        2 cores × 2 pairs × 2 switches = 8
        // Dist MLAG:        2 pairs = 2
        // Dist-access:      2 pairs × 4 access × 2 dist = 16
        // Access-server:    2 pairs × 4 access × 4 servers = 32
        // Bastion-core:     1
        // Total: 1 + 8 + 2 + 16 + 32 + 1 = 60
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");

        assert_eq!(
            topo.links.len(),
            60,
            "expected 60 links at default params, got {}",
            topo.links.len()
        );
    }

    #[test]
    fn control_plane_nodes_present() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        assert!(topo.nodes.contains_key("bastion"), "bastion node missing");
        assert!(topo.nodes.contains_key("services"), "services node missing");
    }

    #[test]
    fn bastion_bootstrap_is_seed() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        assert_eq!(topo.nodes["bastion"].bootstrap, Bootstrap::Seed);
        assert_eq!(topo.nodes["services"].bootstrap, Bootstrap::Seed);
    }

    #[test]
    fn core_nodes_have_asn_65000() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        for i in 1..=2 {
            let name = format!("core-{i}");
            assert_eq!(
                topo.nodes[&name].asn,
                Some(65000),
                "{name} should have ASN 65000"
            );
        }
    }

    #[test]
    fn dist_nodes_have_pair_asns() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        for p in 1..=2u32 {
            let expected_asn = 65100 + (p - 1);
            for suffix in ["a", "b"] {
                let name = format!("dist-{p}{suffix}");
                assert_eq!(
                    topo.nodes[&name].asn,
                    Some(expected_asn),
                    "{name} should have ASN {expected_asn}"
                );
            }
        }
    }

    #[test]
    fn servers_have_no_asn() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        assert!(topo.nodes["srv-1-1-1"].asn.is_none(), "servers have no ASN");
    }

    #[test]
    fn server_resources() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        let srv = &topo.nodes["srv-1-1-1"];
        assert_eq!(srv.vcpu, 1);
        assert_eq!(srv.memory_mb, 768);
        assert_eq!(srv.disk_gb, 5);
    }

    #[test]
    fn management_cidr_and_bridge() {
        let topo = expand_three_tier("test-lab", "frr-fedora", "eth0", &default_params())
            .expect("expand should succeed");
        assert_eq!(topo.management.bridge, "br-mgmt-test-lab");
        assert_eq!(topo.management.data_bridge, "br-data-test-lab");
        assert_eq!(topo.management.dns_domain, "themis.local");
    }

    #[test]
    fn minimal_params_single_core_single_pair() {
        let mut p = Parameters::new();
        p.set("core_count", serde_json::json!(1));
        p.set("distribution_pair_count", serde_json::json!(1));
        p.set("access_per_pair", serde_json::json!(1));
        p.set("servers_per_access", serde_json::json!(1));
        // 2 control + 1 core + 2 dist + 1 access + 1 server = 7 nodes
        let topo = expand_three_tier("mini-lab", "frr-fedora", "eth0", &p)
            .expect("expand should succeed for minimal params");
        assert_eq!(topo.nodes.len(), 7);
    }

    #[test]
    fn template_name() {
        let t = ThreeTier;
        assert_eq!(t.name(), "three-tier");
    }
}
