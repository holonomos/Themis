//! frr-fedora — FRR routing daemon on Fedora.
//! Populated by Phase 6a agent. Port reference: former Python
//! `platforms/frr-fedora/nos-driver/driver.py` + `templates/*.j2`.

use std::collections::HashMap;
use std::path::PathBuf;

use minijinja::{Environment, Value};
use serde::Serialize;

use themis_core::{
    platform::ResourceProfile, Error, Node, Platform, Result, Role, Topology,
};

// ── Embedded templates ────────────────────────────────────────────────────────

const FRR_CONF_TEMPLATE: &str = r#"!
! Themis — FRR configuration for {{ hostname }}
! Generated from topology — DO NOT HAND-EDIT
!
frr defaults datacenter
hostname {{ hostname }}
log syslog informational
service integrated-vtysh-config
!
! --- Loopback ---
interface lo
 ip address {{ loopback }}
!
! --- Fabric interfaces ---
{% for iface in interfaces %}
interface {{ iface.name }}
 ip address {{ iface.ip }}
 no shutdown
!
{% endfor %}
{% if bastion_gateways %}
! --- Static default route (north-south exit via bastion) ---
{% for gw in bastion_gateways %}
ip route 0.0.0.0/0 {{ gw }}
{% endfor %}
!
{% endif %}
{% if server_static_routes %}
! --- Static routes to server loopbacks ---
{% for route in server_static_routes %}
ip route {{ route.prefix }} {{ route.nexthop }}
{% endfor %}
!
{% endif %}
! --- Prefix list: deny management subnet from redistribution ---
ip prefix-list CONNECTED-FILTER seq 5 deny 192.168.0.0/24 le 32
ip prefix-list CONNECTED-FILTER seq 100 permit 0.0.0.0/0 le 32
!
route-map CONNECTED-FILTER permit 10
 match ip address prefix-list CONNECTED-FILTER
exit
!
router bgp {{ asn }}
 bgp router-id {{ router_id }}
 no bgp ebgp-requires-policy
 no bgp default ipv4-unicast
 bgp bestpath as-path multipath-relax
 timers bgp {{ bgp_keepalive }} {{ bgp_holdtime }}
 !
{% for nbr in bgp_neighbors %}
 neighbor {{ nbr.ip }} remote-as {{ nbr.remote_asn }}
 neighbor {{ nbr.ip }} description {{ nbr.name }}
 neighbor {{ nbr.ip }} bfd
{% endfor %}
 !
 address-family ipv4 unicast
  redistribute connected route-map CONNECTED-FILTER
{% if bastion_gateways or server_static_routes %}
  redistribute static
{% endif %}
  maximum-paths 8
{% for nbr in bgp_neighbors %}
  neighbor {{ nbr.ip }} activate
{% if needs_allowas_in %}
  neighbor {{ nbr.ip }} allowas-in 1
{% endif %}
{% if bastion_gateways %}
  neighbor {{ nbr.ip }} default-originate
{% endif %}
{% endfor %}
 exit-address-family
 !
{% if evpn_vtep or is_spine %}
 address-family l2vpn evpn
{% for nbr in bgp_neighbors %}
  neighbor {{ nbr.ip }} activate
{% if is_spine %}
  neighbor {{ nbr.ip }} next-hop-unchanged
{% endif %}
{% endfor %}
{% if evpn_vtep %}
  advertise-all-vni
{% endif %}
 exit-address-family
{% endif %}
exit
!
bfd
{% for nbr in bgp_neighbors %}
 peer {{ nbr.ip }}
  transmit-interval {{ bfd_tx }}
  receive-interval {{ bfd_rx }}
  detect-multiplier {{ bfd_mult }}
 exit
 !
{% endfor %}
exit
!
end
"#;

const DAEMONS_TEMPLATE: &str = r#"# Themis — FRR daemon selection for {{ hostname }}
# Generated from topology — DO NOT HAND-EDIT
#
bgpd=yes
ospfd=no
ospf6d=no
ripd=no
ripngd=no
isisd=no
pimd=no
pim6d=no
ldpd=no
nhrpd=no
eigrpd=no
babeld=no
sharpd=no
pbrd=no
bfdd=yes
fabricd=no
vrrpd=no
pathd=no
zebra=yes
staticd=yes

