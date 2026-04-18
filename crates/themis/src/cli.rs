//! Clap argument tree — all subcommands and global flags.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

// ── Top-level command ─────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "themis",
    about = "Themis — declarative network fabric lab",
    long_about = "themis controls themisd, the Themis fabric daemon.\n\n\
                  Labs are defined from a Themisfile, deployed onto KVM, and \
                  inspected or torn down with the subcommands below.",
    version,
    propagate_version = true,
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Override the socket path for themisd.
    /// Defaults to $THEMIS_SOCKET, then $XDG_RUNTIME_DIR/themisd.sock.
    #[arg(long, global = true, env = "THEMIS_SOCKET", value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Emit machine-readable JSON instead of pretty text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Do not auto-start themisd if the socket is absent. Fail instead.
    #[arg(long, global = true)]
    pub no_auto_start: bool,

    /// Increase log verbosity. Pass twice for trace.
    #[arg(short = 'v', action = ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

// ── Subcommand tree ───────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
pub enum Command {
    // ── Lab lifecycle ─────────────────────────────────────────────────────────
    /// Parse and register a Themisfile with the daemon.
    Define {
        /// Path to the Themisfile to register.
        themisfile: PathBuf,
    },

    /// List all defined labs.
    List,

    /// Show detailed information about a lab.
    Inspect {
        /// Lab name.
        name: String,
    },

    /// Deploy (provision and start) a lab.
    Deploy {
        /// Lab name.
        name: String,

        /// Tail live events until the lab reaches a terminal state.
        #[arg(short, long)]
        follow: bool,
    },

    /// Destroy a running or defined lab.
    Destroy {
        /// Lab name.
        name: String,

        /// Tail live events until destruction completes.
        #[arg(short, long)]
        follow: bool,
    },

    /// Show the resource estimate for a lab (vCPU, RAM, disk).
    Estimate {
        /// Lab name.
        name: String,
    },

    // ── Runtime control ───────────────────────────────────────────────────────
    /// Push NOS configuration to one or more nodes.
    PushConfig {
        /// Lab name.
        name: String,

        /// Specific node names to push to. Empty = all nodes.
        nodes: Vec<String>,
    },

    /// Run a chaos scenario against a lab.
    Chaos {
        /// Lab name.
        name: String,

        /// Chaos DSL scenario source string.
        scenario: String,
    },

    /// Pause all VMs in a lab (suspend).
    Pause {
        /// Lab name.
        name: String,
    },

    /// Resume a paused lab.
    Resume {
        /// Lab name.
        name: String,
    },

    // ── Event streaming ───────────────────────────────────────────────────────
    /// Tail live events from a lab.
    Logs {
        /// Lab name. Empty = all labs.
        name: Option<String>,

        /// Event kinds to include (comma-separated).
        /// One of: lab-state, node-state, chaos, error.
        /// Default: all kinds.
        #[arg(long, value_delimiter = ',', value_name = "KIND")]
        kinds: Vec<EventKindArg>,
    },

    // ── Daemon ────────────────────────────────────────────────────────────────
    /// Check daemon health.
    Health,

    /// Print version information (CLI + daemon).
    Version,

    /// Ask the daemon to shut down.
    Shutdown {
        /// Wait for in-flight work to finish before exiting.
        #[arg(long)]
        drain: bool,
    },

    // ── Local-only commands (no daemon required) ──────────────────────────────
    /// Scaffold a minimal Themisfile in the current directory.
    Init {
        /// Template name (clos-3tier, three-tier, hub-spoke).
        #[arg(long, default_value = "clos-3tier")]
        template: String,

        /// Platform name (frr-fedora, cumulus-vx).
        #[arg(long, default_value = "frr-fedora")]
        platform: String,

        /// Fabric name written into the generated file.
        #[arg(long, default_value = "my-lab")]
        name: String,

        /// Output path for the generated file.
        #[arg(long, short, default_value = "Themisfile")]
        output: PathBuf,
    },

    /// Parse and validate a Themisfile without contacting the daemon.
    Validate {
        /// Path to the Themisfile.
        themisfile: PathBuf,
    },

    /// Render an ASCII topology diagram from a Themisfile.
    Diagram {
        /// Path to the Themisfile.
        themisfile: PathBuf,

        /// Suppress color output even on a tty.
        #[arg(long)]
        no_color: bool,
    },

    /// Validate + estimate + preflight summary (no daemon, no host changes).
    Plan {
        /// Path to the Themisfile.
        themisfile: PathBuf,
    },

    /// Generate shell completion script.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

/// Event kind argument (matches proto EventKind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EventKindArg {
    #[value(name = "lab-state")]
    LabState,
    #[value(name = "node-state")]
    NodeState,
    Chaos,
    Error,
}

impl EventKindArg {
    pub fn to_proto(self) -> themis_proto::EventKind {
        match self {
            Self::LabState => themis_proto::EventKind::LabState,
            Self::NodeState => themis_proto::EventKind::NodeState,
            Self::Chaos => themis_proto::EventKind::Chaos,
            Self::Error => themis_proto::EventKind::Error,
        }
    }
}
