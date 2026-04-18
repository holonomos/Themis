//! `themis pause <NAME>` / `themis resume <NAME>` — RuntimeService.Pause/Resume

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{runtime_service_client::RuntimeServiceClient, PauseRequest, ResumeRequest};

use crate::output::OutputFormat;

pub async fn pause(
    channel: tonic::transport::Channel,
    name: String,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = RuntimeServiceClient::new(channel);
    client
        .pause(Request::new(PauseRequest { lab: name.clone() }))
        .await
        .context("RuntimeService.Pause")?;

    match fmt {
        OutputFormat::Json => println!("{}", serde_json::json!({ "lab": name, "action": "paused" })),
        OutputFormat::Pretty => println!("Lab '{name}' paused."),
    }
    Ok(())
}

pub async fn resume(
    channel: tonic::transport::Channel,
    name: String,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = RuntimeServiceClient::new(channel);
    client
        .resume(Request::new(ResumeRequest { lab: name.clone() }))
        .await
        .context("RuntimeService.Resume")?;

    match fmt {
        OutputFormat::Json => println!("{}", serde_json::json!({ "lab": name, "action": "resumed" })),
        OutputFormat::Pretty => println!("Lab '{name}' resumed."),
    }
    Ok(())
}
