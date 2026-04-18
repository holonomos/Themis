//! `themis plan <THEMISFILE>` — validate + estimate + preflight summary.
//!
//! No daemon contact, no host changes. Gives the engineer a clear picture of
//! what a `deploy` will do before committing.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use themis_compiler::{estimator, expander, loader};

use crate::output::{color_enabled, fmt_gb, fmt_mb, print_table, styled_table, OutputFormat};

pub fn run(themisfile: PathBuf, fmt: OutputFormat) -> Result<()> {
    // 1. Parse.
    let doc = loader::parse_themisfile_from_path(&themisfile)
        .with_context(|| format!("parsing {}", themisfile.display()))?;

    // 2. Expand.
    let topology = expander::expand_with_builtins(
        &doc.name,
        &doc.template,
        &doc.platform,
        doc.wan_interface.as_deref().unwrap_or(""),
        &doc.parameters,
    )
    .with_context(|| format!("expanding topology for '{}'", doc.name))?;

    // 3. Estimate.
    let est = estimator::estimate_with_builtin_platforms(&topology, &doc.platform)
        .with_context(|| format!("estimating resources for platform '{}'", doc.platform))?;

    // 4. Collect bridges and DNS domain.
    let mut bridges: Vec<String> = topology
        .links
        .iter()
        .map(|l| l.bridge.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    bridges.sort();

    let dns_domain = &topology.management.dns_domain;
    let mgmt_bridge = &topology.management.bridge;
    let data_bridge = &topology.management.data_bridge;

    match fmt {
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "name":     doc.name,
                "template": doc.template,
                "platform": doc.platform,
                "estimate": {
                    "vcpu":           est.total_vcpu,
                    "nodes":          est.total_nodes,
                    "nominal_mem_mb": est.nominal_memory_mb,
                    "ksm_mem_mb":     est.projected_memory_mb_after_ksm,
                    "disk_gb":        est.total_disk_gb,
                },
                "bridges": bridges,
                "dns_domain": dns_domain,
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
        OutputFormat::Pretty => {
            let color = color_enabled(false);

            println!("Plan for '{}'\n", doc.name);
            println!("  Template:  {}", doc.template);
            println!("  Platform:  {}", doc.platform);
            println!("  DNS domain: {dns_domain}");
            println!();

            // Resource table.
            let mut table = styled_table(&["RESOURCE", "NOMINAL", "AFTER KSM"], color);
            table.add_row(vec![
                comfy_table::Cell::new("Nodes"),
                comfy_table::Cell::new(est.total_nodes.to_string()),
                comfy_table::Cell::new(""),
            ]);
            table.add_row(vec![
                comfy_table::Cell::new("vCPU"),
                comfy_table::Cell::new(est.total_vcpu.to_string()),
                comfy_table::Cell::new(""),
            ]);
            table.add_row(vec![
                comfy_table::Cell::new("RAM"),
                comfy_table::Cell::new(fmt_mb(est.nominal_memory_mb)),
                comfy_table::Cell::new(fmt_mb(est.projected_memory_mb_after_ksm)),
            ]);
            table.add_row(vec![
                comfy_table::Cell::new("Disk"),
                comfy_table::Cell::new(fmt_gb(est.total_disk_gb)),
                comfy_table::Cell::new(""),
            ]);
            print_table(&table);
            println!();

            // Bridges.
            println!("Host bridges to be created ({}):", bridges.len() + 2);
            println!("  {mgmt_bridge}  (management)");
            println!("  {data_bridge}  (data)");
            for b in &bridges {
                println!("  {b}");
            }
            println!();
            println!("No host changes made — this is a dry-run summary.");
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write_themisfile(content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "themis_plan_test_{}.kdl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn clos_valid() -> &'static str {
        r#"fabric "plan-lab" {
    template "clos-3tier"
    platform "frr-fedora"
    parameters {
        border_count     2
        spine_count      2
        rack_count       2
        servers_per_rack 2
    }
}"#
    }

    #[test]
    fn plan_succeeds_on_valid_themisfile() {
        let path = write_themisfile(clos_valid());
        let result = run(path.clone(), OutputFormat::Pretty);
        std::fs::remove_file(path).ok();
        assert!(result.is_ok(), "plan failed: {result:?}");
    }

    #[test]
    fn plan_json_succeeds() {
        let path = write_themisfile(clos_valid());
        let result = run(path.clone(), OutputFormat::Json);
        std::fs::remove_file(path).ok();
        assert!(result.is_ok(), "plan json failed: {result:?}");
    }

    #[test]
    fn plan_fails_on_invalid_themisfile() {
        let path = write_themisfile("bad kdl {{");
        let result = run(path.clone(), OutputFormat::Pretty);
        std::fs::remove_file(path).ok();
        assert!(result.is_err());
    }
}
