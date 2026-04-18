//! cumulus-vx — Cumulus VX with NVUE startup YAML.
//!
//! Implements the [`Platform`] trait for Cumulus VX nodes. Config generation
//! produces two files per node:
//!
//! - `/etc/nvue.d/startup.yaml` — NVUE declarative config, loaded on boot by
//!   `nv config apply` after startup.
//! - `/etc/udev/rules.d/70-fabric.rules` — deterministic interface naming via
//!   MAC address matching, identical pattern to frr-fedora.
//!
//! Port reference: former Python `platforms/cumulus-vx/nos-driver/driver.py`
//! (Phase 6b per WORK_PLAN.md).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use themis_core::platform::ResourceProfile;
use themis_core::{Node, Platform, Result, Role, Topology};

// ---------------------------------------------------------------------------
// Platform struct
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct CumulusVx;

// ---------------------------------------------------------------------------
// NVUE config struct tree
//
// The NVUE startup YAML is a list with a single element whose top-level key
// is "set". Everything underneath is the declarative config tree. We model
// the well-known subtrees as named structs (for compile-time field safety) and
// fall back to BTreeMap<String, serde_yml::Value> for the interface/neighbor
// dictionaries where keys are dynamic (IP addresses, interface names).
//
// Final shape:
//   - set:
//       system:
//         hostname: <name>
//       interface:
//         lo:     { ip: { address: { "<lo/32>": {} } }, type: loopback }
//         <swp>:  { ip: { address: { "<ip/pfx>": {} } }, type: swp }
//       router:
//         bgp:
//           enable: "on"
//           autonomous-system: <asn>
//           router-id: "<lo_ip>"
//       vrf:
//         default:
//           router:
//             bgp:
//               enable: "on"
//               address-family: ...
//               neighbor: { "<ip>": { remote-as: <asn>, type: numbered, bfd: {...} } }
// ---------------------------------------------------------------------------

/// Top-level list element (`- set: ...`).
#[derive(Debug, Serialize)]
struct NvueDoc {
    set: NvueSet,
}

#[derive(Debug, Serialize)]
struct NvueSet {
    system: NvueSystem,
    interface: BTreeMap<String, serde_yml::Value>,
    router: NvueRouter,
    vrf: NvueVrfMap,
}

#[derive(Debug, Serialize)]
struct NvueSystem {
    hostname: String,
}

// --- Router / global BGP ---

#[derive(Debug, Serialize)]
struct NvueRouter {
    bgp: NvueRouterBgp,
}

#[derive(Debug, Serialize)]
struct NvueRouterBgp {
    enable: String,
    #[serde(rename = "autonomous-system")]
    autonomous_system: u32,
    #[serde(rename = "router-id")]
    router_id: String,
}

// --- VRF map ---

#[derive(Debug, Serialize)]
struct NvueVrfMap {
    default: NvueVrfDefault,
}

#[derive(Debug, Serialize)]
struct NvueVrfDefault {
    router: NvueVrfRouter,
}

#[derive(Debug, Serialize)]
struct NvueVrfRouter {
    bgp: NvueVrfBgp,
}

#[derive(Debug, Serialize)]
struct NvueVrfBgp {
    enable: String,
    #[serde(rename = "address-family")]
    address_family: NvueAddressFamily,
    neighbor: BTreeMap<String, serde_yml::Value>,
}

#[derive(Debug, Serialize)]
struct NvueAddressFamily {
    #[serde(rename = "ipv4-unicast")]
    ipv4_unicast: NvueIpv4Unicast,
}

#[derive(Debug, Serialize)]
struct NvueIpv4Unicast {
    enable: String,
    redistribute: NvueRedistribute,
    #[serde(rename = "maximum-paths")]
    maximum_paths: NvueMaximumPaths,
}

#[derive(Debug, Serialize)]
struct NvueRedistribute {
    connected: NvueConnected,
}

#[derive(Debug, Serialize)]
struct NvueConnected {
    enable: String,
}

#[derive(Debug, Serialize)]
struct NvueMaximumPaths {
    ebgp: u32,
}

// ---------------------------------------------------------------------------
// Helper: build a single interface entry value
// ---------------------------------------------------------------------------

fn interface_entry(ip_cidr: &str, iface_type: &str) -> serde_yml::Value {
    let mut address_map: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    // NVUE wants an empty mapping under each address key.
    address_map.insert(ip_cidr.to_string(), serde_yml::Value::Mapping(Default::default()));

    let mut ip_map: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    ip_map.insert(
        "address".to_string(),
        serde_yml::to_value(&address_map).unwrap_or(serde_yml::Value::Null),
    );

    let mut entry: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    entry.insert(
        "ip".to_string(),
        serde_yml::to_value(&ip_map).unwrap_or(serde_yml::Value::Null),
    );
    entry.insert("type".to_string(), serde_yml::Value::String(iface_type.to_string()));

    serde_yml::to_value(&entry).unwrap_or(serde_yml::Value::Null)
}

