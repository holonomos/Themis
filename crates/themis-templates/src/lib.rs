//! Themis templates — built-in topology implementations.
//!
//! Populated in Phase 5 per `docs/WORK_PLAN.md`.

use themis_core::Template;

pub mod clos_3tier;
pub mod three_tier;
pub mod hub_spoke;

/// Registry of all built-in templates. Used by the compiler's expander
/// to resolve a template name from a Themisfile.
pub fn builtin() -> Vec<Box<dyn Template>> {
    vec![
        Box::new(clos_3tier::Clos3Tier),
        Box::new(three_tier::ThreeTier),
        Box::new(hub_spoke::HubSpoke),
    ]
}
