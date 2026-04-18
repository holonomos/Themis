//! Node roles.
//!
//! Templates own their domain vocabulary (e.g., clos uses border/spine/leaf;
//! three-tier uses core/distribution/access; hub-spoke uses hub/branch), but
//! the framework-level `Role` enum covers all known roles so runtime and
//! platform drivers can dispatch on them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    // Clos fabric
    Border,
    Spine,
    Leaf,
    Server,

    // Traditional three-tier
    Core,
    Distribution,
    Access,

    // Hub-and-spoke
    Hub,
    Branch,

    // Control plane
    Bastion,
    Services,
    Orchestrator,
    Telemetry,
    Registry,
}

impl Role {
    /// Tier code for deterministic MAC generation.
    /// Every role occupies a distinct byte so MACs sort by role.
    pub fn tier_code(&self) -> u8 {
        match self {
            Role::Border => 0x01,
            Role::Spine => 0x02,
            Role::Leaf => 0x03,
            Role::Server => 0x04,
            Role::Bastion => 0x05,
            Role::Services => 0x06,
            Role::Telemetry => 0x07,
            Role::Orchestrator => 0x08,
            Role::Registry => 0x09,
            Role::Core => 0x0A,
            Role::Distribution => 0x0B,
            Role::Access => 0x0C,
            Role::Hub => 0x0D,
            Role::Branch => 0x0E,
        }
    }

    pub fn is_control_plane(&self) -> bool {
        matches!(
            self,
            Role::Bastion
                | Role::Services
                | Role::Orchestrator
                | Role::Telemetry
                | Role::Registry
        )
    }

    /// True when the node runs a routing stack (FRR, NVUE, etc.).
    pub fn is_routed(&self) -> bool {
        matches!(
            self,
            Role::Border
                | Role::Spine
                | Role::Leaf
                | Role::Core
                | Role::Distribution
                | Role::Access
                | Role::Hub
                | Role::Branch
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Border => "border",
            Role::Spine => "spine",
            Role::Leaf => "leaf",
            Role::Server => "server",
            Role::Core => "core",
            Role::Distribution => "distribution",
            Role::Access => "access",
            Role::Hub => "hub",
            Role::Branch => "branch",
            Role::Bastion => "bastion",
            Role::Services => "services",
            Role::Orchestrator => "orchestrator",
            Role::Telemetry => "telemetry",
            Role::Registry => "registry",
        }
    }
}