// ---------------------------------------------------------------------------
// Helper: build a single BGP neighbor value
// ---------------------------------------------------------------------------

fn neighbor_entry(remote_asn: u32) -> serde_yml::Value {
    // bfd sub-map
    let mut bfd: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    bfd.insert("enable".to_string(), serde_yml::Value::String("on".into()));
    bfd.insert(
        "detect-multiplier".to_string(),
        serde_yml::Value::Number(3u64.into()),
    );
    bfd.insert(
        "min-rx-interval".to_string(),
        serde_yml::Value::Number(300u64.into()),
    );
    bfd.insert(
        "min-tx-interval".to_string(),
        serde_yml::Value::Number(300u64.into()),
    );

    let mut nbr: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    nbr.insert(
        "remote-as".to_string(),
        serde_yml::Value::Number((remote_asn as u64).into()),
    );
    nbr.insert("type".to_string(), serde_yml::Value::String("numbered".into()));
    nbr.insert(
        "bfd".to_string(),
        serde_yml::to_value(&bfd).unwrap_or(serde_yml::Value::Null),
    );

    serde_yml::to_value(&nbr).unwrap_or(serde_yml::Value::Null)
}

// ---------------------------------------------------------------------------
// NVUE YAML generator
// ---------------------------------------------------------------------------

fn generate_nvue_yaml(node: &Node) -> Result<String> {
    // --- router-id: loopback IP without the /32 suffix ---
    let (loopback_cidr_str, router_id) = if let Some(lo) = &node.loopback {
        (
            Some(lo.to_string()),   // e.g. "10.0.0.1/32"
            lo.addr().to_string(),  // e.g. "10.0.0.1"
        )
    } else {
        (None, node.mgmt_ip.to_string())
    };

    // --- interface map ---
    let mut interface_map: BTreeMap<String, serde_yml::Value> = BTreeMap::new();

    // loopback — only when node.loopback.is_some()
    if let Some(lo_cidr) = &loopback_cidr_str {
        interface_map.insert("lo".to_string(), interface_entry(lo_cidr, "loopback"));
    }

    // fabric interfaces
    for iface in &node.interfaces {
        if let Some(ip) = &iface.ip {
            interface_map.insert(
                iface.name.clone(),
                interface_entry(&ip.to_string(), "swp"),
            );
        }
    }

    // --- BGP neighbor map ---
    let mut neighbor_map: BTreeMap<String, serde_yml::Value> = BTreeMap::new();
    for nbr in &node.bgp_neighbors {
        neighbor_map.insert(nbr.ip.to_string(), neighbor_entry(nbr.remote_asn));
    }

    // --- assemble the document ---
    let asn = node.asn.unwrap_or(0);

    let doc = NvueDoc {
        set: NvueSet {
            system: NvueSystem {
                hostname: node.name.clone(),
            },
            interface: interface_map,
            router: NvueRouter {
                bgp: NvueRouterBgp {
                    enable: "on".into(),
                    autonomous_system: asn,
                    router_id,
                },
            },
            vrf: NvueVrfMap {
                default: NvueVrfDefault {
                    router: NvueVrfRouter {
                        bgp: NvueVrfBgp {
                            enable: "on".into(),
                            address_family: NvueAddressFamily {
                                ipv4_unicast: NvueIpv4Unicast {
                                    enable: "on".into(),
                                    redistribute: NvueRedistribute {
                                        connected: NvueConnected {
                                            enable: "on".into(),
                                        },
                                    },
                                    maximum_paths: NvueMaximumPaths { ebgp: 8 },
                                },
                            },
                            neighbor: neighbor_map,
                        },
                    },
                },
            },
        },
    };

    // Wrap in a Vec to emit the leading `- set:` list syntax.
    let docs = vec![doc];

    serde_yml::to_string(&docs).map_err(|e| {
        themis_core::Error::Platform(format!("NVUE YAML serialization failed: {e}"))
    })
}

// ---------------------------------------------------------------------------
// udev rules generator (same pattern as frr-fedora)
// ---------------------------------------------------------------------------

fn generate_udev_rules(node: &Node) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "# Themis \u{2014} udev interface naming rules for {}",
        node.name
    ));
    lines.push("# Maps deterministic MACs to Cumulus VX interface names.".to_string());
    lines.push(String::new());

    for iface in &node.interfaces {
        lines.push(format!(
            r#"SUBSYSTEM=="net", ACTION=="add", ATTR{{address}}=="{}", NAME="{}""#,
            iface.mac.to_lowercase(),
            iface.name,
        ));
    }

    lines.join("\n") + "\n"
}