vtysh_enable=yes

zebra_options="  -A 0.0.0.0 -s 90000000"
bgpd_options="   -A 0.0.0.0"
bfdd_options="   -A 0.0.0.0"
staticd_options="-A 0.0.0.0"
"#;

const VTYSH_CONF_TEMPLATE: &str = r#"! Themis — vtysh configuration for {{ hostname }}
! Generated from topology — DO NOT HAND-EDIT
!
hostname {{ hostname }}
service integrated-vtysh-config
"#;

// ── Context structs ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct IfaceCtx {
    name: String,
    /// IP address in CIDR notation, e.g. "10.1.2.3/30"
    ip: String,
    /// MAC address in lowercase colon notation
    mac: String,
}

#[derive(Debug, Serialize)]
struct NbrCtx {
    ip: String,
    remote_asn: u32,
    name: String,
}

#[derive(Debug, Serialize)]
struct ServerRoute {
    /// Server loopback in CIDR, e.g. "10.0.2.5/32"
    prefix: String,
    /// Next-hop IP (peer_ip on the leaf interface facing the server)
    nexthop: String,
    /// Server node name
    server: String,
}

#[derive(Debug, Serialize)]
struct FrrContext {
    hostname: String,
    role: String,
    asn: u32,
    router_id: String,
    loopback: String,
    interfaces: Vec<IfaceCtx>,
    bgp_neighbors: Vec<NbrCtx>,
    needs_allowas_in: bool,
    evpn_vtep: bool,
    is_spine: bool,
    bastion_gateways: Vec<String>,
    server_static_routes: Vec<ServerRoute>,
    bfd_tx: u32,
    bfd_rx: u32,
    bfd_mult: u32,
    bgp_keepalive: u32,
    bgp_holdtime: u32,
}

// ── Platform impl ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct FrrFedora;

impl Platform for FrrFedora {
    fn name(&self) -> &str {
        "frr-fedora"
    }

    fn display_name(&self) -> &str {
        "FRR on Fedora"
    }

    fn node_roles(&self) -> &[Role] {
        &[
            Role::Border,
            Role::Spine,
            Role::Leaf,
            Role::Core,
            Role::Distribution,
            Role::Access,
            Role::Hub,
            Role::Branch,
        ]
    }

    fn reload_command(&self) -> &str {
        "systemctl reload frr"
    }

    fn verify_command(&self) -> &str {
        "vtysh -c 'show bgp summary'"
    }

    fn resource_profile(&self, role: Role) -> ResourceProfile {
        match role {
            Role::Border
            | Role::Spine
            | Role::Leaf
            | Role::Core
            | Role::Distribution
            | Role::Access
            | Role::Branch => ResourceProfile::new(1, 256, 3),

            Role::Hub => ResourceProfile::new(1, 512, 3),

            Role::Server => ResourceProfile::new(1, 768, 5),

            Role::Bastion => ResourceProfile::new(1, 256, 3),

            Role::Services => ResourceProfile::new(1, 512, 5),

            Role::Orchestrator => ResourceProfile::new(1, 256, 3),

            Role::Telemetry => ResourceProfile::new(2, 2048, 10),

            Role::Registry => ResourceProfile::new(1, 512, 20),
        }
    }

