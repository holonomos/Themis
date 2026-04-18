//! cloud-init NoCloud seed ISO building.
//!
//! Generates the three cloud-init payload files (user-data, meta-data,
//! network-config) from a [`themis_core::Node`] and, optionally, materialises
//! them into a bootable ISO via `genisoimage`.
//!
//! # DHCP vs Seed nodes
//!
//! [`generate_cloud_init`] always returns a [`CloudInitContent`] — callers can
//! use the content for record-keeping even when the node boots via DHCP.  Only
//! Seed-mode nodes (Bootstrap::Seed) should have [`build_seed_iso`] called on
//! them; the decision belongs to the caller.
//!
//! # genisoimage
//!
//! The function shells out to `genisoimage` (available on Fedora as
//! `genisoimage` from the `genisoimage` package, and on Debian from
//! `genisoimage`).  The invocation is:
//!
//! ```text
//! genisoimage -output <out.iso> -volid cidata -joliet -rock \
//!             <user-data> <meta-data> <network-config>
//! ```
//!
//! Files are written to a `tempfile`-managed directory, genisoimage is
//! invoked, then the directory is removed.

use std::path::Path;

use tokio::process::Command;

use themis_core::{Node, Topology};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The three cloud-init NoCloud payload strings for one node.
#[derive(Debug, Clone)]
pub struct CloudInitContent {
    /// The `#cloud-config` YAML blob that goes into the `user-data` file.
    pub user_data: String,
    /// The key/value YAML blob that goes into the `meta-data` file.
    pub meta_data: String,
    /// The cloud-init v2 network-config YAML blob.
    pub network_config: String,
}

// ---------------------------------------------------------------------------
// Content generation
// ---------------------------------------------------------------------------

/// Generate the three cloud-init files for `node`.
///
/// - `ssh_public_key`: optional OpenSSH-format public key line (e.g.
///   `ssh-ed25519 AAAA...`). When `None`, only the dev-default password is
///   configured.
/// - `extra_files`: list of `(remote_path, content)` pairs that cloud-init
///   will `write_files`-inject into the guest on first boot. Use this for
///   per-node service config (e.g. `dnsmasq.conf` on the services node).
pub fn generate_cloud_init(
    node: &Node,
    topology: &Topology,
    ssh_public_key: Option<&str>,
    extra_files: &[(String, String)],
) -> CloudInitContent {
    CloudInitContent {
        meta_data: build_meta_data(node),
        user_data: build_user_data(node, ssh_public_key, extra_files),
        network_config: build_network_config(node, topology),
    }
}

// ---------------------------------------------------------------------------
// ISO builder
// ---------------------------------------------------------------------------

