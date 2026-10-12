use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ledger", version, about = "Which model is worth using for which task, from your own transcripts")]
pub struct Cli {
    /// SQLite database path (default: platform data dir).
    #[arg(long, global = true, env = "LEDGER_DB")]
    pub db: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Read transcripts and store normalized rows.
    Ingest {
        #[arg(long, default_value = "claude")]
        source: String,
        #[arg(long, env = "LEDGER_CLAUDE_ROOT")]
        root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Per-model (or repo / session) tokens, cost, tool errors, re-reads.
    Report {
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value = "model")]
        by: String,
        #[arg(long, default_value = "table")]
        format: String,
        #[arg(long)]
        exclude_subagents: bool,
        #[arg(long)]
        include_synthetic: bool,
        #[arg(long)]
        pricing: Option<PathBuf>,
    },
    /// Parser and data health.
    Doctor {
        #[arg(long)]
        pricing: Option<PathBuf>,
    },
}

pub fn run<W: Write>(cli: Cli, out: &mut W) -> Result<()> {
    let name = match cli.command {
        Command::Ingest { .. } => "ingest",
        Command::Report { .. } => "report",
        Command::Doctor { .. } => "doctor",
    };
    writeln!(out, "{name}: not implemented")?;
    Ok(())
}
