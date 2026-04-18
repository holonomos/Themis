//! libvirt driver — `virsh` shell-outs and domain XML generation.
//!
//! All async functions shell out to `virsh` on the PATH. The caller is
//! responsible for ensuring that the executing user has the correct libvirt
//! group membership and that `virsh` can reach the system QEMU driver
//! (`qemu:///system`).
//!
//! `generate_domain_xml` builds a KVM domain XML string from a
//! `themis_core::Node` suitable for `virsh define`. The caller is responsible
//! for provisioning per-node disk clones before passing paths to this function.

use std::fmt::Write as _;
use std::path::Path;

use tokio::process::Command;
use tracing::{debug, instrument, warn};

use themis_core::{Node, Result, Topology};
use themis_core::Error;

// ── Domain state ─────────────────────────────────────────────────────────────

/// The running state of a libvirt domain, as reported by `virsh domstate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainState {
    Running,
    Paused,
    Shutoff,
    Crashed,
    /// Any state string not covered by the named variants above.
    Other(String),
}

impl DomainState {
    fn from_str(s: &str) -> Self {
        match s.trim() {
            "running" => DomainState::Running,
            "paused" => DomainState::Paused,
            "shut off" | "shutoff" => DomainState::Shutoff,
            "crashed" => DomainState::Crashed,
            other => DomainState::Other(other.to_owned()),
        }
    }
}

impl std::fmt::Display for DomainState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DomainState::Running => f.write_str("running"),
            DomainState::Paused => f.write_str("paused"),
            DomainState::Shutoff => f.write_str("shut off"),
            DomainState::Crashed => f.write_str("crashed"),
            DomainState::Other(s) => f.write_str(s),
        }
    }
}

/// A row from `virsh list --all`.
#[derive(Debug, Clone)]
pub struct DomainSummary {
    pub name: String,
    pub state: DomainState,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Run `virsh <args>` and return stdout on success.
///
/// On non-zero exit the raw stderr is wrapped in `Error::Runtime`.
async fn virsh(args: &[&str]) -> Result<String> {
    debug!(cmd = %args.join(" "), "virsh");

    let output = Command::new("virsh")
        .args(args)
        .output()
        .await
        .map_err(|e| Error::Runtime(format!("failed to spawn virsh: {e}")))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(Error::Runtime(format!(
            "virsh {} exited with {}: {}{}",
            args.join(" "),
            output.status,
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" / {}", stdout.trim())
            },
        )))
    }
}

// ── Public lifecycle functions ────────────────────────────────────────────────

/// Define a domain from an XML string (`virsh define`).
///
/// The XML is written to a temp file and passed with `--file`; libvirt
/// requires a filesystem path, not stdin.
#[instrument(skip(xml), fields(domain = name))]
pub async fn define_domain(name: &str, xml: &str) -> Result<()> {
    // Write to a named temp file so virsh can open it by path.
    let tmp = tempfile_path(name);
    tokio::fs::write(&tmp, xml)
        .await
        .map_err(|e| Error::Runtime(format!("could not write temp XML for {name}: {e}")))?;

    let result = virsh(&["define", "--file", &tmp]).await;

    // Best-effort cleanup — ignore any removal error.
    let _ = tokio::fs::remove_file(&tmp).await;

    result.map(|_| ())
}

/// Undefine (permanently remove) a domain (`virsh undefine`).
///
/// Passes `--nvram` so that UEFI variable stores are also removed when present;
/// libvirt ignores the flag for non-UEFI domains.
#[instrument(fields(domain = name))]
pub async fn undefine_domain(name: &str) -> Result<()> {
    virsh(&["undefine", name, "--nvram"])
        .await
        .map(|_| ())
}

/// Start a defined domain (`virsh start`).
#[instrument(fields(domain = name))]
pub async fn start_domain(name: &str) -> Result<()> {
    virsh(&["start", name]).await.map(|_| ())
}

/// Fabric-scoped libvirt domain name. Two Themis labs running side-by-side
/// each have their own nodes named e.g. `spine-1`; prefixing with the fabric
/// name gives libvirt globally unique domain names.
pub fn domain_name(fabric_name: &str, node_name: &str) -> String {
    format!("{}-{}", fabric_name, node_name)
}

