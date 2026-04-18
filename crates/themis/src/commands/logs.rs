//! `themis logs [NAME] [--kinds k1,k2,...]` — StreamService.SubscribeEvents

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{stream_service_client::StreamServiceClient, EventKind, SubscribeRequest};

use crate::cli::EventKindArg;
use crate::output::{fmt_ts_ns, OutputFormat};

pub async fn run(
    channel: tonic::transport::Channel,
    name: Option<String>,
    kinds: Vec<EventKindArg>,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = StreamServiceClient::new(channel);

    let proto_kinds: Vec<i32> = kinds.iter().map(|k| k.to_proto() as i32).collect();

    let req = SubscribeRequest {
        lab: name.clone().unwrap_or_default(),
        kinds: proto_kinds,
    };

    let response = client
        .subscribe_events(Request::new(req))
        .await
        .context("StreamService.SubscribeEvents")?;
    let mut stream = response.into_inner();

    loop {
        match stream.message().await {
            Ok(Some(event)) => {
                let ts = fmt_ts_ns(event.timestamp_unix_ns);
                let kind_str = event_kind_str(event.kind);
                let subj = if event.subject.is_empty() {
                    event.lab.clone()
                } else {
                    event.subject.clone()
                };

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
            }
            Ok(None) => break,
            Err(e) => return Err(e).context("event stream error"),
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
