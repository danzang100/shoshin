//! Default locations. Everything is overridable by flag or environment variable.
use std::path::PathBuf;

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// Where Claude Code writes transcripts. Only ever read.
pub fn default_claude_root() -> Option<PathBuf> {
    home().map(|h| h.join(".claude").join("projects"))
}

/// Directory holding the database and the user pricing override.
pub fn data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local").join("share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("ledger")
}

pub fn default_db() -> PathBuf {
    data_dir().join("ledger.db")
}

pub fn default_pricing_override() -> PathBuf {
    data_dir().join("pricing.toml")
}
