//! `themis inspect <NAME>` — LabService.Inspect

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{lab_service_client::LabServiceClient, InspectRequest};

use crate::output::{color_enabled, fmt_ts_secs, lab_state_display, OutputFormat};

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = LabServiceClient::new(channel);
    let resp = client
        .inspect(Request::new(InspectRequest { name }))
        .await
        .context("LabService.Inspect")?
        .into_inner();

    let summary = resp.summary.unwrap_or_default();
    let topology_json = String::from_utf8_lossy(&resp.topology_json);

    match fmt {
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "name":     summary.name,
                "template": summary.template,
                "platform": summary.platform,
                "state":    lab_state_display(summary.state).0,
                "nodes":    summary.node_count,
                "created":  fmt_ts_secs(summary.created_unix),
                "topology": if topology_json.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_str(&topology_json).unwrap_or(serde_json::Value::Null)
                },
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
        OutputFormat::Pretty => {
            let color = color_enabled(false);
            let (state_str, state_color) = lab_state_display(summary.state);

            // Header line.
            if color {
                if let Some(c) = state_color {
                    // Use comfy-table color codes via crossterm-style ANSI.
                    let ansi = color_to_ansi(c);
                    println!("Lab: \x1b[1m{}\x1b[0m  state: {ansi}{state_str}\x1b[0m", summary.name);
                } else {
                    println!("Lab: \x1b[1m{}\x1b[0m  state: {state_str}", summary.name);
                }
            } else {
                println!("Lab: {}  state: {state_str}", summary.name);
            }

            println!("  Template:  {}", summary.template);
            println!("  Platform:  {}", summary.platform);
            println!("  Nodes:     {}", summary.node_count);
            println!("  Created:   {}", fmt_ts_secs(summary.created_unix));

            if !topology_json.is_empty() {
                println!("\nTopology (JSON):\n{topology_json}");
            }
        }
    }
    Ok(())
}

fn color_to_ansi(c: comfy_table::Color) -> &'static str {
    match c {
        comfy_table::Color::Green => "\x1b[32m",
        comfy_table::Color::Red => "\x1b[31m",
        comfy_table::Color::Yellow => "\x1b[33m",
        comfy_table::Color::Cyan => "\x1b[36m",
        comfy_table::Color::DarkGrey => "\x1b[90m",
        _ => "",
    }
}
