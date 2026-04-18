//! `themis version` — DaemonService.Version + local CLI version.

use anyhow::Result;
use tonic::Request;

use themis_proto::{daemon_service_client::DaemonServiceClient, VersionRequest};

use crate::output::OutputFormat;

/// Version baked in at compile time from Cargo.toml.
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(
    channel: Option<tonic::transport::Channel>,
    fmt: OutputFormat,
) -> Result<()> {
    // Daemon version (may fail if daemon unavailable — that's OK).
    let daemon_version = if let Some(ch) = channel {
        let mut client = DaemonServiceClient::new(ch);
        match client.version(Request::new(VersionRequest {})).await {
            Ok(resp) => {
                let r = resp.into_inner();
                Some((r.version, r.git_commit))
            }
            Err(e) => {
                tracing::warn!("could not fetch daemon version: {e}");
                None
            }
        }
    } else {
        None
    };

    match fmt {
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "cli_version": CLI_VERSION,
                "daemon": daemon_version.as_ref().map(|(v, c)| serde_json::json!({
                    "version": v,
                    "git_commit": c,
                })),
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
        OutputFormat::Pretty => {
            println!("themis CLI   {CLI_VERSION}");
            match daemon_version {
                Some((v, commit)) => {
                    let short = if commit.len() > 7 { &commit[..7] } else { &commit };
                    println!("themisd      {v} ({short})");
                }
                None => println!("themisd      (unavailable)"),
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::CLI_VERSION;

    #[test]
    fn cli_version_is_semver() {
        // Basic sanity: should look like "0.1.0" or similar.
        assert!(!CLI_VERSION.is_empty());
        let parts: Vec<&str> = CLI_VERSION.split('.').collect();
        assert!(parts.len() >= 2, "version should have at least major.minor");
    }
}