    fn generate_config(
        &self,
        node: &Node,
        topology: &Topology,
    ) -> Result<HashMap<PathBuf, String>> {
        // ── Build template context ────────────────────────────────────────────

        let loopback = node
            .loopback
            .ok_or_else(|| Error::Platform(format!("{}: loopback address is required", node.name)))?;

        let router_id = loopback.addr().to_string();
        let loopback_cidr = loopback.to_string();

        let asn = node
            .asn
            .ok_or_else(|| Error::Platform(format!("{}: ASN is required", node.name)))?;

        // Build IfaceCtx list — skip interfaces that have no IP (e.g. pure-L2).
        let interfaces: Vec<IfaceCtx> = node
            .interfaces
            .iter()
            .filter_map(|iface| {
                iface.ip.map(|ip| IfaceCtx {
                    name: iface.name.clone(),
                    ip: ip.to_string(),
                    mac: iface.mac.to_lowercase(),
                })
            })
            .collect();

        // Build NbrCtx list.
        let bgp_neighbors: Vec<NbrCtx> = node
            .bgp_neighbors
            .iter()
            .map(|nbr| NbrCtx {
                ip: nbr.ip.to_string(),
                remote_asn: nbr.remote_asn,
                name: nbr.name.clone(),
            })
            .collect();

        // needs_allowas_in for Border and Leaf.
        let needs_allowas_in =
            matches!(node.role, Role::Border | Role::Leaf);

        // bastion_gateways: Border nodes only.
        let bastion_gateways: Vec<String> = if node.role == Role::Border {
            node.interfaces
                .iter()
                .filter(|iface| iface.peer == "bastion")
                .filter_map(|iface| iface.peer_ip.map(|ip| ip.to_string()))
                .collect()
        } else {
            vec![]
        };

        // server_static_routes: Leaf nodes only.
        let server_static_routes: Vec<ServerRoute> = if node.role == Role::Leaf {
            node.interfaces
                .iter()
                .filter_map(|iface| {
                    // Look up the peer in the topology; keep only Server-role peers.
                    let peer_node = topology.nodes.get(&iface.peer)?;
                    if peer_node.role != Role::Server {
                        return None;
                    }
                    // Server's loopback as the prefix.
                    let prefix = peer_node.loopback?.to_string();
                    // The peer_ip on this interface is the nexthop toward the server.
                    let nexthop = iface.peer_ip?.to_string();
                    Some(ServerRoute {
                        prefix,
                        nexthop,
                        server: iface.peer.clone(),
                    })
                })
                .collect()
        } else {
            vec![]
        };

        let ctx = FrrContext {
            hostname: node.name.clone(),
            role: node.role.as_str().to_string(),
            asn,
            router_id,
            loopback: loopback_cidr,
            interfaces,
            bgp_neighbors,
            needs_allowas_in,
            evpn_vtep: false,
            is_spine: node.role == Role::Spine,
            bastion_gateways,
            server_static_routes,
            bfd_tx: 300,
            bfd_rx: 300,
            bfd_mult: 3,
            bgp_keepalive: 3,
            bgp_holdtime: 9,
        };

        // ── Render templates ──────────────────────────────────────────────────

        let mut env = Environment::new();
        env.add_template("frr.conf", FRR_CONF_TEMPLATE)
            .map_err(|e| Error::Platform(format!("frr.conf template parse error: {e}")))?;
        env.add_template("daemons", DAEMONS_TEMPLATE)
            .map_err(|e| Error::Platform(format!("daemons template parse error: {e}")))?;
        env.add_template("vtysh.conf", VTYSH_CONF_TEMPLATE)
            .map_err(|e| Error::Platform(format!("vtysh.conf template parse error: {e}")))?;

        let ctx_value = Value::from_serialize(&ctx);

        let frr_conf = env
            .get_template("frr.conf")
            .unwrap()
            .render(ctx_value.clone())
            .map_err(|e| Error::Platform(format!("frr.conf render error: {e}")))?;

        let daemons = env
            .get_template("daemons")
            .unwrap()
            .render(ctx_value.clone())
            .map_err(|e| Error::Platform(format!("daemons render error: {e}")))?;

        let vtysh_conf = env
            .get_template("vtysh.conf")
            .unwrap()
            .render(ctx_value)
            .map_err(|e| Error::Platform(format!("vtysh.conf render error: {e}")))?;

        // ── Build udev rules (programmatic, no template) ──────────────────────

        let udev_rules = build_udev_rules(node);

        // ── Assemble output map ───────────────────────────────────────────────

        let mut configs = HashMap::new();
        configs.insert(PathBuf::from("/etc/frr/frr.conf"), frr_conf);
        configs.insert(PathBuf::from("/etc/frr/daemons"), daemons);
        configs.insert(PathBuf::from("/etc/frr/vtysh.conf"), vtysh_conf);
        configs.insert(
            PathBuf::from("/etc/udev/rules.d/70-fabric.rules"),
            udev_rules,
        );

        Ok(configs)
    }
}

