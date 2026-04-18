//! `themis diagram <THEMISFILE>` — ASCII/Unicode topology render.
//!
//! Parses + expands the Themisfile, then renders a topology diagram showing
//! nodes grouped by tier. Output is paste-able into Slack or a README.
//!
//! Layout:
//!   - Nodes grouped into tiers (rows) based on role priority.
//!   - Each tier is rendered as a row of boxes with the node name inside.
//!   - Tier-to-tier edges shown as lines between rows.
//!   - Large topologies (>40 nodes) truncate lower tiers.
//!   - Legend at the bottom.

use std::collections::HashMap;
use std::io::IsTerminal as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use themis_compiler::{expander, loader};
use themis_core::{Role, Topology};

use crate::output::OutputFormat;

// ── Tier ordering ─────────────────────────────────────────────────────────────

/// Tier index (lower = higher in diagram). Roles not listed get tier 99.
fn tier_index(role: Role) -> u8 {
    match role {
        // Control plane (first, topmost)
        Role::Bastion | Role::Services | Role::Orchestrator | Role::Telemetry | Role::Registry => 0,
        // Clos fabric top
        Role::Border | Role::Core | Role::Hub => 1,
        // Middle
        Role::Spine | Role::Distribution | Role::Branch => 2,
        // Lower
        Role::Leaf | Role::Access => 3,
        // Leaf nodes
        Role::Server => 4,
    }
}

fn tier_name(role: Role) -> &'static str {
    match role {
        Role::Bastion | Role::Services | Role::Orchestrator | Role::Telemetry | Role::Registry => {
            "control-plane"
        }
        Role::Border => "border",
        Role::Core => "core",
        Role::Hub => "hub",
        Role::Spine => "spine",
        Role::Distribution => "distribution",
        Role::Branch => "branch",
        Role::Leaf => "leaf",
        Role::Access => "access",
        Role::Server => "server",
    }
}

// ── ANSI color helpers ────────────────────────────────────────────────────────

fn tier_color(tier: u8, use_color: bool) -> (&'static str, &'static str) {
    if !use_color {
        return ("", "");
    }
    match tier {
        0 => ("\x1b[36m", "\x1b[0m"), // cyan = control plane
        1 => ("\x1b[33m", "\x1b[0m"), // yellow = top routing tier
        2 => ("\x1b[32m", "\x1b[0m"), // green = mid tier
        3 => ("\x1b[34m", "\x1b[0m"), // blue = leaf tier
        4 => ("\x1b[90m", "\x1b[0m"), // dark grey = servers
        _ => ("", ""),
    }
}

// ── Node box rendering ────────────────────────────────────────────────────────

/// Render a node as a box:
/// ```text
/// ╭────────╮
/// │ spine1 │
/// ╰────────╯
/// ```
fn node_box(name: &str) -> Vec<String> {
    let inner = format!(" {name} ");
    let width = inner.len();
    let top = format!("╭{}╮", "─".repeat(width));
    let mid = format!("│{}│", inner);
    let bot = format!("╰{}╯", "─".repeat(width));
    vec![top, mid, bot]
}

/// Horizontal-join several multi-line strings with a gap between them.
fn hjoin(boxes: &[Vec<String>], gap: usize) -> Vec<String> {
    let height = boxes.iter().map(|b| b.len()).max().unwrap_or(0);
    let spacer = " ".repeat(gap);
    let mut rows: Vec<String> = Vec::with_capacity(height);
    for row in 0..height {
        let line: String = boxes
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let part = b.get(row).map(|s| s.as_str()).unwrap_or("");
                if i == 0 {
                    part.to_string()
                } else {
                    format!("{spacer}{part}")
                }
            })
            .collect();
        rows.push(line);
    }
    rows
}

/// Width of the first line of a multi-line block.
fn block_width(block: &[String]) -> usize {
    block.first().map(|s| s.len()).unwrap_or(0)
}

// ── Tier structure ────────────────────────────────────────────────────────────

const MAX_NODES: usize = 40;
const TRUNCATE_THRESHOLD: usize = 6; // nodes per tier before truncation

struct TierInfo {
    tier_idx: u8,
    label: &'static str,
    nodes: Vec<String>, // sorted node names
    #[allow(dead_code)]
    role: Role, // representative role for future per-role color differentiation
    truncated: bool,
    total_count: usize,
}

fn build_tiers(topology: &Topology) -> Vec<TierInfo> {
    // Group nodes by tier index.
    let mut by_tier: HashMap<u8, (Role, Vec<String>)> = HashMap::new();
    for node in topology.nodes.values() {
        let ti = tier_index(node.role);
        let entry = by_tier.entry(ti).or_insert((node.role, vec![]));
        entry.1.push(node.name.clone());
    }

    let mut tiers: Vec<TierInfo> = by_tier
        .into_iter()
        .map(|(ti, (role, mut nodes))| {
            nodes.sort();
            let total_count = nodes.len();
            let truncated = total_count > TRUNCATE_THRESHOLD;
            if truncated {
                nodes.truncate(TRUNCATE_THRESHOLD);
            }
            TierInfo {
                tier_idx: ti,
                label: tier_name(role),
                nodes,
                role,
                truncated,
                total_count,
            }
        })
        .collect();

    tiers.sort_by_key(|t| t.tier_idx);
    tiers
}

