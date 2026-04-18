//! `themis estimate <NAME>` — LabService.Estimate

use anyhow::{Context as _, Result};
use tonic::Request;

use themis_proto::{lab_service_client::LabServiceClient, EstimateRequest};

use crate::output::{color_enabled, fmt_gb, fmt_mb, print_table, styled_table, OutputFormat};

pub async fn run(
    channel: tonic::transport::Channel,
    name: String,
    fmt: OutputFormat,
) -> Result<()> {
    let mut client = LabServiceClient::new(channel);
    let resp = client
        .estimate(Request::new(EstimateRequest { name: name.clone() }))
        .await
        .context("LabService.Estimate")?
        .into_inner();

    match fmt {
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "lab":                      name,
                "total_vcpu":               resp.total_vcpu,
                "nominal_memory_mb":        resp.nominal_memory_mb,
                "projected_memory_mb_ksm":  resp.projected_memory_mb_after_ksm,
                "total_disk_gb":            resp.total_disk_gb,
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
        OutputFormat::Pretty => {
            let color = color_enabled(false);
            let mut table = styled_table(&["RESOURCE", "NOMINAL", "AFTER KSM"], color);
            table.add_row(vec![
                comfy_table::Cell::new("vCPU"),
                comfy_table::Cell::new(resp.total_vcpu.to_string()),
                comfy_table::Cell::new(""),
            ]);
            table.add_row(vec![
                comfy_table::Cell::new("RAM"),
                comfy_table::Cell::new(fmt_mb(resp.nominal_memory_mb)),
                comfy_table::Cell::new(fmt_mb(resp.projected_memory_mb_after_ksm)),
            ]);
            table.add_row(vec![
                comfy_table::Cell::new("Disk"),
                comfy_table::Cell::new(fmt_gb(resp.total_disk_gb)),
                comfy_table::Cell::new(""),
            ]);
            print_table(&table);
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::output::{fmt_gb, fmt_mb};

    #[test]
    fn fmt_mb_and_gb_round_trip() {
        assert_eq!(fmt_mb(1024), "1.0 GiB");
        assert_eq!(fmt_mb(512), "512 MiB");
        assert_eq!(fmt_gb(20), "20 GiB");
    }
}
