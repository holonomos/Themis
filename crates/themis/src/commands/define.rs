//! `themis define <THEMISFILE>` — LabService.Define

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{lab_service_client::LabServiceClient, DefineRequest};

use crate::output::OutputFormat;

pub async fn run(
    channel: tonic::transport::Channel,
    themisfile: PathBuf,
    fmt: OutputFormat,
) -> Result<()> {
    let content = std::fs::read_to_string(&themisfile)
        .with_context(|| format!("reading {}", themisfile.display()))?;

    let mut client = LabServiceClient::new(channel);
    let resp = client
        .define(Request::new(DefineRequest {
            themisfile_path: themisfile.to_string_lossy().to_string(),
            themisfile_content: content,
        }))
        .await
        .context("LabService.Define")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            let obj = serde_json::json!({ "name": resp.name });
            println!("{obj}");
        }
        OutputFormat::Pretty => {
            println!("Lab '{}' defined.", resp.name);
        }
    }
    Ok(())
}
