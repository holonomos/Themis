//! Inventory builder — per-node runtime artifacts (libvirt XML, cloud-init seeds).
//!
//! This module is a pure-data computation step: it takes a [`Topology`] and
//! produces the complete set of artifacts that the runtime needs to materialise
//! every VM in the fabric.  No disk I/O is performed here; the runtime is
//! responsible for writing the generated content to the appropriate paths.
//!
//! # Usage
//!
//! ```rust,ignore
//! let inventory = build_inventory(&topology, base_image, seed_iso_dir)?;
//! for artifact in &inventory.artifacts {
//!     // hand artifact.domain_xml to the runtime to call virsh define
//!     // if artifact.needs_seed_iso: burn artifact.cloud_init to ISO
//! }
//! ```

use std::path::Path;

use themis_core::{Bootstrap, Topology};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-node artifacts the runtime needs to materialise a VM.
#[derive(Debug, Clone)]
pub struct NodeArtifacts {
    /// The node name (matches `Node::name` and the key in `Topology::nodes`).
    pub node_name: String,

    /// The per-node qcow2 disk path that the domain XML references.
    /// The runtime must clone the golden image to this exact path before
    /// calling `virsh define`.
    pub disk_path: std::path::PathBuf,

    /// The per-node seed ISO path that the domain XML references (only set
    /// when `needs_seed_iso` is true). The runtime must write the ISO to
    /// this exact path before calling `virsh define`.
    pub seed_iso_path: Option<std::path::PathBuf>,

    /// libvirt domain XML suitable for `virsh define`.
    pub domain_xml: String,

    /// cloud-init triplet (user-data, meta-data, network-config).
    ///
    /// Present for **all** nodes — the runtime decides whether to burn an ISO
    /// (only for [`Bootstrap::Seed`] nodes).
    pub cloud_init: themis_runtime::iso::CloudInitContent,

    /// Whether this node requires a seed ISO on its libvirt domain.
    ///
    /// `true`  ↔ `node.bootstrap == Bootstrap::Seed`
    /// `false` ↔ `node.bootstrap == Bootstrap::Dhcp`
    pub needs_seed_iso: bool,
}

