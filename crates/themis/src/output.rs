//! Output formatting helpers.
//!
//! Every command renders through two paths:
//!   - Pretty: aligned tables, selective color, human-readable.
//!   - JSON:   one object/array per invocation, then newline.
//!
//! Color is only applied when stdout is a tty (or when `force_color` is set).

use std::io::IsTerminal as _;

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};

// ── OutputFormat ─────────────────────────────────────────────────────────────

/// Governs whether a command emits pretty text or machine-readable JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Pretty,
    Json,
}

impl OutputFormat {
    pub fn from_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Pretty }
    }
}

// ── Color detection ───────────────────────────────────────────────────────────

/// True when stdout supports ANSI color.
pub fn color_enabled(force_no_color: bool) -> bool {
    if force_no_color {
        return false;
    }
    std::io::stdout().is_terminal()
}

// ── Lab state → display string ────────────────────────────────────────────────

/// Return a display string and an optional color for a LabState value.
pub fn lab_state_display(state: i32) -> (&'static str, Option<Color>) {
    use themis_proto::LabState;
    match LabState::try_from(state).unwrap_or(LabState::Unspecified) {
        LabState::Unspecified => ("unknown", None),
        LabState::Defined => ("defined", Some(Color::Cyan)),
        LabState::Provisioning => ("provisioning", Some(Color::Yellow)),
        LabState::Running => ("running", Some(Color::Green)),
        LabState::Paused => ("paused", Some(Color::Yellow)),
        LabState::Destroying => ("destroying", Some(Color::Yellow)),
        LabState::Destroyed => ("destroyed", Some(Color::DarkGrey)),
        LabState::Failed => ("failed", Some(Color::Red)),
    }
}

// ── Table builder ─────────────────────────────────────────────────────────────

/// A pre-styled comfy-table ready for use.
pub fn styled_table(headers: &[&str], color: bool) -> Table {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let header_cells: Vec<Cell> = headers
        .iter()
        .map(|h| {
            let cell = Cell::new(h).set_alignment(CellAlignment::Left);
            if color {
                cell.add_attribute(Attribute::Bold)
            } else {
                cell
            }
        })
        .collect();

    table.set_header(header_cells);
    table
}

/// Emit the table with no surrounding borders — cleaner for CLI.
pub fn print_table(table: &Table) {
    println!("{table}");
}

// ── Timestamp formatting ──────────────────────────────────────────────────────

/// Format a Unix nanosecond timestamp as `2026-04-17T15:34:12Z`.
pub fn fmt_ts_ns(ns: i64) -> String {
    if ns == 0 {
        return "—".to_string();
    }
    let secs = ns / 1_000_000_000;
    fmt_ts_secs(secs)
}

/// Format a Unix second timestamp as `2026-04-17T15:34:12Z`.
pub fn fmt_ts_secs(secs: i64) -> String {
    // Hand-rolled ISO-8601 to avoid a chrono/time dep.
    if secs <= 0 {
        return "—".to_string();
    }
    // Days since epoch, time of day.
    let s = secs as u64;
    let time_of_day = s % 86400;
    let days = s / 86400;

    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let sec = time_of_day % 60;

    // Compute calendar date from days since 1970-01-01.
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{sec:02}Z")
}

/// Convert days since epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

// ── MB/GB human-readable formatting ──────────────────────────────────────────

pub fn fmt_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GiB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MiB")
    }
}

pub fn fmt_gb(gb: u64) -> String {
    format!("{gb} GiB")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_ts_secs_epoch() {
        // 2026-04-17T15:34:12Z
        // 2026-04-17 00:00:00 UTC = 1776384000
        let ts = 1_776_384_000i64 + 15 * 3600 + 34 * 60 + 12;
        let s = fmt_ts_secs(ts);
        assert_eq!(s, "2026-04-17T15:34:12Z");
    }

    #[test]
    fn fmt_ts_secs_zero_returns_dash() {
        assert_eq!(fmt_ts_secs(0), "—");
    }

    #[test]
    fn fmt_mb_below_gib() {
        assert_eq!(fmt_mb(512), "512 MiB");
    }

    #[test]
    fn fmt_mb_above_gib() {
        assert_eq!(fmt_mb(2048), "2.0 GiB");
    }

    #[test]
    fn fmt_gb_basic() {
        assert_eq!(fmt_gb(10), "10 GiB");
    }

    #[test]
    fn lab_state_running_is_green() {
        use themis_proto::LabState;
        let (label, color) = lab_state_display(LabState::Running as i32);
        assert_eq!(label, "running");
        assert_eq!(color, Some(Color::Green));
    }

    #[test]
    fn lab_state_failed_is_red() {
        use themis_proto::LabState;
        let (label, color) = lab_state_display(LabState::Failed as i32);
        assert_eq!(label, "failed");
        assert_eq!(color, Some(Color::Red));
    }
}
