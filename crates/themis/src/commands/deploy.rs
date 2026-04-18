//! `themis deploy <NAME> [--follow]` — LabService.Deploy + optional event tail.

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{
    lab_service_client::LabServiceClient, stream_service_client::StreamServiceClient,
    DeployRequest, EventKind, LabState, SubscribeRequest,
};

use crate::output::{fmt_ts_ns, lab_state_display, OutputFormat};

/// Terminal lab states that end a `--follow` tail.
fn is_terminal_lab_state(state: i32) -> bool {
    matches!(
        LabState::try_from(state).unwrap_or(LabState::Unspecified),
        LabState::Running | LabState::Destroyed | LabState::Failed
    )
}

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    follow: bool,
    fmt: OutputFormat,
) -> Result<()> {
    // 1. Kick off deploy.
    let mut client = LabServiceClient::new(channel.clone());
    client
        .deploy(Request::new(DeployRequest { name: name.clone() }))
        .await
        .context("LabService.Deploy")?;

    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "name": name, "action": "deploy", "status": "accepted" }));
        }
        OutputFormat::Pretty => {
            println!("Deploy accepted for lab '{name}'.");
        }
    }

    if !follow {
        return Ok(());
    }

    // 2. Stream events until terminal state or 30-minute ceiling.
    let timeout = tokio::time::Duration::from_secs(30 * 60);
    stream_until_terminal(channel, &name, fmt, timeout).await
}

/// Open a `SubscribeEvents` stream and print events until a terminal `LAB_STATE`
/// is observed or the timeout fires.
pub(crate) async fn stream_until_terminal(
    channel: tonic::transport::Channel,
    lab: &str,
    fmt: OutputFormat,
    timeout: tokio::time::Duration,
) -> Result<()> {
    let mut stream_client = StreamServiceClient::new(channel);

    let req = SubscribeRequest {
        lab: lab.to_string(),
        kinds: vec![],
    };

    let response = stream_client
        .subscribe_events(Request::new(req))
        .await
        .context("StreamService.SubscribeEvents")?;
    let mut stream = response.into_inner();

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("event stream timed out after 30 minutes");
        }

        let event = match tokio::time::timeout(remaining, stream.message()).await {
            Ok(Ok(Some(e))) => e,
            Ok(Ok(None)) => break, // stream closed
            Ok(Err(e)) => return Err(e).context("event stream error"),
            Err(_) => anyhow::bail!("event stream timed out"),
        };

        let ts = fmt_ts_ns(event.timestamp_unix_ns);
        let kind_str = event_kind_str(event.kind);
        let subj = if event.subject.is_empty() { lab.to_string() } else { event.subject.clone() };

        match fmt {
            OutputFormat::Json => {
                let obj = serde_json::json!({
                    "ts":      ts,
                    "lab":     event.lab,
                    "kind":    kind_str,
                    "subject": subj,
                    "message": event.message,
                });
                println!("{obj}");
            }
            OutputFormat::Pretty => {
                println!("{ts}  {subj:<20}  {kind_str:<12}  {}", event.message);
            }
        }

        // Check for terminal LAB_STATE events.
        if event.kind == EventKind::LabState as i32 {
            // Try to decode a state from the payload JSON.
            if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&event.payload) {
                if let Some(state_n) = payload.get("state").and_then(|s| s.as_i64()) {
                    if is_terminal_lab_state(state_n as i32) {
                        let (label, _) = lab_state_display(state_n as i32);
                        match fmt {
                            OutputFormat::Pretty => {
                                println!("\nLab reached terminal state: {label}");
                            }
                            OutputFormat::Json => {
                                println!("{}", serde_json::json!({ "terminal_state": label }));
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn event_kind_str(kind: i32) -> &'static str {
    match EventKind::try_from(kind).unwrap_or(EventKind::Unspecified) {
        EventKind::Unspecified => "unknown",
        EventKind::LabState => "LAB_STATE",
        EventKind::NodeState => "NODE_STATE",
        EventKind::Chaos => "CHAOS",
        EventKind::Error => "ERROR",
    }
}
