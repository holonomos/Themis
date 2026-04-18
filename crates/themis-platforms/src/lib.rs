//! Themis platforms — built-in NOS driver implementations.
//!
//! Populated in Phase 6 per `docs/WORK_PLAN.md`.

use themis_core::Platform;

pub mod frr_fedora;
pub mod cumulus_vx;

/// Registry of all built-in platforms. Used by the compiler's renderer
/// to resolve a platform name from a Themisfile.
pub fn builtin() -> Vec<Box<dyn Platform>> {
    vec![
        Box::new(frr_fedora::FrrFedora),
        Box::new(cumulus_vx::CumulusVx),
    ]
}
