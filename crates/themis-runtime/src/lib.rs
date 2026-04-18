//! Themis runtime — orchestration primitives.
//!
//! Populated in Phase 4 per `docs/WORK_PLAN.md`. Each sub-module is one
//! independent primitive the daemon composes to realize labs:
//!   - host:    ip / bridge / iptables / cloud-localds via `std::process::Command`
//!   - libvirt: `virsh` shell-outs (create, destroy, list, define, undefine)
//!   - iso:     cloud-init seed ISO building
//!   - ssh:     `russh` wrapper, parallel execution on `tokio`

pub mod fabric;
pub mod host;
pub mod iso;
pub mod keys;
pub mod libvirt;
pub mod ssh;
