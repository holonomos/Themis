//! `themis list` — LabService.List

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{lab_service_client::LabServiceClient, ListRequest};

use crate::output::{color_enabled, fmt_ts_secs, lab_state_display, print_table, styled_table, OutputFormat};

pub async fn run(channel: tonic::transport::Channel, fmt: OutputFormat) -> Result<()> {
    let mut client = LabServiceClient::new(channel);
    let resp = client
        .list(Request::new(ListRequest {}))
        .await
        .context("LabService.List")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            let labs: Vec<serde_json::Value> = resp
                .labs
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "name":       l.name,
                        "template":   l.template,
                        "platform":   l.platform,
                        "state":      lab_state_display(l.state).0,
                        "nodes":      l.node_count,
                        "created":    fmt_ts_secs(l.created_unix),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&labs)?);
        }
        OutputFormat::Pretty => {
            if resp.labs.is_empty() {
                println!("No labs defined. Use `themis define <Themisfile>` to add one.");
                return Ok(());
            }

            let color = color_enabled(false);
            let mut table = styled_table(
                &["NAME", "TEMPLATE", "PLATFORM", "STATE", "NODES", "CREATED"],
                color,
            );

            for lab in &resp.labs {
                let (state_str, state_color) = lab_state_display(lab.state);
                let state_cell = if color {
                    if let Some(c) = state_color {
                        comfy_table::Cell::new(state_str).fg(c)
                    } else {
                        comfy_table::Cell::new(state_str)
                    }
                } else {
                    comfy_table::Cell::new(state_str)
                };

                table.add_row(vec![
                    comfy_table::Cell::new(&lab.name),
                    comfy_table::Cell::new(&lab.template),
                    comfy_table::Cell::new(&lab.platform),
                    state_cell,
                    comfy_table::Cell::new(lab.node_count.to_string()),
                    comfy_table::Cell::new(fmt_ts_secs(lab.created_unix)),
                ]);
            }

            print_table(&table);
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use themis_proto::{LabState, LabSummary};

    use crate::output::{fmt_ts_secs, lab_state_display};

    fn make_lab(name: &str, state: LabState) -> LabSummary {
        LabSummary {
            name: name.to_string(),
            template: "clos-3tier".to_string(),
            platform: "frr-fedora".to_string(),
            state: state as i32,
            node_count: 12,
            // 2026-04-17 00:00:00 UTC
            created_unix: 1_776_384_000,
        }
    }

    #[test]
    fn list_format_state_strings() {
        let running = make_lab("lab-a", LabState::Running);
        let (s, _) = lab_state_display(running.state);
        assert_eq!(s, "running");

        let failed = make_lab("lab-b", LabState::Failed);
        let (s, _) = lab_state_display(failed.state);
        assert_eq!(s, "failed");
    }

    #[test]
    fn list_format_timestamp() {
        // 2026-04-17 00:00:00 UTC = 1776384000
        let ts = fmt_ts_secs(1_776_384_000);
        assert_eq!(ts, "2026-04-17T00:00:00Z");
    }

    #[test]
    fn list_json_fields() {
        let lab = make_lab("test", LabState::Running);
        let val = serde_json::json!({
            "name":     lab.name,
            "template": lab.template,
            "platform": lab.platform,
            "state":    lab_state_display(lab.state).0,
            "nodes":    lab.node_count,
            "created":  fmt_ts_secs(lab.created_unix),
        });
        assert_eq!(val["state"], "running");
        assert_eq!(val["nodes"], 12);
        assert_eq!(val["name"], "test");
    }
}
