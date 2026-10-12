use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::adapter::claude_code::ClaudeCode;
use crate::ingest;
use crate::paths;
use crate::store::Store;

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
    let db = cli.db.clone().unwrap_or_else(paths::default_db);
    match cli.command {
        Command::Ingest { source, root, json } => {
            if source != "claude" {
                bail!("unsupported source {source:?} (M0 supports: claude)");
            }
            let root = root
                .or_else(paths::default_claude_root)
                .context("no transcript root: pass --root or set LEDGER_CLAUDE_ROOT")?;
            ingest::ensure_db_outside_root(&db, &root)?;
            let mut store = Store::open(&db)?;
            let summary = ingest::ingest(&mut store, &ClaudeCode, &root)?;
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&summary)?)?;
            } else {
                write!(out, "{}", summary.render())?;
            }
            Ok(())
        }
        Command::Report { .. } => {
            writeln!(out, "report: not implemented")?;
            Ok(())
        }
        Command::Doctor { .. } => {
            writeln!(out, "doctor: not implemented")?;
            Ok(())
        }
    }
}
