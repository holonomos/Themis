//! Themis core — domain types and extension traits.
//!
//! This crate defines the shapes that flow between the compiler, runtime,
//! templates, and platforms. The traits `Template` and `Platform` are the
//! two extension points that topology and NOS implementations plug into.

pub mod conversions;
pub mod error;
pub mod platform;
pub mod role;
pub mod template;
pub mod topology;

pub use error::{Error, Result};
pub use platform::Platform;
pub use role::Role;
pub use template::{
    ParameterDef, ParameterSchema, ParameterType, Parameters, Template,
};
pub use topology::{
    Addressing, BgpNeighbor, Bootstrap, Interface, Link, Management, Node, Topology,
};
