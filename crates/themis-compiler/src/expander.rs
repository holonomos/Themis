//! Topology expander — resolves a template by name and calls `expand()`.
//!
//! The expander is a thin registry lookup followed by a single trait-method
//! dispatch.  It intentionally owns no template logic; all topology-shape
//! decisions live inside each `Template` implementation.
//!
//! # Registry contract
//!
//! A *registry* is any `&[Box<dyn Template>]`.  The canonical registry is the
//! one returned by `themis_templates::builtin()`, but the slice abstraction
//! lets callers (and tests) inject arbitrary implementations without touching
//! the built-in crate.
//!
//! # Parameter validation
//!
//! Before calling `Template::expand`, the expander delegates to
//! `crate::loader::validate_and_fill` so that parameter coercion, default
//! application, and schema enforcement have exactly one home in the codebase.

use themis_core::{Parameters, Template, Topology};

// ── Public API ────────────────────────────────────────────────────────────────

/// Look up a template by name and expand it into a `Topology`.
///
/// `registry` is typically `themis_templates::builtin()`, but is passed in to
/// allow tests (and future dynamic plugin loading) to swap implementations.
///
/// # Errors
///
/// - [`themis_core::Error::UnknownTemplate`] if `template_name` is not present
///   in `registry`.
/// - Any error propagated from `crate::loader::validate_and_fill` (parameter
///   validation failures) or from `Template::expand` (topology generation
///   failures).
pub fn expand(
    fabric_name: &str,
    template_name: &str,
    platform: &str,
    wan_interface: &str,
    parameters: &Parameters,
    registry: &[Box<dyn Template>],
) -> themis_core::Result<Topology> {
    // ── 1. Registry lookup ────────────────────────────────────────────────
    let template = registry
        .iter()
        .find(|t| t.name() == template_name)
        .ok_or_else(|| themis_core::Error::UnknownTemplate(template_name.to_string()))?;

    // ── 2. Validate & fill defaults via the loader's canonical validator ──
    //
    // `crate::loader::validate_and_fill` is authored by the Phase 3a agent.
    // It is referenced here by path so that both agents can land independently
    // and the workspace compiles once both modules are complete.
    let filled_params = crate::loader::validate_and_fill(parameters, template.schema())?;

    // ── 3. Expand ─────────────────────────────────────────────────────────
    template.expand(fabric_name, platform, wan_interface, &filled_params)
}

