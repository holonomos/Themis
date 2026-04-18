//! `themis validate <THEMISFILE>` — local-only parse + schema check.
//!
//! Runs: parse_themisfile → validate_and_fill → expand_with_builtins.
//! No daemon contact.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use themis_compiler::{expander, loader};

use crate::output::OutputFormat;

pub fn run(themisfile: PathBuf, fmt: OutputFormat) -> Result<()> {
    // 1. Parse.
    let doc = loader::parse_themisfile_from_path(&themisfile)
        .with_context(|| format!("parsing {}", themisfile.display()))?;

    // 2. Expand (validates params + template).
    expander::expand_with_builtins(
        &doc.name,
        &doc.template,
        &doc.platform,
        doc.wan_interface.as_deref().unwrap_or(""),
        &doc.parameters,
    )
    .with_context(|| format!("expanding topology for '{}'", doc.name))?;

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({
                "ok": true,
                "name": doc.name,
                "template": doc.template,
                "platform": doc.platform,
            }));
        }
        OutputFormat::Pretty => {
            println!(
                "OK  {name}  ({tmpl} / {plat})",
                name = doc.name,
                tmpl = doc.template,
                plat = doc.platform,
            );
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
            "themis_validate_test_{}.kdl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn clos_valid() -> &'static str {
        r#"fabric "test-lab" {
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
    fn valid_themisfile_succeeds() {
        let path = write_themisfile(clos_valid());
        let result = run(path.clone(), OutputFormat::Pretty);
        std::fs::remove_file(path).ok();
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
    }

    #[test]
    fn invalid_kdl_fails() {
        let path = write_themisfile("this is not { valid kdl !!!");
        let result = run(path.clone(), OutputFormat::Pretty);
        std::fs::remove_file(path).ok();
        assert!(result.is_err());
    }

    #[test]
    fn unknown_template_fails() {
        let content = r#"fabric "lab" {
    template "nonexistent-template"
    platform "frr-fedora"
    parameters {}
}"#;
        let path = write_themisfile(content);
        let result = run(path.clone(), OutputFormat::Pretty);
        std::fs::remove_file(path).ok();
        assert!(result.is_err());
    }

    #[test]
    fn json_output_on_success() {
        let path = write_themisfile(clos_valid());
        // This just checks it doesn't panic; we can't capture stdout in unit tests easily.
        let result = run(path.clone(), OutputFormat::Json);
        std::fs::remove_file(path).ok();
        assert!(result.is_ok());
    }
}
