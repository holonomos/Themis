//! Topology template extension point.
//!
//! A `Template` defines a fabric shape. Given user-supplied parameters, it
//! produces a fully enumerated `Topology` that the runtime can realize.
//! Templates own their domain vocabulary — a clos template talks about
//! spines and leafs; a three-tier template talks about core/dist/access.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{Result, Topology};

/// Primitive types a parameter may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterType {
    Integer,
    String,
    Boolean,
}

/// Schema entry for one parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDef {
    #[serde(rename = "type")]
    pub ty: ParameterType,
    pub default: Option<serde_json::Value>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub description: Option<String>,
}

/// The schema a template advertises. Drives validation of user input.
#[derive(Debug, Clone, Default)]
pub struct ParameterSchema {
    pub fields: BTreeMap<String, ParameterDef>,
}

impl ParameterSchema {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, name: impl Into<String>, def: ParameterDef) -> Self {
        self.fields.insert(name.into(), def);
        self
    }
}

/// User-supplied values for a template's parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Parameters {
    pub values: BTreeMap<String, serde_json::Value>,
}

impl Parameters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.values.insert(key.into(), value);
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.values.get(key).and_then(|v| v.as_i64())
    }

    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get_i64(key).and_then(|n| u32::try_from(n).ok())
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(|v| v.as_str())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.values.get(key).and_then(|v| v.as_bool())
    }
}

/// Topology template extension point.
///
/// Implementations live in the `themis-templates` crate. `Template::expand`
/// is pure logic — it receives parameters and returns a rendered topology;
/// no I/O, no side effects.
pub trait Template: Send + Sync {
    /// Canonical name used by Themisfile (e.g., `"clos-3tier"`).
    fn name(&self) -> &str;

    /// Human-friendly display name.
    fn display_name(&self) -> &str;

    /// The parameter schema this template advertises.
    fn schema(&self) -> &ParameterSchema;

    /// Expand parameters into a fully enumerated topology.
    fn expand(
        &self,
        fabric_name: &str,
        platform: &str,
        wan_interface: &str,
        params: &Parameters,
    ) -> Result<Topology>;
}
