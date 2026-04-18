//! `themis shutdown [--drain]` — DaemonService.Shutdown

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{daemon_service_client::DaemonServiceClient, ShutdownRequest};

use crate::output::OutputFormat;

pub async fn run(
    channel: tonic::transport::Channel,
    drain: bool,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = DaemonServiceClient::new(channel);
    client
        .shutdown(Request::new(ShutdownRequest { drain }))
        .await
        .context("DaemonService.Shutdown")?;

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "action": "shutdown", "drain": drain }));
        }
        OutputFormat::Pretty => {
            if drain {
                println!("Shutdown requested (draining in-flight work).");
            } else {
                println!("Shutdown requested.");
            }
        }
    }
    Ok(())
}
