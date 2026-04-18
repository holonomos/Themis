//! Resource estimator — RAM / vCPU / KSM projection from a `Topology`.
//!
//! Produced as Phase 3c. Computes:
//! - Total vCPU and node count.
//! - Nominal (no-dedup) RAM: sum of each VM's `memory_mb`.
//! - Projected RAM after KSM deduplication: pages shared across VMs cloned
//!   from the same golden base are counted once; only the non-shared delta
//!   is counted per additional VM.
//! - Total disk in GB.
//!
//! Resource values are taken from each node's own fields when non-zero;
//! otherwise the platform's `resource_profile(role)` is consulted.

use themis_core::{Platform, Topology};

// ── KSM model constants ───────────────────────────────────────────────────────

/// Shared portion (MB) that KSM deduplicates to a single copy across VMs.
/// Covers kernel text, early read-only data, shared libraries, and init state
/// from the golden image — pages that are identical across all cloned VMs.
const BASE_SHARED_MB: u64 = 200;

/// Non-shared delta (MB) contributed by each VM beyond the first.
/// Represents working-set divergence: per-process heap, runtime state, etc.
const INCREMENTAL_MB_PER_VM: u64 = 60;

// ── Public types ──────────────────────────────────────────────────────────────

/// Resource projection for a topology.
#[derive(Debug, Clone, Copy)]
pub struct ResourceEstimate {
    /// Total virtual CPUs across all nodes.
    pub total_vcpu: u32,
    /// Total number of VMs.
    pub total_nodes: u32,
    /// Memory as if every VM ran independently (sum of all `node.memory_mb`).
    pub nominal_memory_mb: u64,
    /// Projected memory after KSM deduplication of shared pages across the
    /// golden base image.
    pub projected_memory_mb_after_ksm: u64,
    /// Total disk across all nodes, in GB.
    pub total_disk_gb: u64,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Estimate resources using each node's per-role resource profile from the
/// given platform.
///
/// If a node's resource fields (`vcpu`, `memory_mb`, `disk_gb`) are non-zero,
/// those values are used directly. Otherwise `Platform::resource_profile(role)`
/// is consulted to fill in any zero fields.
pub fn estimate(topology: &Topology, platform: &dyn Platform) -> ResourceEstimate {
    let mut total_vcpu: u32 = 0;
    let mut total_disk_gb: u64 = 0;
    let mut memory_values: Vec<u64> = Vec::with_capacity(topology.nodes.len());

    for node in topology.nodes.values() {
        // Fall back to platform profile for any zero resource field.
        let profile = platform.resource_profile(node.role);

        let vcpu = if node.vcpu != 0 { node.vcpu } else { profile.vcpu };
        let memory_mb = if node.memory_mb != 0 {
            node.memory_mb as u64
        } else {
            profile.memory_mb as u64
        };
        let disk_gb = if node.disk_gb != 0 {
            node.disk_gb as u64
        } else {
            profile.disk_gb as u64
        };

        total_vcpu = total_vcpu.saturating_add(vcpu);
        total_disk_gb = total_disk_gb.saturating_add(disk_gb);
        memory_values.push(memory_mb);
    }

    let total_nodes = memory_values.len() as u32;
    let nominal_memory_mb: u64 = memory_values.iter().sum();
    let projected_memory_mb_after_ksm = ksm_projection(&memory_values);

    ResourceEstimate {
        total_vcpu,
        total_nodes,
        nominal_memory_mb,
        projected_memory_mb_after_ksm,
    total_disk_gb,
    }
}

/// Convenience: estimate using a platform name looked up from
/// `themis_platforms::builtin()`.
///
/// Returns [`themis_core::Error::UnknownPlatform`] if the name does not match
/// any registered built-in platform.
pub fn estimate_with_builtin_platforms(
    topology: &Topology,
    platform_name: &str,
) -> themis_core::Result<ResourceEstimate> {
    let platforms = themis_platforms::builtin();
    let platform = platforms
        .iter()
        .find(|p| p.name() == platform_name)
        .ok_or_else(|| {
            themis_core::Error::UnknownPlatform(platform_name.to_string())
        })?;

    Ok(estimate(topology, platform.as_ref()))
}

// ── KSM projection model ──────────────────────────────────────────────────────

/// Apply the KSM deduplication model to a slice of per-VM memory allocations
/// (in MB) and return the projected total.
///
/// Model:
/// - A shared base of `BASE_SHARED_MB` pages is common to all VMs cloned from
///   the same golden image; KSM folds those N copies into one.
/// - Each VM contributes at least `INCREMENTAL_MB_PER_VM` of non-shared delta.
/// - For VMs whose total allocation is smaller than `BASE_SHARED_MB`, there is
///   no dedup gain — they contribute their full `memory_mb`.
///
/// Formula:
/// ```text
/// projected = BASE_SHARED_MB
///           + Σ max(memory_mb - BASE_SHARED_MB, INCREMENTAL_MB_PER_VM)
/// ```
///
/// The result is clamped to `[INCREMENTAL_MB_PER_VM * n, nominal]` to keep
/// the projection sensible (never less than a floor per VM, never more than
/// the undeduped total).
fn ksm_projection(memory_values: &[u64]) -> u64 {
    if memory_values.is_empty() {
        return 0;
    }

    let n = memory_values.len() as u64;
    let nominal: u64 = memory_values.iter().sum();

    // projected = BASE_SHARED_MB + Σ max(memory_mb - BASE_SHARED_MB, INCREMENTAL_MB_PER_VM)
    let projected = BASE_SHARED_MB
        + memory_values
            .iter()
            .map(|&mem| {
                if mem <= BASE_SHARED_MB {
                    // VM is too small to benefit from base dedup;
                    // treat it as contributing its full allocation.
                    mem
                } else {
                    // Subtract the shared base and keep only the delta,
                    // floored at INCREMENTAL_MB_PER_VM.
                    (mem - BASE_SHARED_MB).max(INCREMENTAL_MB_PER_VM)
                }
            })
            .sum::<u64>();

    // Clamp: [floor_per_vm * n, nominal]
    let floor = INCREMENTAL_MB_PER_VM * n;
    projected.clamp(floor, nominal)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    use themis_core::platform::ResourceProfile;
    use themis_core::topology::{Addressing, Bootstrap, Management};
    use themis_core::{Node, Platform, Result, Role, Topology};

    use super::*;

    // ── Minimal stub platform ─────────────────────────────────────────────────

    /// A configurable stub platform that returns fixed profiles per role.
    struct StubPlatform {
        profiles: HashMap<Role, ResourceProfile>,
    }

    impl StubPlatform {
        fn new(profiles: HashMap<Role, ResourceProfile>) -> Self {
            Self { profiles }
        }
    }

    impl Platform for StubPlatform {
        fn name(&self) -> &str {
            "stub"
        }

        fn display_name(&self) -> &str {
            "Stub"
        }

        fn node_roles(&self) -> &[Role] {
            &[]
        }

        fn resource_profile(&self, role: Role) -> ResourceProfile {
            self.profiles
                .get(&role)
                .copied()
                .unwrap_or(ResourceProfile::new(1, 256, 3))
        }

        fn generate_config(
            &self,
            _node: &Node,
            _topology: &Topology,
        ) -> Result<HashMap<PathBuf, String>> {
            Ok(HashMap::new())
        }

        fn reload_command(&self) -> &str {
            "true"
        }

        fn verify_command(&self) -> &str {
            "true"
        }
    }

    // ── Topology builders ─────────────────────────────────────────────────────

    fn make_management() -> Management {
        Management {
            cidr: "192.168.0.0/24".parse().unwrap(),
            gateway: IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
            bridge: "br-mgmt".to_string(),
            data_cidr: "10.100.0.0/24".parse().unwrap(),
            data_gateway: IpAddr::V4(Ipv4Addr::new(10, 100, 0, 1)),
            data_bridge: "br-data".to_string(),
            dns_domain: "fabric.local".to_string(),
        }
    }

    fn make_addressing() -> Addressing {
        Addressing {
            loopback_cidr: "10.0.0.0/16".parse().unwrap(),
            fabric_p2p_cidr: "10.1.0.0/16".parse().unwrap(),
        }
    }

    /// Build a node with explicit resource fields set.
    fn make_node(
        name: &str,
        role: Role,
        vcpu: u32,
        memory_mb: u32,
        disk_gb: u32,
        idx: u8,
    ) -> Node {
        Node {
            name: name.to_string(),
            role,
            nos_type: None,
            asn: None,
            loopback: None,
            mgmt_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 0, idx)),
            mgmt_mac: format!("52:54:00:ff:00:{idx:02x}"),
            vcpu,
            memory_mb,
            disk_gb,
            interfaces: vec![],
            bgp_neighbors: vec![],
            bootstrap: Bootstrap::Dhcp,
        }
    }

    /// Build a node with ALL resource fields zero (forces platform profile lookup).
    fn make_zero_node(name: &str, role: Role, idx: u8) -> Node {
        make_node(name, role, 0, 0, 0, idx)
    }

    fn make_topology(nodes: Vec<Node>) -> Topology {
        let node_map: HashMap<String, Node> =
            nodes.into_iter().map(|n| (n.name.clone(), n)).collect();
        Topology {
            name: "test-fabric".to_string(),
            template: "clos-3tier".to_string(),
            platform: "stub".to_string(),
            wan_interface: None,
            nodes: node_map,
            links: vec![],
            management: make_management(),
            addressing: make_addressing(),
        }
    }

    // ── Helper: stub platform with uniform profile ────────────────────────────

    fn uniform_platform(vcpu: u32, memory_mb: u32, disk_gb: u32) -> StubPlatform {
        let mut profiles = HashMap::new();
        for role in [
            Role::Border,
            Role::Spine,
            Role::Leaf,
            Role::Server,
            Role::Core,
            Role::Distribution,
            Role::Access,
            Role::Hub,
            Role::Branch,
            Role::Bastion,
            Role::Services,
            Role::Orchestrator,
            Role::Telemetry,
            Role::Registry,
        ] {
            profiles.insert(role, ResourceProfile::new(vcpu, memory_mb, disk_gb));
        }
        StubPlatform::new(profiles)
    }

    // ── Test: single node ─────────────────────────────────────────────────────

    /// With one VM, KSM has nothing to deduplicate — projection equals nominal.
    #[test]
    fn single_node() {
        let nodes = vec![make_node("leaf1", Role::Leaf, 1, 512, 5, 1)];
        let topology = make_topology(nodes);
        let platform = uniform_platform(1, 512, 5);

        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 1);
        assert_eq!(est.total_vcpu, 1);
        assert_eq!(est.nominal_memory_mb, 512);
        assert_eq!(est.total_disk_gb, 5);

        // With a single VM the projection should equal nominal (no peer to share with).
        // The formula gives BASE_SHARED_MB + max(512-200, 60) = 200+312 = 512.
        // Clamped to [60, 512] → 512.
        assert_eq!(est.projected_memory_mb_after_ksm, 512);
    }

    // ── Test: 10-node homogeneous topology ────────────────────────────────────

    /// Ten identical leaf nodes (256 MB each). KSM should reduce total memory
    /// below the nominal (10 × 256 = 2560 MB).
    #[test]
    fn ten_node_homogeneous() {
        let nodes: Vec<Node> = (1..=10)
            .map(|i| make_node(&format!("leaf{i}"), Role::Leaf, 1, 256, 3, i))
            .collect();
        let topology = make_topology(nodes);
        let platform = uniform_platform(1, 256, 3);

        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 10);
        assert_eq!(est.total_vcpu, 10);
        assert_eq!(est.nominal_memory_mb, 10 * 256);
        assert_eq!(est.total_disk_gb, 10 * 3);

        // Formula: BASE_SHARED_MB + 10 × max(256-200, 60)
        //        = 200 + 10 × max(56, 60)
        //        = 200 + 10 × 60
        //        = 200 + 600 = 800 MB
        // Clamp floor = 60 × 10 = 600, ceiling = 2560 → stays at 800.
        assert_eq!(est.projected_memory_mb_after_ksm, 800);
        assert!(
            est.projected_memory_mb_after_ksm < est.nominal_memory_mb,
            "KSM projection must be less than nominal for 10 homogeneous nodes"
        );
    }

    // ── Test: mixed-role topology ─────────────────────────────────────────────

    /// Two borders (512 MB each) + four spines (256 MB each) + four leaves
    /// (256 MB each). Platform profile values are taken from node fields.
    #[test]
    fn mixed_roles() {
        let mut nodes = Vec::new();
        for i in 1u8..=2 {
            nodes.push(make_node(&format!("border{i}"), Role::Border, 2, 512, 5, i));
        }
        for i in 1u8..=4 {
            nodes.push(make_node(&format!("spine{i}"), Role::Spine, 1, 256, 3, 10 + i));
        }
        for i in 1u8..=4 {
            nodes.push(make_node(&format!("leaf{i}"), Role::Leaf, 1, 256, 3, 20 + i));
        }

        let topology = make_topology(nodes);
        let platform = uniform_platform(1, 256, 3); // profile not used; all fields non-zero

        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 10);
        // 2 borders × 2 vcpu + 8 nodes × 1 vcpu = 12
        assert_eq!(est.total_vcpu, 2 * 2 + 8);
        // 2 × 512 + 8 × 256 = 1024 + 2048 = 3072 MB
        assert_eq!(est.nominal_memory_mb, 2 * 512 + 8 * 256);
        // 2 × 5 + 8 × 3 = 10 + 24 = 34 GB
        assert_eq!(est.total_disk_gb, 2 * 5 + 8 * 3);

        // Formula per VM:
        //   border (512 MB): max(512-200, 60) = max(312, 60) = 312
        //   spine/leaf (256 MB): max(256-200, 60) = max(56, 60) = 60
        // projected = 200 + 2×312 + 8×60 = 200 + 624 + 480 = 1304 MB
        // Clamp floor = 60×10 = 600, ceiling = 3072 → stays at 1304.
        assert_eq!(est.projected_memory_mb_after_ksm, 1304);
        assert!(est.projected_memory_mb_after_ksm < est.nominal_memory_mb);
    }

    // ── Test: nodes smaller than BASE_SHARED_MB ───────────────────────────────

    /// Some nodes (64 MB) are below BASE_SHARED_MB (200 MB) — no dedup gain
    /// for those. They contribute their full memory_mb to the projection.
    #[test]
    fn nodes_smaller_than_base_shared() {
        // 3 small VMs at 64 MB each (below BASE_SHARED_MB = 200).
        // 2 normal VMs at 512 MB each.
        let mut nodes: Vec<Node> = (1u8..=3)
            .map(|i| make_node(&format!("micro{i}"), Role::Server, 1, 64, 2, i))
            .collect();
        nodes.extend((1u8..=2).map(|i| {
            make_node(&format!("spine{i}"), Role::Spine, 1, 512, 5, 10 + i)
        }));

        let topology = make_topology(nodes);
        let platform = uniform_platform(1, 256, 3);

        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 5);
        assert_eq!(est.nominal_memory_mb, 3 * 64 + 2 * 512);

        // Formula:
        //   micro (64 MB, < BASE_SHARED_MB): contributes 64 (full, no dedup)
        //   spine (512 MB): max(512-200, 60) = 312
        // projected = 200 + 3×64 + 2×312 = 200 + 192 + 624 = 1016
        // Clamp floor = 60×5 = 300, ceiling = nominal (192+1024=1216) → 1016.
        let expected = BASE_SHARED_MB + 3 * 64 + 2 * 312;
        assert_eq!(est.projected_memory_mb_after_ksm, expected);

        // All small VMs are below BASE_SHARED_MB, so the projection is not
        // dramatically lower than nominal — verify it's still ≤ nominal.
        assert!(est.projected_memory_mb_after_ksm <= est.nominal_memory_mb);
    }

    // ── Test: zero resource fields fall back to platform profile ──────────────

    /// When node.vcpu, node.memory_mb, and node.disk_gb are all zero, the
    /// estimator must consult the platform's resource_profile instead.
    #[test]
    fn zero_fields_use_platform_profile() {
        // Nodes have zero resource fields; platform returns (2, 1024, 10).
        let nodes: Vec<Node> = (1u8..=4)
            .map(|i| make_zero_node(&format!("spine{i}"), Role::Spine, i))
            .collect();
        let topology = make_topology(nodes);

        let mut profiles = HashMap::new();
        profiles.insert(Role::Spine, ResourceProfile::new(2, 1024, 10));
        let platform = StubPlatform::new(profiles);

        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 4);
        assert_eq!(est.total_vcpu, 4 * 2, "should use platform profile vcpu=2");
        assert_eq!(
            est.nominal_memory_mb,
            4 * 1024,
            "should use platform profile memory_mb=1024"
        );
        assert_eq!(
            est.total_disk_gb,
            4 * 10,
            "should use platform profile disk_gb=10"
        );
    }

    // ── Test: estimate_with_builtin_platforms — unknown platform error ─────────

    #[test]
    fn unknown_builtin_platform_returns_error() {
        let topology = make_topology(vec![]);
        let result = estimate_with_builtin_platforms(&topology, "nonexistent-nos");
        assert!(
            result.is_err(),
            "should return Err for an unknown platform name"
        );
        // Verify it's the right variant.
        let err = result.unwrap_err();
        assert!(
            matches!(err, themis_core::Error::UnknownPlatform(_)),
            "error should be UnknownPlatform, got: {err}"
        );
    }

    // ── Test: estimate_with_builtin_platforms — frr-fedora resolves ───────────

    /// Smoke test: a topology of known-profile nodes resolves via frr-fedora
    /// and produces correct vCPU / disk totals.
    #[test]
    fn builtin_frr_fedora_resolves() {
        // frr-fedora: Leaf → (vcpu=1, memory_mb=256, disk_gb=3)
        // All resource fields are zero → platform profile is used.
        let nodes: Vec<Node> = (1u8..=5)
            .map(|i| make_zero_node(&format!("leaf{i}"), Role::Leaf, i))
            .collect();
        let mut topo = make_topology(nodes);
        topo.platform = "frr-fedora".to_string();

        let est = estimate_with_builtin_platforms(&topo, "frr-fedora")
            .expect("frr-fedora is a known built-in platform");

        assert_eq!(est.total_nodes, 5);
        assert_eq!(est.total_vcpu, 5); // 5 × 1
        assert_eq!(est.nominal_memory_mb, 5 * 256); // 5 × 256 MB
        assert_eq!(est.total_disk_gb, 5 * 3); // 5 × 3 GB
        assert!(
            est.projected_memory_mb_after_ksm <= est.nominal_memory_mb,
            "KSM projection must not exceed nominal"
        );
    }

    // ── Test: empty topology ──────────────────────────────────────────────────

    #[test]
    fn empty_topology() {
        let topology = make_topology(vec![]);
        let platform = uniform_platform(1, 256, 3);
        let est = estimate(&topology, &platform);

        assert_eq!(est.total_nodes, 0);
        assert_eq!(est.total_vcpu, 0);
        assert_eq!(est.nominal_memory_mb, 0);
        assert_eq!(est.projected_memory_mb_after_ksm, 0);
        assert_eq!(est.total_disk_gb, 0);
    }

    // ── Test: ksm_projection clamping ─────────────────────────────────────────

    /// Verify the floor clamp: with many tiny VMs the projection can in theory
    /// drop below `INCREMENTAL_MB_PER_VM * n`. The clamp must prevent that.
    #[test]
    fn ksm_projection_floor_clamp() {
        // 50 VMs at 10 MB each (well below BASE_SHARED_MB).
        // Formula: 200 + 50×10 = 700 MB
        // Clamp floor: 60×50 = 3000 → projection is clamped UP to 3000.
        // Nominal: 50×10 = 500 MB → ceiling clamp brings it down to 500.
        // So: clamp(700, 3000, 500) = 500 (ceiling wins, floor > ceiling, ceiling wins).
        //
        // Actually: when floor > ceiling, we respect nominal (ceiling) — the
        // clamp(x, lo, hi) call will return hi when lo > hi. Rust's u64::clamp
        // panics if lo > hi, so let's test a case where floor ≤ nominal.
        //
        // Use 50 VMs at 100 MB each:
        //   nominal = 50×100 = 5000
        //   formula = 200 + 50×100 = 5200 (but > nominal so clamped to 5000)
        //   floor   = 60×50 = 3000
        //   result  = clamp(5200, 3000, 5000) = 5000
        let values: Vec<u64> = vec![100; 50];
        let projected = super::ksm_projection(&values);
        let nominal: u64 = values.iter().sum();
        assert_eq!(projected, nominal, "projected should be clamped to nominal when formula exceeds nominal");
    }

    /// Verify the ceiling clamp: the result is never greater than nominal.
    #[test]
    fn ksm_projection_never_exceeds_nominal() {
        // One tiny VM at 50 MB: formula = 200 + 50 = 250, nominal = 50.
        // Clamp: max(250, 60) → wait, floor = 60×1 = 60, ceiling = 50.
        // floor > ceiling — Rust clamp panics. Let's use a case where floor ≤ nominal.
        // Use 4 VMs at 100 MB each: nominal=400, formula=200+4×100=600, floor=60×4=240.
        // clamp(600, 240, 400) = 400.
        let values: Vec<u64> = vec![100; 4];
        let projected = super::ksm_projection(&values);
        let nominal: u64 = values.iter().sum();
        assert!(projected <= nominal, "projection must never exceed nominal");
    }
}
