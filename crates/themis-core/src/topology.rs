//! Topology — the fully enumerated description of a fabric.
//!
//! Produced by a `Template`, consumed by the runtime to make VMs exist and by
//! `Platform` implementations to render per-node NOS configuration.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

use crate::role::Role;

/// The complete, rendered topology of one fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    pub name: String,
    pub template: String,
    pub platform: String,
    /// Host WAN interface used for outbound MASQUERADE. None = defer to
    /// host default (no NAT configured). Set by `Template::expand` from the
    /// `wan_interface` argument.
    #[serde(default)]
    pub wan_interface: Option<String>,
    pub nodes: HashMap<String, Node>,
    pub links: Vec<Link>,
    pub management: Management,
    pub addressing: Addressing,
}

/// A single VM in the fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub role: Role,
    pub nos_type: Option<String>,

    // Routing identity (None for non-routed nodes like servers).
    pub asn: Option<u32>,
    pub loopback: Option<IpNet>,

    // Management plane.
    pub mgmt_ip: IpAddr,
    pub mgmt_mac: String,

    // Resource profile.
    pub vcpu: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,

    // Fabric wiring.
    pub interfaces: Vec<Interface>,
    pub bgp_neighbors: Vec<BgpNeighbor>,

    // Identity-injection mode at boot.
    pub bootstrap: Bootstrap,
}

/// A single network interface on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub ip: Option<IpNet>,
    pub peer_ip: Option<IpAddr>,
    pub subnet: Option<IpNet>,
    pub peer: String,
    pub bridge: String,
    pub mac: String,
    /// Optional free-form tag (e.g., "data", "fabric").
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpNeighbor {
    pub ip: IpAddr,
    pub remote_asn: u32,
    pub name: String,
    pub interface: Option<String>,
}

/// How a node receives its identity (hostname, IP, SSH keys) on first boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bootstrap {
    /// cloud-init seed ISO attached as second disk.
    Seed,
    /// DHCP with a MAC reservation in the services VM's dnsmasq.
    Dhcp,
}

/// A point-to-point edge between two nodes, backed by a Linux bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub bridge: String,
    pub a: String,
    pub b: String,
    pub a_ip: Option<IpNet>,
    pub b_ip: Option<IpNet>,
    pub subnet: Option<IpNet>,
    pub a_ifname: String,
    pub b_ifname: String,
    pub a_mac: String,
    pub b_mac: String,
    pub tier: String,
}

/// Management + data-plane substrate for the fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Management {
    pub cidr: IpNet,
    pub gateway: IpAddr,
    pub bridge: String,
    pub data_cidr: IpNet,
    pub data_gateway: IpAddr,
    pub data_bridge: String,
    pub dns_domain: String,
}

/// Address allocation plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Addressing {
    pub loopback_cidr: IpNet,
    pub fabric_p2p_cidr: IpNet,
}
