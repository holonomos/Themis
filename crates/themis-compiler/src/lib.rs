//! Themis compiler — Themisfile → artifacts pipeline.
//!
//! Populated in Phase 3 per `docs/WORK_PLAN.md`. Each sub-module corresponds
//! to one stage of the pipeline:
//!   - loader:    Themisfile (KDL) parser + schema validation
//!   - expander:  invokes `Template`, returns `Topology`
//!   - estimator: RAM/vCPU/KSM projection
//!   - inventory: per-node runtime artifacts (libvirt XML, cloud-init seeds)
//!   - renderer:  minijinja-driven NOS config rendering via `Platform`

pub mod estimator;
pub mod expander;
pub mod inventory;
pub mod loader;
pub mod renderer;
pub mod services_config;
