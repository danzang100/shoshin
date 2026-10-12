//! Harness adapters: detect, discover, parse. Each one is best-effort and tolerant.
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::redact::Hasher;
use crate::schema::ParsedFile;

pub mod claude_code;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    /// Project slug (directory name under the root).
    pub project: String,
    /// Session uuid (file stem for main files, parent directory name for subagent files).
    pub session_id: String,
    pub is_subagent: bool,
    /// Subagent id derived from the file name (`agent-<id>.jsonl`).
    pub agent_id: Option<String>,
}

pub trait Adapter {
    fn name(&self) -> &'static str;
    /// True if `root` looks like a transcript root for this harness.
    fn detect(&self, root: &Path) -> bool;
    fn discover(&self, root: &Path) -> Result<Vec<DiscoveredFile>>;
    /// Parse one file. Never fails on malformed content, only on I/O.
    fn parse(&self, file: &DiscoveredFile, hasher: &Hasher) -> Result<ParsedFile>;
}
