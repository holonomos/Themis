//! `themis health` — DaemonService.Health

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{daemon_service_client::DaemonServiceClient, HealthRequest};

use crate::output::OutputFormat;

pub async fn run(channel: tonic::transport::Channel, fmt: OutputFormat) -> Result<()> {
    let mut client = DaemonServiceClient::new(channel);
    let resp = client
        .health(Request::new(HealthRequest {}))
        .await
        .context("DaemonService.Health")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "ready": resp.ready }));
        }
        OutputFormat::Pretty => {
            if resp.ready {
                println!("themisd is ready.");
            } else {
                println!("themisd is running but not ready.");
            }
        }
    }
    Ok(())
}
