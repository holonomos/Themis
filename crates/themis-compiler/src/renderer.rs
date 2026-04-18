//! Config renderer — invokes `Platform::generate_config` for every node.
//!
//! For each node in the topology whose role is handled by the platform,
//! this module calls `Platform::generate_config` and collects the resulting
//! `HashMap<PathBuf, String>` into a `NodeConfig`. Nodes whose role is not
//! in `platform.node_roles()` are silently skipped.

use std::collections::HashMap;
use std::path::PathBuf;

use themis_core::{Platform, Result, Topology};

/// Per-node rendered configuration: a map of remote paths to file contents.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node_name: String,
    pub files: HashMap<PathBuf, String>,
}

/// The full set of rendered configurations for a topology.
#[derive(Debug)]
pub struct RenderedConfigs {
    pub nodes: Vec<NodeConfig>,
}

/// Render configs for every node whose role is among `platform.node_roles()`.
///
/// Nodes whose role is not handled by the platform (e.g., servers, bastion) are
/// silently skipped — they have no NOS to configure.
///
/// The returned `RenderedConfigs::nodes` is sorted by node name for
/// deterministic output.
pub fn render(
    topology: &Topology,
    platform: &dyn Platform,
) -> Result<RenderedConfigs> {
    let handled_roles = platform.node_roles();

    let mut node_configs: Vec<NodeConfig> = topology
        .nodes
        .values()
        .filter(|node| handled_roles.contains(&node.role))
        .map(|node| {
            let files = platform.generate_config(node, topology)?;
            Ok(NodeConfig {
                node_name: node.name.clone(),
                files,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    node_configs.sort_by(|a, b| a.node_name.cmp(&b.node_name));

    Ok(RenderedConfigs { nodes: node_configs })
}

/// Convenience: look up a platform by name from `themis_platforms::builtin()` and render.
///
/// Returns `Error::UnknownPlatform` if the name is not registered.
pub fn render_with_builtin_platforms(
    topology: &Topology,
    platform_name: &str,
) -> Result<RenderedConfigs> {
    let platforms = themis_platforms::builtin();
    let platform = platforms
        .iter()
        .find(|p| p.name() == platform_name)
        .ok_or_else(|| themis_core::Error::UnknownPlatform(platform_name.to_string()))?;

    render(topology, platform.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use themis_core::{
        platform::ResourceProfile, Addressing, Bootstrap, Management, Node, Role, Topology,
    };

    /// A mock platform that only handles `Role::Leaf` nodes and returns a
    /// single fixed file per node.
    struct MockPlatform;

    impl Platform for MockPlatform {
        fn name(&self) -> &str {
            "mock"
        }

        fn display_name(&self) -> &str {
            "Mock Platform"
        }

        fn node_roles(&self) -> &[Role] {
            &[Role::Leaf]
        }

        fn generate_config(
            &self,
            node: &Node,
            _topology: &Topology,
        ) -> Result<HashMap<PathBuf, String>> {
            let mut files = HashMap::new();
            files.insert(
                PathBuf::from("/etc/frr/frr.conf"),
                format!("! config for {}", node.name),
            );
            Ok(files)
        }

        fn reload_command(&self) -> &str {
            "systemctl reload frr"
        }

        fn verify_command(&self) -> &str {
            "vtysh -c 'show version'"
        }

        fn resource_profile(&self, _role: Role) -> ResourceProfile {
            ResourceProfile::new(1, 512, 10)
        }
    }

    /// Build a minimal `Topology` with the given nodes.
    fn make_topology(nodes: Vec<Node>) -> Topology {
        let mgmt_cidr: ipnet::IpNet = "10.0.0.0/24".parse().unwrap();
        let data_cidr: ipnet::IpNet = "10.1.0.0/16".parse().unwrap();
        let loopback_cidr: ipnet::IpNet = "10.255.0.0/24".parse().unwrap();
        let fabric_p2p_cidr: ipnet::IpNet = "10.2.0.0/16".parse().unwrap();

        Topology {
            name: "test-fabric".into(),
            template: "clos-3tier".into(),
            platform: "mock".into(),
            wan_interface: None,
            nodes: nodes.into_iter().map(|n| (n.name.clone(), n)).collect(),
            links: vec![],
            management: Management {
                cidr: mgmt_cidr,
                gateway: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                bridge: "br-mgmt".into(),
                data_cidr,
                data_gateway: IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)),
                data_bridge: "br-data".into(),
                dns_domain: "lab.local".into(),
            },
            addressing: Addressing {
                loopback_cidr,
                fabric_p2p_cidr,
            },
        }
    }

    /// Build a minimal `Node` with a given name and role.
    fn make_node(name: &str, role: Role) -> Node {
        Node {
            name: name.into(),
            role,
            nos_type: None,
            asn: None,
            loopback: None,
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
            mgmt_mac: "aa:bb:cc:dd:ee:ff".into(),
            vcpu: 1,
            memory_mb: 512,
            disk_gb: 10,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Dhcp,
        }
    }

    #[test]
    fn render_includes_only_matching_role_nodes() {
        // Two leaf nodes (handled) and one server node (not handled).
        let topology = make_topology(vec![
            make_node("leaf-01", Role::Leaf),
            make_node("leaf-02", Role::Leaf),
            make_node("server-01", Role::Server),
        ]);

        let platform = MockPlatform;
        let result = render(&topology, &platform).expect("render should succeed");

        // Only the two Leaf nodes should appear.
        assert_eq!(result.nodes.len(), 2, "expected 2 leaf NodeConfigs");

        // Verify none of the results is the server.
        for nc in &result.nodes {
            assert_ne!(nc.node_name, "server-01", "server should be skipped");
        }
    }

    #[test]
    fn render_skips_non_matching_role_entirely() {
        // Topology with no leaf nodes — only servers.
        let topology = make_topology(vec![
            make_node("server-01", Role::Server),
            make_node("server-02", Role::Server),
        ]);

        let platform = MockPlatform;
        let result = render(&topology, &platform).expect("render should succeed");

        assert!(
            result.nodes.is_empty(),
            "no nodes should be rendered when no roles match"
        );
    }

    #[test]
    fn render_produces_correct_file_contents() {
        let topology = make_topology(vec![make_node("leaf-01", Role::Leaf)]);

        let platform = MockPlatform;
        let result = render(&topology, &platform).expect("render should succeed");

        assert_eq!(result.nodes.len(), 1);
        let nc = &result.nodes[0];
        assert_eq!(nc.node_name, "leaf-01");

        let conf_path = PathBuf::from("/etc/frr/frr.conf");
        let content = nc.files.get(&conf_path).expect("expected frr.conf");
        assert_eq!(content, "! config for leaf-01");
    }

    #[test]
    fn render_output_is_sorted_by_name() {
        // Insert in reverse order; expect sorted output.
        let topology = make_topology(vec![
            make_node("leaf-03", Role::Leaf),
            make_node("leaf-01", Role::Leaf),
            make_node("leaf-02", Role::Leaf),
        ]);

        let platform = MockPlatform;
        let result = render(&topology, &platform).expect("render should succeed");

        let names: Vec<&str> = result.nodes.iter().map(|n| n.node_name.as_str()).collect();
        assert_eq!(names, vec!["leaf-01", "leaf-02", "leaf-03"]);
    }

    #[test]
    fn render_with_builtin_unknown_platform_returns_error() {
        let topology = make_topology(vec![]);
        let err = render_with_builtin_platforms(&topology, "nonexistent-platform")
            .expect_err("should fail for unknown platform");
        assert!(
            matches!(err, themis_core::Error::UnknownPlatform(_)),
            "expected UnknownPlatform error, got: {err}"
        );
    }
}