/// Create a qcow2 overlay that uses `base` as a read-only backing store.
///
/// The resulting `dest` file shares pages with `base` (copy-on-write), so
/// spinning up N VMs from the same golden image costs only the per-VM
/// divergence, not N full copies. This is the mechanism KSM further
/// compounds at the memory layer.
///
/// Shells out to `qemu-img create -f qcow2 -b <base> -F qcow2 <dest>`.
#[instrument(skip_all, fields(base = %base.display(), dest = %dest.display()))]
pub async fn clone_golden_image(
    base: &std::path::Path,
    dest: &std::path::Path,
) -> Result<()> {
    let base_str = base
        .to_str()
        .ok_or_else(|| Error::Runtime(format!("base path is not valid UTF-8: {}", base.display())))?;
    let dest_str = dest
        .to_str()
        .ok_or_else(|| Error::Runtime(format!("dest path is not valid UTF-8: {}", dest.display())))?;

    let output = Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-b", base_str, "-F", "qcow2", dest_str])
        .output()
        .await
        .map_err(|e| Error::Runtime(format!("failed to spawn qemu-img: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Runtime(format!(
            "qemu-img create for {dest_str} failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

/// Forcibly stop a running domain (`virsh destroy`).
///
/// `virsh destroy` sends SIGKILL to QEMU — equivalent to pulling the power
/// cord. Use this for teardown, not graceful shutdown.
#[instrument(fields(domain = name))]
pub async fn destroy_domain(name: &str) -> Result<()> {
    virsh(&["destroy", name]).await.map(|_| ())
}

/// Return `true` if a domain with `name` is known to libvirt.
///
/// Uses `virsh dominfo` — returns `false` on any error (including "domain not
/// found"), and `true` on success.
#[instrument(fields(domain = name))]
pub async fn domain_exists(name: &str) -> Result<bool> {
    match virsh(&["dominfo", name]).await {
        Ok(_) => Ok(true),
        Err(Error::Runtime(ref msg)) if msg.contains("failed to get domain") || msg.contains("Domain not found") => {
            Ok(false)
        }
        // virsh dominfo exits non-zero with a recognizable message when the
        // domain is unknown; treat any Runtime error as "not found" so that
        // callers don't have to parse virsh stderr.
        Err(Error::Runtime(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Return the current state of a domain (`virsh domstate`).
#[instrument(fields(domain = name))]
pub async fn domain_state(name: &str) -> Result<DomainState> {
    let out = virsh(&["domstate", name]).await?;
    Ok(DomainState::from_str(out.trim()))
}

/// List all domains known to libvirt (`virsh list --all`).
///
/// Parses the tabular output produced by `virsh list --all`.  The format is:
/// ```text
///  Id   Name          State
/// -----------------------------------------------
///  1    spine-01      running
///  -    leaf-01       shut off
/// ```
///
/// Lines that cannot be parsed are logged at WARN and skipped.
#[instrument]
pub async fn list_domains() -> Result<Vec<DomainSummary>> {
    let out = virsh(&["list", "--all"]).await?;
    let domains = parse_virsh_list(&out);
    Ok(domains)
}

/// Parse the `virsh list --all` tabular output into `DomainSummary` values.
///
/// Exported so that unit tests can exercise it without spawning a process.
pub(crate) fn parse_virsh_list(output: &str) -> Vec<DomainSummary> {
    let mut results = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        // Skip the header line (starts with "Id") and separator lines (all dashes).
        if trimmed.is_empty()
            || trimmed.starts_with("Id")
            || trimmed.chars().all(|c| c == '-')
        {
            continue;
        }

        // Each data row: <id> <name> <state...>
        // The id column is either a number or "-".
        // State can be multiple words ("shut off"), so join everything from
        // token index 2 onward with a single space.
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 3 {
            warn!(%line, "virsh list: fewer than 3 columns");
            continue;
        }

        let _id = tokens[0];
        let name = tokens[1].to_owned();
        let state_raw = tokens[2..].join(" ");
        let state = DomainState::from_str(&state_raw);

        results.push(DomainSummary { name, state });
    }

    results
}

// ── XML generation ────────────────────────────────────────────────────────────

/// Generate a libvirt KVM domain XML string for `node`.
///
/// # Parameters
/// - `node`            — the node whose VM this XML describes.
/// - `topology`        — used to look up the management bridge name.
/// - `base_image_path` — path of the per-node disk image (qcow2). The caller
///                       is responsible for cloning the golden image before
///                       calling this function.
/// - `seed_iso_path`   — if `Some`, a cloud-init seed ISO is attached as a
///                       read-only SATA CDROM.
///
/// The returned string is valid libvirt domain XML targeting the `qemu:///system`
/// driver with a KVM hypervisor, x86_64 architecture, and virtio NICs.
pub fn generate_domain_xml(
    node: &Node,
    topology: &Topology,
    base_image_path: &Path,
    seed_iso_path: Option<&Path>,
) -> String {
    let mut xml = String::with_capacity(4096);

    // ── Domain open ───────────────────────────────────────────────────────────
    writeln!(xml, "<domain type='kvm'>").unwrap();
    writeln!(
        xml,
        "  <name>{}</name>",
        xml_escape(&domain_name(&topology.name, &node.name))
    )
    .unwrap();
    writeln!(xml, "  <memory unit='MiB'>{}</memory>", node.memory_mb).unwrap();
    writeln!(xml, "  <vcpu>{}</vcpu>", node.vcpu).unwrap();

    // ── OS boot ───────────────────────────────────────────────────────────────
    writeln!(xml, "  <os>").unwrap();
    writeln!(xml, "    <type arch='x86_64' machine='pc'>hvm</type>").unwrap();
    writeln!(xml, "    <boot dev='hd'/>").unwrap();
    writeln!(xml, "  </os>").unwrap();

    // ── Platform features ─────────────────────────────────────────────────────
    writeln!(xml, "  <features>").unwrap();
    writeln!(xml, "    <acpi/>").unwrap();
    writeln!(xml, "    <apic/>").unwrap();
    writeln!(xml, "  </features>").unwrap();

    // ── CPU model ─────────────────────────────────────────────────────────────
    // host-passthrough exposes the full host CPU to the guest; required for
    // routing software (FRR) that uses advanced vector instructions.
    writeln!(xml, "  <cpu mode='host-passthrough'/>").unwrap();

    // ── Devices ───────────────────────────────────────────────────────────────
    writeln!(xml, "  <devices>").unwrap();
    writeln!(
        xml,
        "    <emulator>/usr/bin/qemu-system-x86_64</emulator>"
    )
    .unwrap();

    // Primary disk — caller-provisioned qcow2 clone.
    let disk_path = base_image_path.to_string_lossy();
    writeln!(xml, "    <disk type='file' device='disk'>").unwrap();
    writeln!(xml, "      <driver name='qemu' type='qcow2'/>").unwrap();
    writeln!(xml, "      <source file='{}'/>", xml_escape(&disk_path)).unwrap();
    writeln!(xml, "      <target dev='vda' bus='virtio'/>").unwrap();
    writeln!(xml, "    </disk>").unwrap();

    // Optional cloud-init seed ISO (attached only when Bootstrap::Seed).
    if let Some(iso) = seed_iso_path {
        let iso_path = iso.to_string_lossy();
        writeln!(xml, "    <disk type='file' device='cdrom'>").unwrap();
        writeln!(xml, "      <driver name='qemu' type='raw'/>").unwrap();
        writeln!(xml, "      <source file='{}'/>", xml_escape(&iso_path)).unwrap();
        writeln!(xml, "      <target dev='sda' bus='sata'/>").unwrap();
        writeln!(xml, "      <readonly/>").unwrap();
        writeln!(xml, "    </disk>").unwrap();
    }

    // Management NIC — bridges to the fabric management network.
    writeln!(xml, "    <interface type='bridge'>").unwrap();
    writeln!(
        xml,
        "      <source bridge='{}'/>",
        xml_escape(&topology.management.bridge)
    )
    .unwrap();
    writeln!(
        xml,
        "      <mac address='{}'/>",
        xml_escape(&node.mgmt_mac)
    )
    .unwrap();
    writeln!(xml, "      <model type='virtio'/>").unwrap();
    writeln!(xml, "    </interface>").unwrap();

    // Fabric NICs — one per Interface, in declaration order.
    for iface in &node.interfaces {
        writeln!(xml, "    <interface type='bridge'>").unwrap();
        writeln!(
            xml,
            "      <source bridge='{}'/>",
            xml_escape(&iface.bridge)
        )
        .unwrap();
        writeln!(xml, "      <mac address='{}'/>", xml_escape(&iface.mac)).unwrap();
        writeln!(xml, "      <model type='virtio'/>").unwrap();
        writeln!(xml, "    </interface>").unwrap();
    }

    // Serial console — accessible via `virsh console <name>`.
    writeln!(xml, "    <serial type='pty'/>").unwrap();
    writeln!(xml, "    <console type='pty'/>").unwrap();

    writeln!(xml, "  </devices>").unwrap();
    writeln!(xml, "</domain>").unwrap();

    xml
}

// ── Internal utilities ────────────────────────────────────────────────────────

/// Escape the five XML predefined entities in attribute values and text nodes.
///
/// libvirt itself validates the XML it receives; this escape is a safety
/// measure for names or paths that might contain characters from the reserved
/// set (rare in practice, but correct regardless).
fn xml_escape(s: &str) -> std::borrow::Cow<'_, str> {
    // Fast path: most strings need no escaping.
    if !s.contains(['&', '<', '>', '"', '\'']) {
        return std::borrow::Cow::Borrowed(s);
    }

    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Build a temporary file path for domain XML files.
///
/// Uses `/tmp` rather than `std::env::temp_dir()` so that the path is always
/// writable by the libvirt-group user regardless of XDG overrides.
fn tempfile_path(domain_name: &str) -> String {
    format!("/tmp/themis-domain-{}.xml", domain_name)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    
    use std::collections::HashMap;
    

    use themis_core::{
        topology::{Addressing, Bootstrap, Interface, Management, Node, Topology},
        role::Role,
    };

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

    fn make_topology(nodes: HashMap<String, Node>) -> Topology {
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

    fn make_node(name: &str, interfaces: Vec<Interface>) -> Node {
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
            interfaces,
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Seed,
        }
    }

    fn make_interface(name: &str, bridge: &str, mac: &str) -> Interface {
        Interface {
            name: name.to_owned(),
            ip: None,
            peer_ip: None,
            subnet: None,
            peer: "peer-node".to_owned(),
            bridge: bridge.to_owned(),
            mac: mac.to_owned(),
            role: Some("fabric".to_owned()),
        }
    }

    // ── generate_domain_xml ───────────────────────────────────────────────────

    #[test]
    fn xml_contains_fabric_prefixed_node_name() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        // Domain names are fabric-scoped: "<fabric_name>-<node_name>".
        assert!(
            xml.contains("<name>test-lab-spine-01</name>"),
            "missing fabric-prefixed <name> element; XML was:\n{xml}"
        );
    }

    #[test]
    fn domain_name_prefixes_with_fabric() {
        assert_eq!(domain_name("my-lab", "spine-01"), "my-lab-spine-01");
    }

    #[test]
    fn xml_contains_memory_and_vcpu() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        assert!(xml.contains("<memory unit='MiB'>1024</memory>"));
        assert!(xml.contains("<vcpu>2</vcpu>"));
    }

    #[test]
    fn xml_contains_mgmt_nic_bridge_and_mac() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        // Management bridge from topology.
        assert!(xml.contains("br-mgmt"), "missing mgmt bridge");
        // Management MAC from node.
        assert!(xml.contains("de:ad:be:ef:00:01"), "missing mgmt MAC");
    }

    #[test]
    fn xml_no_seed_iso_when_none() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        assert!(!xml.contains("cdrom"), "unexpected cdrom device when seed_iso_path is None");
    }

    #[test]
    fn xml_seed_iso_present_when_some() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(
            &node,
            &topo,
            Path::new("/images/spine-01.qcow2"),
            Some(Path::new("/seeds/spine-01.iso")),
        );
        assert!(xml.contains("cdrom"), "missing cdrom device");
        assert!(xml.contains("/seeds/spine-01.iso"), "missing seed ISO path");
        assert!(xml.contains("<readonly/>"), "cdrom must be read-only");
        assert!(xml.contains("bus='sata'"), "seed ISO must use SATA bus");
    }

    #[test]
    fn xml_interface_count_matches_node_interfaces() {
        // Three fabric interfaces.
        let ifaces = vec![
            make_interface("eth1", "br-fabric-0", "aa:bb:cc:00:00:01"),
            make_interface("eth2", "br-fabric-1", "aa:bb:cc:00:00:02"),
            make_interface("eth3", "br-fabric-2", "aa:bb:cc:00:00:03"),
        ];
        let node = make_node("leaf-01", ifaces);
        let topo = make_topology(HashMap::from([("leaf-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/leaf-01.qcow2"), None);

        // Total interface elements = 1 mgmt + 3 fabric = 4.
        let count = xml.matches("<interface type='bridge'>").count();
        assert_eq!(count, 4, "expected 4 interface elements (1 mgmt + 3 fabric), got {count}");
    }

    #[test]
    fn xml_fabric_bridges_and_macs_present() {
        let ifaces = vec![
            make_interface("eth1", "br-spine-leaf-0", "aa:bb:cc:00:01:01"),
            make_interface("eth2", "br-spine-leaf-1", "aa:bb:cc:00:01:02"),
        ];
        let node = make_node("spine-02", ifaces);
        let topo = make_topology(HashMap::from([("spine-02".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-02.qcow2"), None);

        assert!(xml.contains("br-spine-leaf-0"), "missing first fabric bridge");
        assert!(xml.contains("br-spine-leaf-1"), "missing second fabric bridge");
        assert!(xml.contains("aa:bb:cc:00:01:01"), "missing first fabric MAC");
        assert!(xml.contains("aa:bb:cc:00:01:02"), "missing second fabric MAC");
    }

    #[test]
    fn xml_zero_fabric_interfaces() {
        let node = make_node("server-01", vec![]);
        let topo = make_topology(HashMap::from([("server-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/server-01.qcow2"), None);

        // Only the management NIC.
        let count = xml.matches("<interface type='bridge'>").count();
        assert_eq!(count, 1, "expected 1 interface (mgmt only), got {count}");
    }

    #[test]
    fn xml_disk_path_is_present() {
        let node = make_node("leaf-02", vec![]);
        let topo = make_topology(HashMap::from([("leaf-02".into(), node.clone())]));
        let xml = generate_domain_xml(
            &node,
            &topo,
            Path::new("/var/lib/themis/leaf-02.qcow2"),
            None,
        );
        assert!(xml.contains("/var/lib/themis/leaf-02.qcow2"), "missing disk path");
        assert!(xml.contains("type='qcow2'"), "missing qcow2 driver type");
        assert!(xml.contains("bus='virtio'"), "disk must use virtio bus");
    }

    #[test]
    fn xml_kvm_domain_type() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        assert!(xml.starts_with("<domain type='kvm'>"), "must be a KVM domain");
    }

    #[test]
    fn xml_has_console_and_serial() {
        let node = make_node("spine-01", vec![]);
        let topo = make_topology(HashMap::from([("spine-01".into(), node.clone())]));
        let xml = generate_domain_xml(&node, &topo, Path::new("/images/spine-01.qcow2"), None);
        assert!(xml.contains("<serial type='pty'/>"), "missing serial pty");
        assert!(xml.contains("<console type='pty'/>"), "missing console pty");
    }

    // ── parse_virsh_list ──────────────────────────────────────────────────────

    #[test]
    fn parse_virsh_list_typical_output() {
        let output = "\
 Id   Name          State
-----------------------------------------------
 1    spine-01      running
 2    leaf-01       running
 -    leaf-02       shut off
";
        let domains = parse_virsh_list(output);
        assert_eq!(domains.len(), 3);
        assert_eq!(domains[0].name, "spine-01");
        assert_eq!(domains[0].state, DomainState::Running);
        assert_eq!(domains[1].name, "leaf-01");
        assert_eq!(domains[1].state, DomainState::Running);
        assert_eq!(domains[2].name, "leaf-02");
        assert_eq!(domains[2].state, DomainState::Shutoff);
    }

    #[test]
    fn parse_virsh_list_empty_output() {
        let output = "\
 Id   Name          State
-----------------------------------------------
";
        let domains = parse_virsh_list(output);
        assert!(domains.is_empty(), "expected no domains from empty table");
    }

    #[test]
    fn parse_virsh_list_crashed_domain() {
        let output = "\
 Id   Name     State
---------------------
 -    chaos    crashed
";
        let domains = parse_virsh_list(output);
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].state, DomainState::Crashed);
    }

    #[test]
    fn domain_state_from_str_round_trips() {
        assert_eq!(DomainState::from_str("running"), DomainState::Running);
        assert_eq!(DomainState::from_str("paused"), DomainState::Paused);
        assert_eq!(DomainState::from_str("shut off"), DomainState::Shutoff);
        assert_eq!(DomainState::from_str("shutoff"), DomainState::Shutoff);
        assert_eq!(DomainState::from_str("crashed"), DomainState::Crashed);
        assert_eq!(
            DomainState::from_str("pmsuspended"),
            DomainState::Other("pmsuspended".to_owned())
        );
    }

    #[test]
    fn xml_escape_handles_entities() {
        assert_eq!(xml_escape("no-special"), std::borrow::Cow::Borrowed("no-special"));
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(xml_escape("it's"), "it&apos;s");
        assert_eq!(xml_escape("\"quote\""), "&quot;quote&quot;");
    }
}