// ---------------------------------------------------------------------------
// Platform impl
// ---------------------------------------------------------------------------

impl Platform for CumulusVx {
    fn name(&self) -> &str {
        "cumulus-vx"
    }

    fn display_name(&self) -> &str {
        "Cumulus VX"
    }

    fn node_roles(&self) -> &[Role] {
        // NVUE is typical for DC gear — clos and three-tier roles only.
        &[
            Role::Border,
            Role::Spine,
            Role::Leaf,
            Role::Core,
            Role::Distribution,
            Role::Access,
        ]
    }

    fn reload_command(&self) -> &str {
        "nv config apply"
    }

    fn verify_command(&self) -> &str {
        "nv show router bgp --operational"
    }

    fn resource_profile(&self, role: Role) -> ResourceProfile {
        // Mirror frr-fedora profiles: routed → slim, services → fat,
        // bastion → slim, server → medium.
        match role {
            // DC switching roles — all routed, slim profile
            Role::Border
            | Role::Spine
            | Role::Leaf
            | Role::Core
            | Role::Distribution
            | Role::Access => ResourceProfile::new(1, 256, 3),

            // Control-plane VMs (not in node_roles but resource_profile is
            // a general-purpose query so we keep them consistent).
            Role::Bastion => ResourceProfile::new(1, 256, 3),
            Role::Services => ResourceProfile::new(1, 512, 5),

            // Everything else (server, hub, branch, …) — medium profile
            _ => ResourceProfile::new(1, 768, 5),
        }
    }