/// The complete set of per-node artifacts for one topology.
pub struct Inventory {
    /// Artifacts sorted by [`NodeArtifacts::node_name`] for deterministic output.
    pub artifacts: Vec<NodeArtifacts>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build an inventory from a topology.
///
/// # Parameters
///
/// - `topology`     — the fully rendered topology.
/// - `disk_dir`     — directory under which per-node disk paths are computed
///                    as `<disk_dir>/<node_name>.qcow2`. That exact path is
///                    embedded in the generated libvirt domain XML, so the
///                    runtime MUST clone the golden image to the same path
///                    before calling `virsh define`. See
///                    [`NodeArtifacts::disk_path`] for the concrete path per
///                    node.
/// - `seed_iso_dir` — directory under which per-node seed ISO paths are
///                    computed as `<seed_iso_dir>/<node_name>.iso`. The path
///                    is embedded in the domain XML for Seed nodes; the
///                    runtime is responsible for writing the ISO there.
/// - `ssh_public_key` — OpenSSH pubkey string injected into each node's
///                    cloud-init for password-less SSH access.
///
/// # Returns
///
/// An [`Inventory`] whose `artifacts` vec is sorted by node name.
///
/// # Errors
///
/// This function currently always succeeds (returns `Ok`). The signature
/// returns [`themis_core::Result`] so future validation or enrichment can
/// propagate errors without a breaking API change.
pub fn build_inventory(
    topology: &Topology,
    disk_dir: &Path,
    seed_iso_dir: &Path,
    ssh_public_key: Option<&str>,
) -> themis_core::Result<Inventory> {
    // Pre-compute the services-node dnsmasq config once; only the services
    // node receives it as an extra cloud-init file.
    let dnsmasq_cfg = crate::services_config::generate_dnsmasq_config(topology);

    let mut artifacts: Vec<NodeArtifacts> = topology
        .nodes
        .values()
        .map(|node| {
            // Role-specific extra files written via cloud-init write_files.
            let extra_files: Vec<(String, String)> = if node.role == themis_core::Role::Services {
                vec![(
                    "/etc/dnsmasq.d/themis.conf".to_string(),
                    dnsmasq_cfg.clone(),
                )]
            } else {
                Vec::new()
            };

            // Always generate cloud-init content — the runtime decides whether
            // to materialise it into an ISO.
            let cloud_init = themis_runtime::iso::generate_cloud_init(
                node,
                topology,
                ssh_public_key,
                &extra_files,
            );

            let needs_seed_iso = node.bootstrap == Bootstrap::Seed;

            // Compute the seed ISO path only for Seed nodes; pass None for
            // Dhcp nodes so no cdrom device appears in the domain XML.
            let seed_iso_path = needs_seed_iso
                .then(|| seed_iso_dir.join(format!("{}.iso", node.name)));

            // Per-node disk path. The runtime is responsible for cloning the
            // golden image to this exact path before `virsh define`.
            let disk_path = disk_dir.join(format!("{}.qcow2", node.name));

            let domain_xml = themis_runtime::libvirt::generate_domain_xml(
                node,
                topology,
                &disk_path,
                seed_iso_path.as_deref(),
            );

            NodeArtifacts {
                node_name: node.name.clone(),
                disk_path,
                seed_iso_path,
                domain_xml,
                cloud_init,
                needs_seed_iso,
            }
        })
        .collect();

    // Sort for deterministic output — callers can rely on stable ordering.
    artifacts.sort_by(|a, b| a.node_name.cmp(&b.node_name));

    Ok(Inventory { artifacts })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::Path;

    use themis_core::{
        topology::{Addressing, Bootstrap, Management, Node, Topology},
        role::Role,
    };

    use super::*;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    fn make_management() -> Management {
        Management {
            cidr: "10.0.0.0/24".parse().unwrap(),
            gateway: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            bridge: "br-mgmt".to_owned(),
            data_cidr: "10.1.0.0/24".parse().unwrap(),
            data_gateway: IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)),
            data_bridge: "br-data".to_owned(),
            dns_domain: "lab.local".to_owned(),
        }
    }

    fn make_addressing() -> Addressing {
        Addressing {
            loopback_cidr: "192.0.2.0/24".parse().unwrap(),
            fabric_p2p_cidr: "198.51.100.0/24".parse().unwrap(),
        }
    }

    fn make_node(name: &str, bootstrap: Bootstrap) -> Node {
        Node {
            name: name.to_owned(),
            role: Role::Spine,
            nos_type: Some("frr".to_owned()),
            asn: Some(65000),
            loopback: Some("192.0.2.1/32".parse().unwrap()),
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
            mgmt_mac: "de:ad:be:ef:00:01".to_owned(),
            vcpu: 2,
            memory_mb: 1024,
            disk_gb: 10,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap,
        }
    }

    /// Build a minimal two-node topology: one Seed node + one Dhcp node.
    fn make_two_node_topology() -> Topology {
        let seed_node = make_node("alpha-seed", Bootstrap::Seed);
        let dhcp_node = make_node("beta-dhcp", Bootstrap::Dhcp);

        let mut nodes = HashMap::new();
        nodes.insert(seed_node.name.clone(), seed_node);
        nodes.insert(dhcp_node.name.clone(), dhcp_node);

        Topology {
            name: "test-lab".to_owned(),
            template: "clos-3tier".to_owned(),
            platform: "frr-fedora".to_owned(),
            wan_interface: None,
            nodes,
            links: vec![],
            management: make_management(),
            addressing: make_addressing(),
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn artifact_count_matches_node_count() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        assert_eq!(
            inventory.artifacts.len(),
            2,
            "expected one artifact per node"
        );
    }

    #[test]
    fn artifacts_are_sorted_by_node_name() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        // "alpha-seed" < "beta-dhcp" lexicographically.
        assert_eq!(
            inventory.artifacts[0].node_name, "alpha-seed",
            "first artifact must be alpha-seed"
        );
        assert_eq!(
            inventory.artifacts[1].node_name, "beta-dhcp",
            "second artifact must be beta-dhcp"
        );
    }

    #[test]
    fn seed_node_has_needs_seed_iso_true() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        let seed = inventory
            .artifacts
            .iter()
            .find(|a| a.node_name == "alpha-seed")
            .expect("alpha-seed artifact missing");

        assert!(seed.needs_seed_iso, "Seed bootstrap node must have needs_seed_iso = true");
    }

    #[test]
    fn dhcp_node_has_needs_seed_iso_false() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        let dhcp = inventory
            .artifacts
            .iter()
            .find(|a| a.node_name == "beta-dhcp")
            .expect("beta-dhcp artifact missing");

        assert!(!dhcp.needs_seed_iso, "Dhcp bootstrap node must have needs_seed_iso = false");
    }

    #[test]
    fn domain_xml_contains_node_name() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        for artifact in &inventory.artifacts {
            assert!(
                artifact.domain_xml.contains(&artifact.node_name),
                "domain_xml for {} must contain the node name",
                artifact.node_name
            );
        }
    }

    #[test]
    fn seed_node_domain_xml_contains_iso_path() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        let seed = inventory
            .artifacts
            .iter()
            .find(|a| a.node_name == "alpha-seed")
            .expect("alpha-seed artifact missing");

        // The domain XML for a Seed node must reference the computed ISO path.
        assert!(
            seed.domain_xml.contains("/seeds/alpha-seed.iso"),
            "Seed node domain XML must contain the seed ISO path; got:\n{}",
            seed.domain_xml
        );
        assert!(
            seed.domain_xml.contains("cdrom"),
            "Seed node domain XML must contain a cdrom device"
        );
    }

    #[test]
    fn dhcp_node_domain_xml_has_no_cdrom() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        let dhcp = inventory
            .artifacts
            .iter()
            .find(|a| a.node_name == "beta-dhcp")
            .expect("beta-dhcp artifact missing");

        assert!(
            !dhcp.domain_xml.contains("cdrom"),
            "Dhcp node domain XML must not contain a cdrom device"
        );
    }

    #[test]
    fn all_nodes_have_cloud_init_content() {
        let topology = make_two_node_topology();
        let inventory = build_inventory(
            &topology,
            Path::new("/images/golden.qcow2"),
            Path::new("/seeds"),
            None,
        )
        .expect("build_inventory must not fail");

        for artifact in &inventory.artifacts {
            assert!(
                artifact.cloud_init.user_data.starts_with("#cloud-config"),
                "{}: user_data must start with #cloud-config",
                artifact.node_name
            );
            assert!(
                artifact.cloud_init.meta_data.contains(&artifact.node_name),
                "{}: meta_data must contain the node name",
                artifact.node_name
            );
            assert!(
                artifact.cloud_init.network_config.starts_with("version: 2"),
                "{}: network_config must start with 'version: 2'",
                artifact.node_name
            );
        }
    }
}
