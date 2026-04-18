//! themis — Themis CLI client.
//!
//! Every capability that `themisd` exposes has a subcommand here. Local-only
//! commands (validate, diagram, plan, init, completions) run without contacting
//! the daemon. All others lazy-start `themisd` when the socket is absent.

use anyhow::Result;
use clap::Parser as _;
use tracing_subscriber::{EnvFilter, fmt as tracing_fmt};

mod cli;
mod client;
mod commands;
mod output;

use cli::{Cli, Command};
use client::{connect_or_start, resolve_socket};
use output::OutputFormat;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    // ── Tracing setup ─────────────────────────────────────────────────────────
    let level = match args.verbose {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    // Honour RUST_LOG if set; our verbosity flags override.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));
    tracing_fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let fmt = OutputFormat::from_flag(args.json);

    // ── Dispatch ──────────────────────────────────────────────────────────────
    match args.command {
        // ── Local-only commands (no daemon needed) ────────────────────────────
        Command::Init { template, platform, name, output } => {
            commands::init::run(template, platform, name, output, fmt)?;
        }
        Command::Validate { themisfile } => {
            commands::validate::run(themisfile, fmt)?;
        }
        Command::Diagram { themisfile, no_color } => {
            commands::diagram::run(themisfile, no_color, fmt)?;
        }
        Command::Plan { themisfile } => {
            commands::plan::run(themisfile, fmt)?;
        }
        Command::Completions { shell } => {
            commands::completions::run(shell)?;
        }

        // ── Daemon-backed commands ────────────────────────────────────────────
        command => {
            let socket = resolve_socket(args.socket);
            let auto_start = !args.no_auto_start;
            let client = connect_or_start(socket, auto_start).await?;
            let ch = client.channel;

            match command {
                Command::Define { themisfile } => {
                    commands::define::run(ch, themisfile, fmt).await?;
                }
                Command::List => {
                    commands::list::run(ch, fmt).await?;
                }
                Command::Inspect { name } => {
                    commands::inspect::run(ch, name, fmt).await?;
                }
                Command::Deploy { name, follow } => {
                    commands::deploy::run(ch, name, follow, fmt).await?;
                }
                Command::Destroy { name, follow } => {
                    commands::destroy::run(ch, name, follow, fmt).await?;
                }
                Command::Estimate { name } => {
                    commands::estimate::run(ch, name, fmt).await?;
                }
                Command::PushConfig { name, nodes } => {
                    commands::push_config::run(ch, name, nodes, fmt).await?;
                }
                Command::Chaos { name, scenario } => {
                    commands::chaos::run(ch, name, scenario, fmt).await?;
                }
                Command::Pause { name } => {
                    commands::pause::pause(ch, name, fmt).await?;
                }
                Command::Resume { name } => {
                    commands::pause::resume(ch, name, fmt).await?;
                }
                Command::Logs { name, kinds } => {
                    commands::logs::run(ch, name, kinds, fmt).await?;
                }
                Command::Health => {
                    commands::health::run(ch, fmt).await?;
                }
                Command::Version => {
                    commands::version::run(Some(ch), fmt).await?;
                }
                Command::Shutdown { drain } => {
                    commands::shutdown::run(ch, drain, fmt).await?;
                }
                // Local-only commands were handled above; this arm is unreachable.
                Command::Init { .. }
                | Command::Validate { .. }
                | Command::Diagram { .. }
                | Command::Plan { .. }
                | Command::Completions { .. } => unreachable!(),
            }
        }
    }

    Ok(())
}