// ── Connector lines ───────────────────────────────────────────────────────────

/// Emit a simple connector row between two rendered tier rows.
/// Centers a `│` under each box in the upper tier and a `▼` row above each in
/// the lower tier. For simplicity we draw one connector centred on the row.
fn connector_line(upper_width: usize) -> String {
    let mid = upper_width / 2;
    let mut s = " ".repeat(mid);
    s.push('│');
    s
}

// ── Main render ───────────────────────────────────────────────────────────────

pub fn render(topology: &Topology, use_color: bool) -> String {
    let total_nodes = topology.nodes.len();
    let tiers = build_tiers(topology);

    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "Topology: {}  template={}  platform={}\n",
        topology.name, topology.template, topology.platform
    ));
    out.push_str(&format!("Nodes: {total_nodes}\n\n"));

    if tiers.is_empty() {
        out.push_str("(no nodes)\n");
        return out;
    }

    // Warn on very large topologies.
    let is_large = total_nodes > MAX_NODES;
    if is_large {
        out.push_str(&format!(
            "Note: topology has {total_nodes} nodes — lower tiers are truncated.\n\n"
        ));
    }

    let mut prev_row_width = 0usize;

    for (i, tier) in tiers.iter().enumerate() {
        let (color_on, color_off) = tier_color(tier.tier_idx, use_color);

        // Label.
        out.push_str(&format!(
            "{color_on}── {label} ──{color_off}\n",
            label = tier.label
        ));

        if tier.nodes.is_empty() {
            out.push_str("   (none)\n");
            continue;
        }

        // Render all nodes as boxes, then join them horizontally.
        let boxes: Vec<Vec<String>> = tier.nodes.iter().map(|n| node_box(n)).collect();
        let row = hjoin(&boxes, 3);
        let row_width = block_width(&row);

        // Connector from previous tier.
        if i > 0 && prev_row_width > 0 {
            let conn = connector_line(prev_row_width);
            out.push_str(&format!("{conn}\n"));
            // A small downward arrow row.
            let down_row_mid = row_width / 2;
            let arr = format!("{}▼", " ".repeat(down_row_mid));
            out.push_str(&format!("{arr}\n"));
        }

        for line in &row {
            out.push_str(&format!("{color_on}{line}{color_off}\n"));
        }

        if tier.truncated {
            out.push_str(&format!(
                "  … and {} more {}\n",
                tier.total_count - TRUNCATE_THRESHOLD,
                tier.label
            ));
        }

        out.push('\n');
        prev_row_width = row_width;
    }

    // Legend.
    out.push_str("Legend:\n");
    for tier in &tiers {
        let (c, r) = tier_color(tier.tier_idx, use_color);
        out.push_str(&format!(
            "  {c}■{r}  {} ({})\n",
            tier.label, tier.total_count
        ));
    }

    out
}

// ── Public command entry point ────────────────────────────────────────────────

