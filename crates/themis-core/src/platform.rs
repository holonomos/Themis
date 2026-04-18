//! NOS platform extension point.
//!
//! A `Platform` knows how to produce the config files a node needs and how
//! to reload/verify them once they've been pushed. The runtime handles the
//! actual file transfer and command execution — platforms are pure config
//! authors, not execution drivers.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::{Node, Result, Role, Topology};

/// NOS platform extension point.
///
/// Implementations live in the `themis-platforms` crate.
pub trait Platform: Send + Sync {
    /// Canonical name used by Themisfile (e.g., `"frr-fedora"`).
    fn name(&self) -> &str;

    /// Human-friendly display name.
    fn display_name(&self) -> &str;

    /// Roles this platform can drive. Nodes whose role is not in this list
    /// receive no configuration from this platform.
    fn node_roles(&self) -> &[Role];

    /// Produce the set of (remote path → content) entries to be written to
    /// the node. The runtime is responsible for writing them via SSH.
    fn generate_config(
        &self,
        node: &Node,
        topology: &Topology,
    ) -> Result<HashMap<PathBuf, String>>;

    /// Shell command to reload the NOS after config is pushed.
    /// Runs on the guest over SSH.
    fn reload_command(&self) -> &str;

    /// Shell command that exits 0 when the NOS is healthy after reload.
    /// Runs on the guest over SSH.
    fn verify_command(&self) -> &str;

    /// Resource profile (vCPU, memory_mb, disk_gb) for a given role.
    fn resource_profile(&self, role: Role) -> ResourceProfile;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceProfile {
    pub vcpu: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
}

impl ResourceProfile {
    pub const fn new(vcpu: u32, memory_mb: u32, disk_gb: u32) -> Self {
        Self {
            vcpu,
            memory_mb,
            disk_gb,
        }
    }
}
