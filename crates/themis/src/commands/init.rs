//! `themis init` — scaffold a minimal Themisfile.
//!
//! Flag-driven; no TTY prompts. Use `--template`, `--platform`, `--name`,
//! and `--output` to control the output.

use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};

use crate::output::OutputFormat;

/// Templates we know how to scaffold.
const KNOWN_TEMPLATES: &[(&str, &str)] = &[
    ("clos-3tier",  CLOS_3TIER_TEMPLATE),
    ("three-tier",  THREE_TIER_TEMPLATE),
    ("hub-spoke",   HUB_SPOKE_TEMPLATE),
];

/// Platforms we know about.
const KNOWN_PLATFORMS: &[&str] = &["frr-fedora", "cumulus-vx"];

pub fn run(
    template: String,
    platform: String,
    name: String,
    output: PathBuf,
    fmt: OutputFormat,
) -> Result<()> {
    // Validate template.
    let tmpl_body = KNOWN_TEMPLATES
        .iter()
        .find(|(t, _)| *t == template)
        .map(|(_, body)| *body)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown template '{}'. Available: {}",
                template,
                KNOWN_TEMPLATES.iter().map(|(t, _)| *t).collect::<Vec<_>>().join(", ")
            )
        })?;

    // Validate platform.
    if !KNOWN_PLATFORMS.contains(&platform.as_str()) {
        bail!(
            "unknown platform '{}'. Available: {}",
            platform,
            KNOWN_PLATFORMS.join(", ")
        );
    }

    if output.exists() {
        bail!("'{}' already exists. Remove it or choose a different --output path.", output.display());
    }

    // Render the Themisfile.
    let content = tmpl_body
        .replace("{{NAME}}", &name)
        .replace("{{TEMPLATE}}", &template)
        .replace("{{PLATFORM}}", &platform);

    std::fs::write(&output, &content)
        .with_context(|| format!("writing {}", output.display()))?;

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({
                "path":     output.to_string_lossy(),
                "name":     name,
                "template": template,
                "platform": platform,
            }));
        }
        OutputFormat::Pretty => {
            println!("Wrote Themisfile to '{}'.", output.display());
            println!("Edit it, then run:  themis define {}", output.display());
        }
    }
    Ok(())
}

// ── Template scaffolds ────────────────────────────────────────────────────────

const CLOS_3TIER_TEMPLATE: &str = r#"fabric "{{NAME}}" {
    template "{{TEMPLATE}}"
    platform "{{PLATFORM}}"

    // Uncomment to enable outbound NAT via the host's WAN interface.
    // wan-interface "eth0"

    parameters {
        border_count      2   // 1–4 border routers
        spine_count       2   // 1–4 spines
        rack_count        4   // 1–8 racks
        servers_per_rack  2   // 1–8 servers per rack
    }
}
"#;

const THREE_TIER_TEMPLATE: &str = r#"fabric "{{NAME}}" {
    template "{{TEMPLATE}}"
    platform "{{PLATFORM}}"

    // wan-interface "eth0"

    parameters {
        core_count        2   // 1–4 core routers
        dist_count        4   // 2–8 distribution switches (must be even)
        access_per_dist   2   // 1–4 access switches per dist pair
        servers_per_access 2  // 1–8 servers per access switch
    }
}
"#;

const HUB_SPOKE_TEMPLATE: &str = r#"fabric "{{NAME}}" {
    template "{{TEMPLATE}}"
    platform "{{PLATFORM}}"

    // wan-interface "eth0"

    parameters {
        branch_count      3   // 1–8 branch sites
        redundant_hub     false  // true = dual hub for HA
    }
}
"#;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clos_template_renders_name() {
        let content = CLOS_3TIER_TEMPLATE
            .replace("{{NAME}}", "my-lab")
            .replace("{{TEMPLATE}}", "clos-3tier")
            .replace("{{PLATFORM}}", "frr-fedora");
        assert!(content.contains(r#"fabric "my-lab""#));
        assert!(content.contains(r#"template "clos-3tier""#));
        assert!(content.contains(r#"platform "frr-fedora""#));
    }

    #[test]
    fn known_templates_are_listed() {
        let names: Vec<&str> = KNOWN_TEMPLATES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"clos-3tier"));
        assert!(names.contains(&"three-tier"));
        assert!(names.contains(&"hub-spoke"));
    }

    #[test]
    fn run_fails_on_unknown_template() {
        let tmp = std::env::temp_dir().join("themis_init_test_unknown.kdl");
        let result = run(
            "nonexistent".to_string(),
            "frr-fedora".to_string(),
            "lab".to_string(),
            tmp,
            OutputFormat::Pretty,
        );
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("unknown template"));
    }

    #[test]
    fn run_fails_on_unknown_platform() {
        let tmp = std::env::temp_dir().join("themis_init_test_bad_plat.kdl");
        let result = run(
            "clos-3tier".to_string(),
            "bad-platform".to_string(),
            "lab".to_string(),
            tmp,
            OutputFormat::Pretty,
        );
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("unknown platform"));
    }

    #[test]
    fn run_writes_file() {
        let tmp = std::env::temp_dir().join("themis_init_test_writes.kdl");
        let _ = std::fs::remove_file(&tmp);
        run(
            "clos-3tier".to_string(),
            "frr-fedora".to_string(),
            "test-lab".to_string(),
            tmp.clone(),
            OutputFormat::Pretty,
        )
        .expect("should write file");
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("test-lab"));
        std::fs::remove_file(tmp).ok();
    }
}