// ── udev helper ───────────────────────────────────────────────────────────────

fn build_udev_rules(node: &Node) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "# Themis — udev interface naming rules for {}",
        node.name
    ));
    lines.push("# Maps deterministic MACs to FRR interface names.".to_string());
    lines.push(String::new());

    for iface in &node.interfaces {
        if iface.mac.is_empty() {
            continue;
        }
        let mac = iface.mac.to_lowercase();
        lines.push(format!(
            r#"SUBSYSTEM=="net", ACTION=="add", ATTR{{address}}=="{mac}", NAME="{name}""#,
            mac = mac,
            name = iface.name,
        ));
    }

    lines.join("\n") + "\n"
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use ipnet::IpNet;

    use themis_core::topology::{Addressing, Bootstrap, Interface, Link, Management};

    use super::*;

    fn make_leaf_topology() -> (Node, Topology) {
        // Fabricate a minimal leaf node with one fabric interface.
        let leaf_loopback: IpNet = "10.0.1.3/32".parse().unwrap();

        let iface = Interface {
            name: "eth-spine1".to_string(),
            ip: Some("10.1.0.2/30".parse().unwrap()),
            peer_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1))),
            subnet: Some("10.1.0.0/30".parse().unwrap()),
            peer: "spine1".to_string(),
            bridge: "br-leaf1-spine1".to_string(),
            mac: "52:54:00:03:01:01".to_string(),
            role: Some("fabric".to_string()),
        };

        let bgp_nbr = themis_core::topology::BgpNeighbor {
            ip: IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)),
            remote_asn: 65000,
            name: "spine1".to_string(),
            interface: Some("eth-spine1".to_string()),
        };

        let leaf = Node {
            name: "leaf1".to_string(),
            role: Role::Leaf,
            nos_type: Some("frr-fedora".to_string()),
            asn: Some(65101),
            loopback: Some(leaf_loopback),
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 11)),
            mgmt_mac: "52:54:00:ff:03:01".to_string(),
            vcpu: 1,
            memory_mb: 256,
            disk_gb: 3,
            interfaces: vec![iface],
            bgp_neighbors: vec![bgp_nbr],
            bootstrap: Bootstrap::Seed,
        };

        // Minimal spine node so the topology lookup works.
        let spine = Node {
            name: "spine1".to_string(),
            role: Role::Spine,
            nos_type: Some("frr-fedora".to_string()),
            asn: Some(65000),
            loopback: Some("10.0.0.1/32".parse().unwrap()),
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 2)),
            mgmt_mac: "52:54:00:ff:02:01".to_string(),
            vcpu: 1,
            memory_mb: 256,
            disk_gb: 3,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Seed,
        };

        let mut nodes = HashMap::new();
        nodes.insert("leaf1".to_string(), leaf.clone());
        nodes.insert("spine1".to_string(), spine);

        let topology = Topology {
            name: "test-fabric".to_string(),
            template: "clos-3tier".to_string(),
            platform: "frr-fedora".to_string(),
            wan_interface: None,
            nodes,
            links: vec![Link {
                bridge: "br-leaf1-spine1".to_string(),
                a: "leaf1".to_string(),
                b: "spine1".to_string(),
                a_ip: Some("10.1.0.2/30".parse().unwrap()),
                b_ip: Some("10.1.0.1/30".parse().unwrap()),
                subnet: Some("10.1.0.0/30".parse().unwrap()),
                a_ifname: "eth-spine1".to_string(),
                b_ifname: "eth-leaf1".to_string(),
                a_mac: "52:54:00:03:01:01".to_string(),
                b_mac: "52:54:00:02:01:01".to_string(),
                tier: "leaf-spine".to_string(),
            }],
            management: Management {
                cidr: "192.168.0.0/24".parse().unwrap(),
                gateway: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
                bridge: "br-mgmt".to_string(),
                data_cidr: "10.100.0.0/24".parse().unwrap(),
                data_gateway: IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1)),
                data_bridge: "br-data".to_string(),
                dns_domain: "fabric.local".to_string(),
            },
            addressing: Addressing {
                loopback_cidr: "10.0.0.0/16".parse().unwrap(),
                fabric_p2p_cidr: "10.1.0.0/16".parse().unwrap(),
            },
        };

        (leaf, topology)
    }

    #[test]
    fn test_generate_config_leaf() {
        let platform = FrrFedora;
        let (node, topology) = make_leaf_topology();

        let configs = platform
            .generate_config(&node, &topology)
            .expect("generate_config should succeed");

        // All four files must be present.
        assert!(
            configs.contains_key(&PathBuf::from("/etc/frr/frr.conf")),
            "missing /etc/frr/frr.conf"
        );
        assert!(
            configs.contains_key(&PathBuf::from("/etc/frr/daemons")),
            "missing /etc/frr/daemons"
        );
        assert!(
            configs.contains_key(&PathBuf::from("/etc/frr/vtysh.conf")),
            "missing /etc/frr/vtysh.conf"
        );
        assert!(
            configs.contains_key(&PathBuf::from("/etc/udev/rules.d/70-fabric.rules")),
            "missing udev rules"
        );

        let frr_conf = &configs[&PathBuf::from("/etc/frr/frr.conf")];

        // Must contain the BGP stanza.
        assert!(
            frr_conf.contains("router bgp"),
            "frr.conf must contain 'router bgp'"
        );

        // Must contain the fabric interface.
        assert!(
            frr_conf.contains("interface eth-spine1"),
            "frr.conf must contain 'interface eth-spine1'"
        );

        // Must contain the correct hostname.
        assert!(
            frr_conf.contains("hostname leaf1"),
            "frr.conf must contain 'hostname leaf1'"
        );

        // Leaf should have allowas-in activated.
        assert!(
            frr_conf.contains("allowas-in"),
            "leaf frr.conf must contain allowas-in"
        );

        // ASN must appear.
        assert!(
            frr_conf.contains("65101"),
            "frr.conf must contain the leaf's ASN"
        );

        // Loopback must appear.
        assert!(
            frr_conf.contains("10.0.1.3/32"),
            "frr.conf must contain the loopback address"
        );

        // udev rules must reference the interface MAC.
        let udev = &configs[&PathBuf::from("/etc/udev/rules.d/70-fabric.rules")];
        assert!(
            udev.contains("52:54:00:03:01:01"),
            "udev rules must contain the interface MAC"
        );
        assert!(
            udev.contains("eth-spine1"),
            "udev rules must contain the interface name"
        );
    }

    #[test]
    fn test_metadata() {
        let platform = FrrFedora;
        assert_eq!(platform.name(), "frr-fedora");
        assert_eq!(platform.display_name(), "FRR on Fedora");
        assert_eq!(platform.reload_command(), "systemctl reload frr");
        assert_eq!(platform.verify_command(), "vtysh -c 'show bgp summary'");
    }

    #[test]
    fn test_resource_profiles() {
        let p = FrrFedora;
        assert_eq!(p.resource_profile(Role::Leaf), ResourceProfile::new(1, 256, 3));
        assert_eq!(p.resource_profile(Role::Hub), ResourceProfile::new(1, 512, 3));
        assert_eq!(p.resource_profile(Role::Telemetry), ResourceProfile::new(2, 2048, 10));
        assert_eq!(p.resource_profile(Role::Registry), ResourceProfile::new(1, 512, 20));
    }

    #[test]
    fn test_node_roles() {
        let p = FrrFedora;
        let roles = p.node_roles();
        assert!(roles.contains(&Role::Border));
        assert!(roles.contains(&Role::Spine));
        assert!(roles.contains(&Role::Leaf));
        assert!(roles.contains(&Role::Branch));
        // Non-routed roles must NOT be present.
        assert!(!roles.contains(&Role::Server));
        assert!(!roles.contains(&Role::Bastion));
    }
}