    fn generate_config(
        &self,
        node: &Node,
        _topology: &Topology,
    ) -> Result<HashMap<PathBuf, String>> {
        let mut configs = HashMap::new();

        // 1. NVUE startup YAML
        let nvue_yaml = generate_nvue_yaml(node)?;
        configs.insert(
            PathBuf::from("/etc/nvue.d/startup.yaml"),
            nvue_yaml,
        );

        // 2. udev interface-naming rules
        let udev_rules = generate_udev_rules(node);
        configs.insert(
            PathBuf::from("/etc/udev/rules.d/70-fabric.rules"),
            udev_rules,
        );

        Ok(configs)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    use themis_core::topology::{BgpNeighbor, Bootstrap, Interface, Node};
    use themis_core::{
        topology::{Addressing, Management, Topology},
        Role,
    };

    /// Build a minimal `Topology` so we can satisfy the signature.
    fn dummy_topology() -> Topology {
        Topology {
            name: "test-lab".into(),
            template: "clos-3tier".into(),
            platform: "cumulus-vx".into(),
            wan_interface: None,
            nodes: HashMap::new(),
            links: vec![],
            management: Management {
                cidr: "192.168.100.0/24".parse().unwrap(),
                gateway: "192.168.100.1".parse().unwrap(),
                bridge: "mgmt-br".into(),
                data_cidr: "10.200.0.0/24".parse().unwrap(),
                data_gateway: "10.200.0.1".parse().unwrap(),
                data_bridge: "data-br".into(),
                dns_domain: "lab.local".into(),
            },
            addressing: Addressing {
                loopback_cidr: "10.0.0.0/24".parse().unwrap(),
                fabric_p2p_cidr: "10.1.0.0/16".parse().unwrap(),
            },
        }
    }

    /// A fabricated spine node with one fabric interface and one BGP neighbor.
    fn spine_node() -> Node {
        Node {
            name: "spine-1".into(),
            role: Role::Spine,
            nos_type: Some("cumulus-vx".into()),
            asn: Some(65100),
            loopback: Some("10.0.0.1/32".parse().unwrap()),
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 100, 10)),
            mgmt_mac: "52:54:00:00:00:01".into(),
            vcpu: 1,
            memory_mb: 256,
            disk_gb: 3,
            interfaces: vec![Interface {
                name: "swp1".into(),
                ip: Some("10.1.0.0/31".parse().unwrap()),
                peer_ip: Some("10.1.0.1".parse().unwrap()),
                subnet: Some("10.1.0.0/31".parse().unwrap()),
                peer: "leaf-1".into(),
                bridge: "br-spine1-leaf1".into(),
                mac: "52:54:00:AA:BB:CC".into(),
                role: Some("fabric".into()),
            }],
            bgp_neighbors: vec![BgpNeighbor {
                ip: "10.1.0.1".parse().unwrap(),
                remote_asn: 65200,
                name: "leaf-1".into(),
                interface: Some("swp1".into()),
            }],
            bootstrap: Bootstrap::Seed,
        }
    }

    #[test]
    fn test_platform_metadata() {
        let p = CumulusVx;
        assert_eq!(p.name(), "cumulus-vx");
        assert_eq!(p.display_name(), "Cumulus VX");
        assert_eq!(p.reload_command(), "nv config apply");
        assert_eq!(p.verify_command(), "nv show router bgp --operational");
        assert!(p.node_roles().contains(&Role::Spine));
        assert!(p.node_roles().contains(&Role::Leaf));
        assert!(!p.node_roles().contains(&Role::Server));
    }

    #[test]
    fn test_resource_profiles() {
        let p = CumulusVx;
        let spine_prof = p.resource_profile(Role::Spine);
        assert_eq!(spine_prof.vcpu, 1);
        assert_eq!(spine_prof.memory_mb, 256);
        assert_eq!(spine_prof.disk_gb, 3);

        let svc_prof = p.resource_profile(Role::Services);
        assert_eq!(svc_prof.memory_mb, 512);
    }

    #[test]
    fn test_generate_config_produces_two_files() {
        let p = CumulusVx;
        let node = spine_node();
        let topo = dummy_topology();
        let configs = p.generate_config(&node, &topo).expect("generate_config failed");

        assert!(
            configs.contains_key(&PathBuf::from("/etc/nvue.d/startup.yaml")),
            "missing startup.yaml"
        );
        assert!(
            configs.contains_key(&PathBuf::from("/etc/udev/rules.d/70-fabric.rules")),
            "missing udev rules"
        );
    }

    #[test]
    fn test_nvue_yaml_contains_required_fields() {
        let p = CumulusVx;
        let node = spine_node();
        let topo = dummy_topology();
        let configs = p.generate_config(&node, &topo).expect("generate_config failed");

        let yaml = &configs[&PathBuf::from("/etc/nvue.d/startup.yaml")];

        // Starts with YAML list marker (the `- set:` wrapper)
        assert!(yaml.trim_start().starts_with('-'), "should be a YAML list");

        // ASN key using NVUE hyphenated form
        assert!(
            yaml.contains("autonomous-system"),
            "missing autonomous-system key\nYAML:\n{yaml}"
        );

        // Interface IP present
        assert!(
            yaml.contains("10.1.0.0/31"),
            "missing swp1 IP 10.1.0.0/31\nYAML:\n{yaml}"
        );

        // Loopback address present
        assert!(
            yaml.contains("10.0.0.1/32"),
            "missing loopback CIDR\nYAML:\n{yaml}"
        );

        // router-id = loopback IP without prefix length
        assert!(
            yaml.contains("router-id"),
            "missing router-id\nYAML:\n{yaml}"
        );

        // BGP neighbor IP present
        assert!(
            yaml.contains("10.1.0.1"),
            "missing BGP neighbor IP\nYAML:\n{yaml}"
        );

        // hostname
        assert!(yaml.contains("spine-1"), "missing hostname\nYAML:\n{yaml}");

        println!("--- Generated NVUE YAML ---\n{yaml}");
    }

    #[test]
    fn test_udev_rules_contain_mac() {
        let p = CumulusVx;
        let node = spine_node();
        let topo = dummy_topology();
        let configs = p.generate_config(&node, &topo).expect("generate_config failed");

        let rules = &configs[&PathBuf::from("/etc/udev/rules.d/70-fabric.rules")];

        // MAC should appear lowercased
        assert!(
            rules.contains("52:54:00:aa:bb:cc"),
            "MAC not lowercased in udev rules\nRules:\n{rules}"
        );
        assert!(
            rules.contains(r#"NAME="swp1""#),
            "interface name missing from udev rules\nRules:\n{rules}"
        );
        assert!(
            rules.contains("spine-1"),
            "node name missing from udev comment\nRules:\n{rules}"
        );
    }

    #[test]
    fn test_no_loopback_node() {
        // When loopback is None, the `lo` interface should be absent from YAML
        // and router-id falls back to mgmt_ip.
        let mut node = spine_node();
        node.loopback = None;

        let p = CumulusVx;
        let topo = dummy_topology();
        let configs = p.generate_config(&node, &topo).expect("generate_config failed");
        let yaml = &configs[&PathBuf::from("/etc/nvue.d/startup.yaml")];

        assert!(
            !yaml.contains("loopback"),
            "loopback should be absent when node.loopback is None\nYAML:\n{yaml}"
        );
        // router-id should fall back to mgmt_ip
        assert!(
            yaml.contains("192.168.100.10"),
            "router-id should be mgmt_ip when no loopback\nYAML:\n{yaml}"
        );
    }
}