pub fn run(themisfile: PathBuf, no_color: bool, _fmt: OutputFormat) -> Result<()> {
    let doc = loader::parse_themisfile_from_path(&themisfile)
        .with_context(|| format!("parsing {}", themisfile.display()))?;

    let topology = expander::expand_with_builtins(
        &doc.name,
        &doc.template,
        &doc.platform,
        doc.wan_interface.as_deref().unwrap_or(""),
        &doc.parameters,
    )
    .with_context(|| format!("expanding topology for '{}'", doc.name))?;

    let use_color = !no_color && std::io::stdout().is_terminal();
    let diagram = render(&topology, use_color);
    print!("{diagram}");
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use themis_core::topology::{Addressing, Bootstrap, Management};
    use themis_core::{Node, Role, Topology};

    use super::*;

    fn make_node(name: &str, role: Role, idx: u8) -> Node {
        Node {
            name: name.to_string(),
            role,
            nos_type: None,
            asn: None,
            loopback: None,
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, idx)),
            mgmt_mac: format!("52:54:00:00:00:{idx:02x}"),
            vcpu: 1,
            memory_mb: 256,
            disk_gb: 3,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Dhcp,
        }
    }

    fn small_clos_topology() -> Topology {
        let mut nodes = HashMap::new();
        for (name, role, idx) in [
            ("border-1", Role::Border, 1u8),
            ("border-2", Role::Border, 2),
            ("spine-1", Role::Spine, 3),
            ("spine-2", Role::Spine, 4),
            ("leaf-1", Role::Leaf, 5),
            ("leaf-2", Role::Leaf, 6),
            ("srv-1", Role::Server, 7),
            ("srv-2", Role::Server, 8),
        ] {
            nodes.insert(name.to_string(), make_node(name, role, idx));
        }
        let cidr = "10.0.0.0/24".parse().unwrap();
        let gw = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        Topology {
            name: "test-clos".to_string(),
            template: "clos-3tier".to_string(),
            platform: "frr-fedora".to_string(),
            wan_interface: None,
            nodes,
            links: vec![],
            management: Management {
                cidr,
                gateway: gw,
                bridge: "br-mgmt".to_string(),
                data_cidr: cidr,
                data_gateway: gw,
                data_bridge: "br-data".to_string(),
                dns_domain: "lab.local".to_string(),
            },
            addressing: Addressing {
                loopback_cidr: "10.254.0.0/24".parse().unwrap(),
                fabric_p2p_cidr: "10.255.0.0/16".parse().unwrap(),
            },
        }
    }

    #[test]
    fn render_contains_node_names() {
        let topo = small_clos_topology();
        let output = render(&topo, false);
        assert!(output.contains("border-1"), "missing border-1");
        assert!(output.contains("spine-1"), "missing spine-1");
        assert!(output.contains("leaf-1"), "missing leaf-1");
        assert!(output.contains("srv-1"), "missing srv-1");
    }

    #[test]
    fn render_contains_tier_labels() {
        let topo = small_clos_topology();
        let output = render(&topo, false);
        assert!(output.contains("border"), "missing border tier label");
        assert!(output.contains("spine"), "missing spine tier label");
        assert!(output.contains("leaf"), "missing leaf tier label");
        assert!(output.contains("server"), "missing server tier label");
    }

    #[test]
    fn render_contains_legend() {
        let topo = small_clos_topology();
        let output = render(&topo, false);
        assert!(output.contains("Legend:"), "missing legend");
    }

    #[test]
    fn render_topology_and_node_count_in_header() {
        let topo = small_clos_topology();
        let output = render(&topo, false);
        assert!(output.contains("Nodes: 8"), "expected 8 nodes in header");
        assert!(output.contains("test-clos"), "missing lab name");
    }

    #[test]
    fn node_box_three_lines() {
        let b = node_box("spine-1");
        assert_eq!(b.len(), 3);
        assert!(b[0].starts_with('╭'));
        assert!(b[1].contains("spine-1"));
        assert!(b[2].starts_with('╰'));
    }

    #[test]
    fn hjoin_produces_correct_width() {
        let a = node_box("a");
        let b = node_box("bb");
        let gap = 2;
        let joined = hjoin(&[a.clone(), b.clone()], gap);
        let expected_width = block_width(&a) + gap + block_width(&b);
        assert_eq!(joined[0].len(), expected_width);
    }

    /// Golden-output snapshot for a known small topology.
    #[test]
    fn golden_small_clos_render() {
        let topo = small_clos_topology();
        let output = render(&topo, false);

        // The golden expectations we care about:
        //   - header has name and node count
        //   - border tier appears before spine
        //   - node boxes use unicode corners
        //   - legend is present

        let lines: Vec<&str> = output.lines().collect();

        // First non-empty line is "Topology: ..."
        let header = lines.iter().find(|l| !l.is_empty()).copied().unwrap_or("");
        assert!(header.starts_with("Topology:"), "header: {header}");

        // Nodes line.
        let nodes_line = lines.iter().find(|l| l.starts_with("Nodes:")).copied().unwrap_or("");
        assert_eq!(nodes_line, "Nodes: 8");

        // Tier ordering: border row comes before spine row.
        let border_pos = lines.iter().position(|l| l.contains("border")).unwrap_or(usize::MAX);
        let spine_pos = lines.iter().position(|l| l.contains("spine")).unwrap_or(usize::MAX);
        assert!(border_pos < spine_pos, "border tier must precede spine tier");

        // Unicode box corners are present.
        assert!(output.contains('╭'), "missing box top-left corner");
        assert!(output.contains('╰'), "missing box bottom-left corner");

        // Legend.
        let legend_pos = lines.iter().position(|l| *l == "Legend:").unwrap_or(usize::MAX);
        assert!(legend_pos < lines.len(), "Legend: not found");
    }

    #[test]
    fn truncation_threshold() {
        // Build a topology with more than TRUNCATE_THRESHOLD servers.
        let mut nodes = HashMap::new();
        for i in 0..10u8 {
            let name = format!("srv-{i}");
            nodes.insert(name.clone(), make_node(&name, Role::Server, i));
        }
        let cidr = "10.0.0.0/24".parse().unwrap();
        let gw = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let topo = Topology {
            name: "big-lab".to_string(),
            template: "clos-3tier".to_string(),
            platform: "frr-fedora".to_string(),
            wan_interface: None,
            nodes,
            links: vec![],
            management: Management {
                cidr,
                gateway: gw,
                bridge: "br-mgmt".to_string(),
                data_cidr: cidr,
                data_gateway: gw,
                data_bridge: "br-data".to_string(),
                dns_domain: "lab.local".to_string(),
            },
            addressing: Addressing {
                loopback_cidr: "10.254.0.0/24".parse().unwrap(),
                fabric_p2p_cidr: "10.255.0.0/16".parse().unwrap(),
            },
        };
        let output = render(&topo, false);
        // Should contain the truncation message.
        assert!(output.contains("… and"), "expected truncation message");
    }
}
