//! `themis destroy <NAME> [--follow]` — LabService.Destroy + optional event tail.

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{lab_service_client::LabServiceClient, DestroyRequest};

use crate::commands::deploy::stream_until_terminal;
use crate::output::OutputFormat;

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    follow: bool,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = LabServiceClient::new(channel.clone());
    client
        .destroy(Request::new(DestroyRequest { name: name.clone() }))
        .await
        .context("LabService.Destroy")?;

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "name": name, "action": "destroy", "status": "accepted" }));
        }
        OutputFormat::Pretty => {
            println!("Destroy accepted for lab '{name}'.");
        }
    }

    if !follow {
        return Ok(());
    }

    let timeout = tokio::time::Duration::from_secs(30 * 60);
    stream_until_terminal(channel, &name, fmt, timeout).await
}