/// Write a NoCloud seed ISO at `output_path` containing the three cloud-init
/// payload files.
///
/// Uses `genisoimage` as a subprocess.  The three payload files are staged to
/// a temporary directory, genisoimage is invoked, and the directory is removed
/// regardless of success or failure.
///
/// # Errors
///
/// Returns [`themis_core::Error::Runtime`] if:
/// - the temp directory cannot be created,
/// - payload files cannot be written,
/// - `genisoimage` exits with a non-zero status,
/// - or any other I/O error occurs.
pub async fn build_seed_iso(
    output_path: &Path,
    content: &CloudInitContent,
) -> themis_core::Result<()> {
    // Create a temp dir that lives for the duration of this function.
    let tmp = tempfile::tempdir().map_err(|e| {
        themis_core::Error::Runtime(format!("failed to create temp dir: {e}"))
    })?;

    let user_data_path = tmp.path().join("user-data");
    let meta_data_path = tmp.path().join("meta-data");
    let network_config_path = tmp.path().join("network-config");

    tokio::fs::write(&user_data_path, &content.user_data)
        .await
        .map_err(|e| themis_core::Error::Runtime(format!("failed to write user-data: {e}")))?;
    tokio::fs::write(&meta_data_path, &content.meta_data)
        .await
        .map_err(|e| themis_core::Error::Runtime(format!("failed to write meta-data: {e}")))?;
    tokio::fs::write(&network_config_path, &content.network_config)
        .await
        .map_err(|e| {
            themis_core::Error::Runtime(format!("failed to write network-config: {e}"))
        })?;

    let output = Command::new("genisoimage")
        .arg("-output")
        .arg(output_path)
        .arg("-volid")
        .arg("cidata")
        .arg("-joliet")
        .arg("-rock")
        .arg(&user_data_path)
        .arg(&meta_data_path)
        .arg(&network_config_path)
        .output()
        .await
        .map_err(|e| {
            themis_core::Error::Runtime(format!("failed to spawn genisoimage: {e}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(themis_core::Error::Runtime(format!(
            "genisoimage exited with {}: {}",
            output.status, stderr.trim()
        )));
    }

    // tmp is dropped here, cleaning up the temp directory.
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// ```yaml
/// instance-id: {node.name}
/// local-hostname: {node.name}
/// ```
fn build_meta_data(node: &Node) -> String {
    format!(
        "instance-id: {name}\nlocal-hostname: {name}\n",
        name = node.name
    )
}

/// Produces the `#cloud-config` YAML block.
///
/// If `ssh_public_key` is `Some`, the key is injected into `authorized_keys`;
/// otherwise the password (`themis`) remains the only credential.
///
/// `extra_files` emit a `write_files:` section that cloud-init materialises
/// on first boot — used for per-node service config (e.g. `dnsmasq.conf`
/// on the services node).
fn build_user_data(
    node: &Node,
    ssh_public_key: Option<&str>,
    extra_files: &[(String, String)],
) -> String {
    let mut out = String::new();
    out.push_str("#cloud-config\n");
    out.push_str(&format!("hostname: {}\n", node.name));
    out.push_str("manage_etc_hosts: true\n");

    out.push_str("users:\n");
    out.push_str("  - name: themis\n");
    out.push_str("    sudo: ALL=(ALL) NOPASSWD:ALL\n");
    out.push_str("    shell: /bin/bash\n");
    out.push_str("    lock_passwd: false\n");
    out.push_str("    plain_text_passwd: themis\n");
    if let Some(key) = ssh_public_key {
        out.push_str("    ssh_authorized_keys:\n");
        out.push_str(&format!("      - \"{}\"\n", key.trim().replace('"', "\\\"")));
    }

    out.push_str("ssh_pwauth: true\n");
    out.push_str("chpasswd:\n");
    out.push_str("  expire: false\n");

    if !extra_files.is_empty() {
        out.push_str("write_files:\n");
        for (path, content) in extra_files {
            out.push_str(&format!("  - path: {path}\n"));
            out.push_str("    owner: root:root\n");
            out.push_str("    permissions: '0644'\n");
            out.push_str("    content: |\n");
            for line in content.lines() {
                out.push_str("      ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    out.push_str("runcmd:\n");
    out.push_str("  - systemctl enable --now sshd\n");

    // Role-specific boot actions: services node starts dnsmasq.
    if node.role == themis_core::Role::Services {
        out.push_str("  - systemctl enable --now dnsmasq\n");
    }

    out
}

/// Produces a cloud-init v2 network-config YAML blob.
///
/// The management interface is configured from [`Node::mgmt_ip`] /
/// [`Node::mgmt_mac`] and [`Topology::management`].  Each fabric interface in
/// [`Node::interfaces`] is rendered as a static-address ethernets entry.
fn build_network_config(node: &Node, topology: &Topology) -> String {
    let mgmt = &topology.management;

    // Management interface prefix length extracted from the CIDR.
    let mgmt_prefix = mgmt.cidr.prefix_len();

    // Build the management interface stanza.
    let mut out = format!(
        r#"version: 2
ethernets:
  eth-mgmt:
    match:
      macaddress: {mgmt_mac}
    set-name: eth-mgmt
    addresses:
      - {mgmt_ip}/{mgmt_prefix}
    gateway4: {gateway}
    nameservers:
      addresses:
        - {gateway}
      search:
        - {dns_domain}
"#,
        mgmt_mac = node.mgmt_mac.to_lowercase(),
        mgmt_ip = node.mgmt_ip,
        mgmt_prefix = mgmt_prefix,
        gateway = mgmt.gateway,
        dns_domain = mgmt.dns_domain,
    );

    // Fabric interfaces — only include those that have an IP assigned.
    for iface in &node.interfaces {
        if let Some(ip_net) = &iface.ip {
            out.push_str(&format!(
                r#"  {iface_name}:
    match:
      macaddress: {mac}
    set-name: {iface_name}
    addresses:
      - {ip_cidr}
"#,
                iface_name = iface.name,
                mac = iface.mac.to_lowercase(),
                ip_cidr = ip_net,
            ));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use themis_core::{Addressing, Bootstrap, Interface, Management, Node, Role, Topology};

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers to build minimal test fixtures
    // -----------------------------------------------------------------------

    fn test_management() -> Management {
        Management {
            cidr: "10.0.0.0/24".parse().unwrap(),
            gateway: "10.0.0.1".parse().unwrap(),
            bridge: "br-mgmt".to_string(),
            data_cidr: "192.168.1.0/24".parse().unwrap(),
            data_gateway: "192.168.1.1".parse().unwrap(),
            data_bridge: "br-data".to_string(),
            dns_domain: "themis.local".to_string(),
        }
    }

    fn test_node_minimal() -> Node {
        Node {
            name: "leaf-01".to_string(),
            role: Role::Leaf,
            nos_type: Some("frr-fedora".to_string()),
            asn: Some(65001),
            loopback: Some("10.255.0.1/32".parse().unwrap()),
            mgmt_ip: "10.0.0.10".parse().unwrap(),
            mgmt_mac: "52:54:00:AA:BB:CC".to_string(),
            vcpu: 2,
            memory_mb: 1024,
            disk_gb: 10,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Seed,
        }
    }

    fn test_node_with_interfaces() -> Node {
        let mut node = test_node_minimal();
        node.interfaces = vec![
            Interface {
                name: "eth-fabric0".to_string(),
                ip: Some("10.1.0.0/31".parse().unwrap()),
                peer_ip: Some("10.1.0.1".parse().unwrap()),
                subnet: Some("10.1.0.0/31".parse().unwrap()),
                peer: "spine-01".to_string(),
                bridge: "br-leaf01-spine01".to_string(),
                mac: "52:54:00:11:22:33".to_string(),
                role: Some("fabric".to_string()),
            },
            Interface {
                // Interface without IP — should be omitted from network-config.
                name: "eth-fabric1".to_string(),
                ip: None,
                peer_ip: None,
                subnet: None,
                peer: "spine-02".to_string(),
                bridge: "br-leaf01-spine02".to_string(),
                mac: "52:54:00:44:55:66".to_string(),
                role: Some("fabric".to_string()),
            },
        ];
        node
    }

    fn test_topology() -> Topology {
        Topology {
            name: "test-lab".to_string(),
            template: "clos-3tier".to_string(),
            platform: "frr-fedora".to_string(),
            wan_interface: None,
            nodes: Default::default(),
            links: vec![],
            management: test_management(),
            addressing: Addressing {
                loopback_cidr: "10.255.0.0/16".parse().unwrap(),
                fabric_p2p_cidr: "10.1.0.0/16".parse().unwrap(),
            },
        }
    }

    // -----------------------------------------------------------------------
    // meta-data
    // -----------------------------------------------------------------------

    #[test]
    fn meta_data_contains_instance_id_and_local_hostname() {
        let node = test_node_minimal();
        let content = build_meta_data(&node);

        assert!(
            content.contains("instance-id: leaf-01"),
            "meta-data missing instance-id; got:\n{content}"
        );
        assert!(
            content.contains("local-hostname: leaf-01"),
            "meta-data missing local-hostname; got:\n{content}"
        );
    }

    #[test]
    fn meta_data_uses_node_name_as_both_fields() {
        let mut node = test_node_minimal();
        node.name = "border-02".to_string();
        let content = build_meta_data(&node);

        assert!(content.contains("instance-id: border-02"));
        assert!(content.contains("local-hostname: border-02"));
    }

    // -----------------------------------------------------------------------
    // user-data
    // -----------------------------------------------------------------------

    #[test]
    fn user_data_starts_with_cloud_config_marker() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(
            content.starts_with("#cloud-config\n"),
            "user-data must start with #cloud-config; got:\n{content}"
        );
    }

    #[test]
    fn user_data_sets_hostname() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(
            content.contains("hostname: leaf-01"),
            "user-data missing hostname; got:\n{content}"
        );
    }

    #[test]
    fn user_data_contains_themis_user() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(content.contains("name: themis"), "missing user declaration");
        assert!(
            content.contains("sudo: ALL=(ALL) NOPASSWD:ALL"),
            "missing sudo line"
        );
        assert!(content.contains("shell: /bin/bash"), "missing shell line");
        assert!(
            content.contains("plain_text_passwd: themis"),
            "missing password"
        );
    }

    #[test]
    fn user_data_injects_ssh_pubkey_when_provided() {
        let node = test_node_minimal();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI... lab-demo";
        let content = build_user_data(&node, Some(key), &[]);

        assert!(
            content.contains("ssh_authorized_keys:"),
            "expected ssh_authorized_keys block when key provided"
        );
        assert!(
            content.contains("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI"),
            "expected key content in user-data"
        );
    }

    #[test]
    fn user_data_omits_authorized_keys_when_pubkey_is_none() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(
            !content.contains("ssh_authorized_keys:"),
            "expected no ssh_authorized_keys block when pubkey is None"
        );
    }

    #[test]
    fn user_data_emits_write_files_for_extra_files() {
        let node = test_node_minimal();
        let extras = vec![(
            "/etc/dnsmasq.d/themis.conf".to_string(),
            "# example\ninterface=eth-mgmt\n".to_string(),
        )];
        let content = build_user_data(&node, None, &extras);
        assert!(content.contains("write_files:"));
        assert!(content.contains("path: /etc/dnsmasq.d/themis.conf"));
        assert!(content.contains("interface=eth-mgmt"));
    }

    #[test]
    fn user_data_enables_sshd() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(
            content.contains("systemctl enable --now sshd"),
            "missing sshd runcmd"
        );
    }

    #[test]
    fn user_data_enables_ssh_password_auth() {
        let node = test_node_minimal();
        let content = build_user_data(&node, None, &[]);

        assert!(content.contains("ssh_pwauth: true"), "missing ssh_pwauth");
        assert!(content.contains("expire: false"), "missing chpasswd.expire");
    }

    // -----------------------------------------------------------------------
    // network-config
    // -----------------------------------------------------------------------

    #[test]
    fn network_config_version_2() {
        let node = test_node_minimal();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        assert!(
            content.starts_with("version: 2\n"),
            "network-config must start with 'version: 2'; got:\n{content}"
        );
    }

    #[test]
    fn network_config_has_mgmt_interface() {
        let node = test_node_minimal();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        assert!(content.contains("eth-mgmt:"), "missing eth-mgmt stanza");
        assert!(
            content.contains("set-name: eth-mgmt"),
            "missing set-name for mgmt"
        );
    }

    #[test]
    fn network_config_mgmt_mac_is_lowercase() {
        let node = test_node_minimal(); // mgmt_mac = "52:54:00:AA:BB:CC"
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        assert!(
            content.contains("macaddress: 52:54:00:aa:bb:cc"),
            "mgmt MAC must be lowercased; got:\n{content}"
        );
        assert!(
            !content.contains("AA:BB:CC"),
            "uppercase MAC leaked into output"
        );
    }

    #[test]
    fn network_config_mgmt_address_includes_prefix() {
        let node = test_node_minimal(); // mgmt_ip = 10.0.0.10, cidr = /24
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        assert!(
            content.contains("10.0.0.10/24"),
            "management address should carry /24 prefix; got:\n{content}"
        );
    }

    #[test]
    fn network_config_mgmt_gateway_and_dns() {
        let node = test_node_minimal();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        assert!(
            content.contains("gateway4: 10.0.0.1"),
            "missing gateway4"
        );
        assert!(
            content.contains("themis.local"),
            "missing DNS search domain"
        );
    }

    #[test]
    fn network_config_fabric_interfaces_included() {
        let node = test_node_with_interfaces();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        // eth-fabric0 has an IP — must appear.
        assert!(
            content.contains("eth-fabric0:"),
            "eth-fabric0 (with IP) must appear; got:\n{content}"
        );
        assert!(
            content.contains("macaddress: 52:54:00:11:22:33"),
            "eth-fabric0 MAC must appear lowercased"
        );
        assert!(
            content.contains("10.1.0.0/31"),
            "eth-fabric0 address must appear"
        );
    }

    #[test]
    fn network_config_interfaces_without_ip_omitted() {
        let node = test_node_with_interfaces();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        // eth-fabric1 has no IP — must not appear.
        assert!(
            !content.contains("eth-fabric1:"),
            "eth-fabric1 (no IP) must be omitted; got:\n{content}"
        );
        assert!(
            !content.contains("52:54:00:44:55:66"),
            "MAC for IP-less interface must not appear"
        );
    }

    #[test]
    fn network_config_fabric_mac_is_lowercase() {
        let node = test_node_with_interfaces();
        let topology = test_topology();
        let content = build_network_config(&node, &topology);

        // The mac in the fixture is "52:54:00:11:22:33" — already lower, but
        // we also test it doesn't accidentally get uppercased.
        assert!(
            content.contains("macaddress: 52:54:00:11:22:33"),
            "fabric MAC should remain lowercase"
        );
    }

    // -----------------------------------------------------------------------
    // generate_cloud_init — integration test of the assembler
    // -----------------------------------------------------------------------

    #[test]
    fn generate_cloud_init_returns_all_three_fields() {
        let node = test_node_with_interfaces();
        let topology = test_topology();
        let content = generate_cloud_init(&node, &topology, None, &[]);

        // user_data
        assert!(content.user_data.starts_with("#cloud-config\n"));
        assert!(content.user_data.contains("hostname: leaf-01"));

        // meta_data
        assert!(content.meta_data.contains("instance-id: leaf-01"));

        // network_config
        assert!(content.network_config.starts_with("version: 2\n"));
        assert!(content.network_config.contains("eth-mgmt:"));
    }
}