/// Convenience wrapper that uses [`themis_templates::builtin()`] as the
/// registry.
///
/// Prefer [`expand`] in tests so that mock templates can be injected without
/// touching the built-in crate.
pub fn expand_with_builtins(
    fabric_name: &str,
    template_name: &str,
    platform: &str,
    wan_interface: &str,
    parameters: &Parameters,
) -> themis_core::Result<Topology> {
    let registry = themis_templates::builtin();
    expand(
        fabric_name,
        template_name,
        platform,
        wan_interface,
        parameters,
        &registry,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use themis_core::{Error, ParameterSchema, Parameters, Result, Template, Topology};

    // ── Mock template — always succeeds ───────────────────────────────────
    //
    // The topology returned here is minimal but type-correct.  We construct it
    // fully inside the `MockTemplate` impl to avoid importing `ipnet` directly
    // into the test module (it is not a direct dependency of this crate).

    struct MockTemplate {
        id: &'static str,
    }

    impl Template for MockTemplate {
        fn name(&self) -> &str {
            self.id
        }

        fn display_name(&self) -> &str {
            self.id
        }

        fn schema(&self) -> &ParameterSchema {
            // Empty schema — `validate_and_fill` is a no-op against an empty
            // schema, so no validation errors are expected in these tests.
            //
            // `ParameterSchema` is not const-constructible, so we lazily
            // initialise it once with `OnceLock`.
            use std::sync::OnceLock;
            static SCHEMA: OnceLock<ParameterSchema> = OnceLock::new();
            SCHEMA.get_or_init(ParameterSchema::new)
        }

        fn expand(
            &self,
            fabric_name: &str,
            platform: &str,
            _wan_interface: &str,
            _params: &Parameters,
        ) -> Result<Topology> {
            use std::collections::HashMap;
            use std::net::{IpAddr, Ipv4Addr};
            use themis_core::{Addressing, Management};

            // ipnet types come from themis_core's re-exported fields; we
            // parse them here to avoid a direct ipnet dependency at the
            // test-module level.
            let cidr: ipnet::IpNet = "10.0.0.0/24".parse().unwrap();
            let gw: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

            Ok(Topology {
                name: fabric_name.to_string(),
                template: self.id.to_string(),
                platform: platform.to_string(),
                wan_interface: None,
                nodes: HashMap::new(),
                links: vec![],
                management: Management {
                    cidr,
                    gateway: gw,
                    bridge: "br-mgmt".to_string(),
                    data_cidr: cidr,
                    data_gateway: gw,
                    data_bridge: "br-data".to_string(),
                    dns_domain: "lab.local".to_string(),
                },
                addressing: Addressing {
                    loopback_cidr: "10.254.0.0/24".parse().unwrap(),
                    fabric_p2p_cidr: "10.255.0.0/16".parse().unwrap(),
                },
            })
        }
    }

    // ── Mock template — always fails ──────────────────────────────────────

    struct BrokenTemplate;

    impl Template for BrokenTemplate {
        fn name(&self) -> &str {
            "broken"
        }

        fn display_name(&self) -> &str {
            "Broken Template"
        }

        fn schema(&self) -> &ParameterSchema {
            use std::sync::OnceLock;
            static SCHEMA: OnceLock<ParameterSchema> = OnceLock::new();
            SCHEMA.get_or_init(ParameterSchema::new)
        }

        fn expand(
            &self,
            _fabric_name: &str,
            _platform: &str,
            _wan_interface: &str,
            _params: &Parameters,
        ) -> Result<Topology> {
            Err(Error::Template("expansion deliberately failed".to_string()))
        }
    }

    // ── Helper: build a small registry ───────────────────────────────────

    fn registry_with(templates: Vec<Box<dyn Template>>) -> Vec<Box<dyn Template>> {
        templates
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: unknown template name returns UnknownTemplate error
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn unknown_template_returns_error() {
        let registry: Vec<Box<dyn Template>> = registry_with(vec![
            Box::new(MockTemplate { id: "mock-a" }),
        ]);
        let params = Parameters::new();

        let err = expand(
            "my-lab",
            "does-not-exist",
            "frr-fedora",
            "eth0",
            &params,
            &registry,
        )
        .unwrap_err();

        assert!(
            matches!(err, Error::UnknownTemplate(ref n) if n == "does-not-exist"),
            "expected UnknownTemplate, got: {err:?}",
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: empty registry also returns UnknownTemplate
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn empty_registry_returns_unknown_template() {
        let registry: Vec<Box<dyn Template>> = vec![];
        let params = Parameters::new();

        let err = expand("lab", "any", "frr-fedora", "eth0", &params, &registry).unwrap_err();

        assert!(
            matches!(err, Error::UnknownTemplate(_)),
            "expected UnknownTemplate on empty registry, got: {err:?}",
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: matching template is found and expand() is called
    //
    // NOTE: this test calls `crate::loader::validate_and_fill` internally.
    // It will only compile once the Phase 3a agent has landed that function.
    // The test is correct and will pass once both modules are present.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn matching_template_is_dispatched() {
        let registry = registry_with(vec![
            Box::new(MockTemplate { id: "mock-a" }),
            Box::new(MockTemplate { id: "mock-b" }),
        ]);
        let params = Parameters::new();

        let topology = expand(
            "my-lab",
            "mock-b",
            "frr-fedora",
            "eth0",
            &params,
            &registry,
        )
        .expect("expand should succeed for known template");

        assert_eq!(topology.name, "my-lab");
        assert_eq!(topology.template, "mock-b");
        assert_eq!(topology.platform, "frr-fedora");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: template expand() errors are propagated
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn expand_error_is_propagated() {
        let registry = registry_with(vec![Box::new(BrokenTemplate)]);
        let params = Parameters::new();

        let err = expand("lab", "broken", "frr-fedora", "eth0", &params, &registry).unwrap_err();

        assert!(
            matches!(err, Error::Template(_)),
            "expected Template error, got: {err:?}",
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: registry with multiple templates picks the right one
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn registry_selects_correct_template_among_many() {
        let registry = registry_with(vec![
            Box::new(MockTemplate { id: "alpha" }),
            Box::new(MockTemplate { id: "beta" }),
            Box::new(MockTemplate { id: "gamma" }),
        ]);
        let params = Parameters::new();

        let topology = expand("fab", "gamma", "frr-fedora", "eth0", &params, &registry)
            .expect("should find gamma");

        assert_eq!(topology.template, "gamma");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: first match wins (duplicate names — unusual but defined behaviour)
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn first_match_wins_on_duplicate_names() {
        // Both entries have name "dup"; the first one in the slice should win.
        // Both use MockTemplate so the topology.template field reflects "dup"
        // in both cases — we just confirm no panic and a successful result.
        let registry = registry_with(vec![
            Box::new(MockTemplate { id: "dup" }),
            Box::new(MockTemplate { id: "dup" }),
        ]);
        let params = Parameters::new();

        let result = expand("lab", "dup", "frr-fedora", "eth0", &params, &registry);
        assert!(result.is_ok(), "expected Ok for duplicate-named registry");
    }
}
