//! themis-tui — Themis terminal UI client.
//!
//! Populated in Phase 9 per `docs/WORK_PLAN.md`.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("themis-tui placeholder — implementation pending Phase 9");
    Ok(())
}
