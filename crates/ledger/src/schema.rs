//! Normalized records (parent spec section 7, M0 subset). Additive changes only.
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const SOURCE_CLAUDE_CODE: &str = "claude_code";
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub agent_id: String,
    pub source: String,
    pub source_version: Option<String>,
    pub project: String,
    pub project_name: String,
    pub cwd_hash: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub turn_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub is_subagent: bool,
    pub parent_session_id: Option<String>,
    pub project: String,
    pub project_name: String,
    pub source_version: Option<String>,
    pub model: String,
    pub ts: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_5m_tokens: Option<i64>,
    pub cache_write_1h_tokens: Option<i64>,
    pub stop_reason: Option<String>,
    pub is_api_error: bool,
    pub is_aborted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Ok,
    Error,
    Unknown,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Allowed,
    Denied,
    None,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_use_id: String,
    pub turn_id: String,
    pub session_id: String,
    pub agent_id: String,
    /// Order of appearance in the source file.
    pub seq: i64,
    pub ts: Option<String>,
    pub tool_name: String,
    pub args_hash: String,
    pub target_hash: Option<String>,
    pub result_status: ResultStatus,
    pub permission: Permission,
    pub denial_kind: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Prompt,
    ToolResult,
    Meta,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::ToolResult => "tool_result",
            Self::Meta => "meta",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSignal {
    pub session_id: String,
    pub agent_id: String,
    pub ts: Option<String>,
    pub kind: SignalKind,
}

/// Counters for one parsed file.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParseStats {
    pub lines: u64,
    pub bad_lines: u64,
    pub partial_tail: bool,
    /// Line `type` values other than the ones the adapter consumes, with counts.
    pub unknown_types: std::collections::BTreeMap<String, u64>,
}

/// Everything extracted from one transcript file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedFile {
    pub session: Option<Session>,
    pub turns: Vec<Turn>,
    pub tool_calls: Vec<ToolCall>,
    pub signals: Vec<UserSignal>,
    pub stats: ParseStats,
}
