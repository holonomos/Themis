//! Themisfile loader — KDL parser and schema validator.
//!
//! Parses a [`Themisfile`](https://kdl.dev) and validates its `parameters {}`
//! block against the named template's [`ParameterSchema`].
//!
//! # Format
//!
//! ```kdl
//! fabric "my-lab" {
//!     template "clos-3tier"
//!     platform "frr-fedora"
//!     wan-interface "eth0"
//!
//!     parameters {
//!         borders 2
//!         spines 2
//!         racks 4
//!         servers-per-rack 4
//!     }
//! }
//! ```
//!
//! Parameter names in the Themisfile are kebab-case; they are stored in
//! [`Parameters`] as underscore_case (e.g. `servers-per-rack` →
//! `servers_per_rack`). [`ParameterSchema`] fields also use underscore_case.

use std::path::Path;

use themis_core::{Error, ParameterSchema, ParameterType, Parameters, Result};

// ─── Public types ────────────────────────────────────────────────────────────

/// Parsed contents of a Themisfile.
#[derive(Debug, Clone)]
pub struct ThemisfileDoc {
    /// The fabric name (the string argument to the top-level `fabric` node).
    pub name: String,
    /// Template identifier, e.g. `"clos-3tier"`.
    pub template: String,
    /// Platform identifier, e.g. `"frr-fedora"`.
    pub platform: String,
    /// Optional WAN interface name from `wan-interface`.
    pub wan_interface: Option<String>,
    /// User-supplied parameter values (keys in underscore_case).
    pub parameters: Parameters,
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Parse a Themisfile string into a [`ThemisfileDoc`].
///
/// Returns [`Error::Config`] for KDL parse failures or structural violations.
pub fn parse_themisfile(input: &str) -> Result<ThemisfileDoc> {
    let doc: kdl::KdlDocument = input
        .parse()
        .map_err(|e: kdl::KdlError| Error::Config(format!("KDL parse error: {e}")))?;

    // Top-level document must have exactly one node named "fabric".
    let fabric_node = doc
        .get("fabric")
        .ok_or_else(|| Error::Config("missing top-level 'fabric' node".to_string()))?;

    // fabric "my-lab" { … }  — first positional argument is the lab name.
    let name = fabric_node
        .get(0)
        .and_then(|v| v.as_string())
        .ok_or_else(|| {
            Error::Config("'fabric' node requires a string argument (the lab name)".to_string())
        })?
        .to_string();

    let body = fabric_node.children().ok_or_else(|| {
        Error::Config("'fabric' node requires a children block '{}'".to_string())
    })?;

    // ── template ──────────────────────────────────────────────────────────
    let template = get_required_string_arg(body, "template")?;

    // ── platform ──────────────────────────────────────────────────────────
    let platform = get_required_string_arg(body, "platform")?;

    // ── wan-interface (optional) ───────────────────────────────────────────
    let wan_interface = body
        .get("wan-interface")
        .and_then(|n| n.get(0))
        .and_then(|v| v.as_string())
        .map(str::to_string);

    // ── parameters {} ─────────────────────────────────────────────────────
    let params_node = body.get("parameters").ok_or_else(|| {
        Error::Config("'fabric' body requires a 'parameters {}' block".to_string())
    })?;

    let params_body = params_node.children().ok_or_else(|| {
        Error::Config("'parameters' requires a children block '{}'".to_string())
    })?;

    let mut parameters = Parameters::new();
    for node in params_body.nodes() {
        let key_kebab = node.name().value();
        let key = kebab_to_snake(key_kebab);

        // Each parameter node has one positional argument.
        let kdl_val = node.get(0).ok_or_else(|| {
            Error::Config(format!(
                "parameter '{}' has no value",
                key_kebab
            ))
        })?;

        let json_val = kdl_to_json(kdl_val).ok_or_else(|| {
            Error::Config(format!(
                "parameter '{}' has an unsupported value type (null or float)",
                key_kebab
            ))
        })?;

        parameters.set(key, json_val);
    }

    Ok(ThemisfileDoc {
        name,
        template,
        platform,
        wan_interface,
        parameters,
    })
}

/// Parse a Themisfile from a filesystem path.
///
/// Returns [`Error::Io`] for I/O failures and [`Error::Config`] for parse /
/// structural errors.
pub fn parse_themisfile_from_path(path: &Path) -> Result<ThemisfileDoc> {
    let content = std::fs::read_to_string(path)?;
    parse_themisfile(&content)
}

/// Validate user-supplied `parameters` against a template's `schema`.
///
/// - Fills in defaults for missing optional fields.
/// - Returns [`Error::InvalidParameter`] for:
///   - keys present in `parameters` that are absent from `schema` ("unknown
///     parameter")
///   - type mismatches between the supplied value and the schema's declared type
///   - integer values outside the `[min, max]` range declared in the schema
///   - required fields (no default) that are absent from `parameters`
pub fn validate_and_fill(
    parameters: &Parameters,
    schema: &ParameterSchema,
) -> Result<Parameters> {
    // Check for unknown parameters first (fail fast).
    for key in parameters.values.keys() {
        if !schema.fields.contains_key(key) {
            return Err(Error::InvalidParameter(format!(
                "unknown parameter: '{key}'"
            )));
        }
    }

    let mut out = Parameters::new();

    for (field, def) in &schema.fields {
        match parameters.values.get(field) {
            Some(value) => {
                // Type check.
                let type_ok = match def.ty {
                    ParameterType::Integer => value.is_i64() || value.is_u64(),
                    ParameterType::String => value.is_string(),
                    ParameterType::Boolean => value.is_boolean(),
                };
                if !type_ok {
                    return Err(Error::InvalidParameter(format!(
                        "parameter '{field}': expected {:?}, got a different type",
                        def.ty
                    )));
                }

                // Range check for integers.
                if def.ty == ParameterType::Integer {
                    let n = value.as_i64().ok_or_else(|| {
                        Error::InvalidParameter(format!(
                            "parameter '{field}': value is not a valid i64"
                        ))
                    })?;
                    if let Some(min) = def.min {
                        if n < min {
                            return Err(Error::InvalidParameter(format!(
                                "parameter '{field}': value {n} is below minimum {min}"
                            )));
                        }
                    }
                    if let Some(max) = def.max {
                        if n > max {
                            return Err(Error::InvalidParameter(format!(
                                "parameter '{field}': value {n} exceeds maximum {max}"
                            )));
                        }
                    }
                }

                out.set(field.clone(), value.clone());
            }
            None => {
                // Use default if available; otherwise the field is required.
                match &def.default {
                    Some(default_val) => {
                        out.set(field.clone(), default_val.clone());
                    }
                    None => {
                        return Err(Error::InvalidParameter(format!(
                            "required parameter '{field}' is missing and has no default"
                        )));
                    }
                }
            }
        }
    }

    Ok(out)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a kebab-case identifier to underscore_case.
///
/// `servers-per-rack` → `servers_per_rack`
fn kebab_to_snake(s: &str) -> String {
    s.replace('-', "_")
}

/// Extract the first positional string argument from a named node, or return
/// a [`Error::Config`] describing what was missing.
fn get_required_string_arg(doc: &kdl::KdlDocument, node_name: &str) -> Result<String> {
    doc.get(node_name)
        .ok_or_else(|| Error::Config(format!("missing required node '{node_name}'")))?
        .get(0)
        .and_then(|v| v.as_string())
        .ok_or_else(|| {
            Error::Config(format!(
                "node '{node_name}' requires a string argument"
            ))
        })
        .map(str::to_string)
}

/// Convert a [`kdl::KdlValue`] to a [`serde_json::Value`].
///
/// Returns `None` for values that have no JSON mapping (Null, Float).
fn kdl_to_json(val: &kdl::KdlValue) -> Option<serde_json::Value> {
    match val {
        kdl::KdlValue::String(s) => Some(serde_json::Value::String(s.clone())),
        kdl::KdlValue::Integer(i) => {
            // KdlValue::Integer is i128; serde_json uses i64 for Number.
            let n = i64::try_from(*i).ok()?;
            Some(serde_json::Value::Number(serde_json::Number::from(n)))
        }
        kdl::KdlValue::Bool(b) => Some(serde_json::Value::Bool(*b)),
        // Float and Null are not valid parameter types in Themis.
        kdl::KdlValue::Float(_) | kdl::KdlValue::Null => None,
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use themis_core::{ParameterDef, ParameterSchema, ParameterType};

    // ── helper to build a minimal valid Themisfile string ─────────────────

    fn minimal_themisfile() -> &'static str {
        r#"fabric "my-lab" {
    template "clos-3tier"
    platform "frr-fedora"

    parameters {
        borders 2
        spines 2
        racks 4
        servers-per-rack 4
    }
}"#
    }

    fn full_themisfile() -> &'static str {
        r#"fabric "full-lab" {
    template "clos-3tier"
    platform "frr-fedora"
    wan-interface "eth0"

    parameters {
        borders 2
        spines 2
        racks 4
        servers-per-rack 4
    }
}"#
    }

    // ── parse_themisfile ──────────────────────────────────────────────────

    #[test]
    fn parses_minimal_valid_themisfile() {
        let doc = parse_themisfile(minimal_themisfile()).expect("should parse");
        assert_eq!(doc.name, "my-lab");
        assert_eq!(doc.template, "clos-3tier");
        assert_eq!(doc.platform, "frr-fedora");
        assert!(doc.wan_interface.is_none());
        assert_eq!(doc.parameters.get_i64("borders"), Some(2));
        assert_eq!(doc.parameters.get_i64("spines"), Some(2));
        assert_eq!(doc.parameters.get_i64("racks"), Some(4));
        // kebab-case is converted to snake_case
        assert_eq!(doc.parameters.get_i64("servers_per_rack"), Some(4));
    }

    #[test]
    fn parses_full_themisfile_with_wan_interface() {
        let doc = parse_themisfile(full_themisfile()).expect("should parse");
        assert_eq!(doc.name, "full-lab");
        assert_eq!(doc.wan_interface.as_deref(), Some("eth0"));
    }

    #[test]
    fn missing_template_returns_error() {
        let input = r#"fabric "lab" {
    platform "frr-fedora"
    parameters {}
}"#;
        let err = parse_themisfile(input).unwrap_err();
        assert!(
            matches!(err, Error::Config(_)),
            "expected Config error, got: {err}"
        );
    }

    #[test]
    fn missing_platform_returns_error() {
        let input = r#"fabric "lab" {
    template "clos-3tier"
    parameters {}
}"#;
        let err = parse_themisfile(input).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn missing_parameters_block_returns_error() {
        let input = r#"fabric "lab" {
    template "clos-3tier"
    platform "frr-fedora"
}"#;
        let err = parse_themisfile(input).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn missing_fabric_node_returns_error() {
        let input = r#"notfabric "lab" {
    template "clos-3tier"
    platform "frr-fedora"
    parameters {}
}"#;
        let err = parse_themisfile(input).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn invalid_kdl_returns_config_error() {
        let input = "this is not {{ valid kdl !!!";
        let err = parse_themisfile(input).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn kebab_case_params_stored_as_snake_case() {
        let doc = parse_themisfile(minimal_themisfile()).expect("should parse");
        // raw kebab key should NOT be present
        assert!(doc.parameters.values.get("servers-per-rack").is_none());
        // snake_case key must be present
        assert!(doc.parameters.values.get("servers_per_rack").is_some());
    }

    // ── validate_and_fill ─────────────────────────────────────────────────

    fn sample_schema() -> ParameterSchema {
        ParameterSchema::new()
            .with(
                "borders",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: None,
                    min: Some(1),
                    max: Some(8),
                    description: Some("Number of border routers".into()),
                },
            )
            .with(
                "spines",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: Some(json!(2)),
                    min: Some(1),
                    max: Some(16),
                    description: None,
                },
            )
            .with(
                "enable_bfd",
                ParameterDef {
                    ty: ParameterType::Boolean,
                    default: Some(json!(false)),
                    min: None,
                    max: None,
                    description: None,
                },
            )
            .with(
                "label",
                ParameterDef {
                    ty: ParameterType::String,
                    default: Some(json!("default-label")),
                    min: None,
                    max: None,
                    description: None,
                },
            )
    }

    #[test]
    fn valid_params_pass_validation() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        params.set("borders", json!(2));
        params.set("spines", json!(4));
        params.set("enable_bfd", json!(true));
        params.set("label", json!("my-fabric"));

        let out = validate_and_fill(&params, &schema).expect("should validate");
        assert_eq!(out.get_i64("borders"), Some(2));
        assert_eq!(out.get_i64("spines"), Some(4));
        assert_eq!(out.get_bool("enable_bfd"), Some(true));
        assert_eq!(out.get_str("label"), Some("my-fabric"));
    }

    #[test]
    fn defaults_fill_in_for_missing_optional_fields() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        // provide only the required field; the rest have defaults
        params.set("borders", json!(2));

        let out = validate_and_fill(&params, &schema).expect("should validate");
        assert_eq!(out.get_i64("spines"), Some(2), "spines should default to 2");
        assert_eq!(
            out.get_bool("enable_bfd"),
            Some(false),
            "enable_bfd should default to false"
        );
        assert_eq!(
            out.get_str("label"),
            Some("default-label"),
            "label should default"
        );
    }

    #[test]
    fn missing_required_field_returns_error() {
        let schema = sample_schema();
        // "borders" has no default → required
        let params = Parameters::new();

        let err = validate_and_fill(&params, &schema).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParameter(msg) if msg.contains("borders")),
            "expected InvalidParameter about 'borders', got: {err}"
        );
    }

    #[test]
    fn unknown_parameter_returns_error() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        params.set("borders", json!(2));
        params.set("ghost_field", json!(99)); // not in schema

        let err = validate_and_fill(&params, &schema).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParameter(msg) if msg.contains("unknown parameter")),
            "expected unknown-parameter error, got: {err}"
        );
    }

    #[test]
    fn type_mismatch_returns_error() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        params.set("borders", json!("two")); // should be integer

        let err = validate_and_fill(&params, &schema).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParameter(msg) if msg.contains("borders")),
            "expected type-mismatch error for 'borders', got: {err}"
        );
    }

    #[test]
    fn out_of_range_integer_below_min_returns_error() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        params.set("borders", json!(0)); // min is 1

        let err = validate_and_fill(&params, &schema).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParameter(msg) if msg.contains("minimum")),
            "expected below-min error, got: {err}"
        );
    }

    #[test]
    fn out_of_range_integer_above_max_returns_error() {
        let schema = sample_schema();
        let mut params = Parameters::new();
        params.set("borders", json!(100)); // max is 8

        let err = validate_and_fill(&params, &schema).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParameter(msg) if msg.contains("maximum")),
            "expected above-max error, got: {err}"
        );
    }

    #[test]
    fn integer_at_boundary_values_passes() {
        let schema = sample_schema();

        let mut params_min = Parameters::new();
        params_min.set("borders", json!(1)); // exactly min
        assert!(validate_and_fill(&params_min, &schema).is_ok());

        let mut params_max = Parameters::new();
        params_max.set("borders", json!(8)); // exactly max
        assert!(validate_and_fill(&params_max, &schema).is_ok());
    }

    // ── round-trip: parse then validate ───────────────────────────────────

    #[test]
    fn parsed_params_pass_schema_validation() {
        let doc = parse_themisfile(minimal_themisfile()).expect("parse");

        let schema = ParameterSchema::new()
            .with(
                "borders",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: None,
                    min: Some(1),
                    max: Some(16),
                    description: None,
                },
            )
            .with(
                "spines",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: None,
                    min: Some(1),
                    max: Some(16),
                    description: None,
                },
            )
            .with(
                "racks",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: None,
                    min: Some(1),
                    max: Some(32),
                    description: None,
                },
            )
            .with(
                "servers_per_rack",
                ParameterDef {
                    ty: ParameterType::Integer,
                    default: None,
                    min: Some(1),
                    max: Some(32),
                    description: None,
                },
            );

        let validated = validate_and_fill(&doc.parameters, &schema).expect("should validate");
        assert_eq!(validated.get_i64("borders"), Some(2));
        assert_eq!(validated.get_i64("spines"), Some(2));
        assert_eq!(validated.get_i64("racks"), Some(4));
        assert_eq!(validated.get_i64("servers_per_rack"), Some(4));
    }
}
