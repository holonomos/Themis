//! `themis chaos <NAME> <SCENARIO>` — RuntimeService.RunChaos

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{runtime_service_client::RuntimeServiceClient, RunChaosRequest};

use crate::output::OutputFormat;

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    scenario: String,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = RuntimeServiceClient::new(channel);
    let resp = client
        .run_chaos(Request::new(RunChaosRequest {
            lab: name.clone(),
            scenario: scenario.clone(),
        }))
        .await
        .context("RuntimeService.RunChaos")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({
                "lab":      name,
                "chaos_id": resp.chaos_id,
            }));
        }
        OutputFormat::Pretty => {
            println!("Chaos scenario started in lab '{name}'.  id: {}", resp.chaos_id);
        }
    }
    Ok(())
}
