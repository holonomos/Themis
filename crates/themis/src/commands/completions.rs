//! `themis completions <SHELL>` — shell completion generator.

use anyhow::Result;
use clap::CommandFactory as _;
use clap_complete::{generate, Shell};

use crate::cli::Cli;

pub fn run(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "themis", &mut std::io::stdout());
    Ok(())
}
