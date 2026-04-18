//! `themis push-config <NAME> [NODE]...` — RuntimeService.PushConfig

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{runtime_service_client::RuntimeServiceClient, PushConfigRequest};

use crate::output::OutputFormat;

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    nodes: Vec<String>,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = RuntimeServiceClient::new(channel);
    let resp = client
        .push_config(Request::new(PushConfigRequest {
            lab: name.clone(),
            nodes: nodes.clone(),
        }))
        .await
        .context("RuntimeService.PushConfig")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({
                "lab": name,
                "nodes_updated": resp.nodes_updated,
            }));
        }
        OutputFormat::Pretty => {
            let scope = if nodes.is_empty() {
                "all nodes".to_string()
            } else {
                nodes.join(", ")
            };
            println!(
                "Config pushed to {} node(s) in lab '{}' ({scope}).",
                resp.nodes_updated, name
            );
        }
    }
    Ok(())
}
