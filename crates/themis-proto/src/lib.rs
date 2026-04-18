//! Themis protocol — generated gRPC types and services.
//!
//! Expanded in Phase 2 per `docs/WORK_PLAN.md`.

pub mod themis {
    pub mod v1 {
        tonic::include_proto!("themis.v1");
    }
}

pub use themis::v1::*;
