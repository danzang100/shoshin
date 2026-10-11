//! Payloads that cross the Rust/webview boundary (spec 002, section 8). Field names are
//! snake_case throughout; Tauri only converts command *argument* names.
use serde::{Deserialize, Serialize};

use crate::event::{Event, SessionStatus};
use crate::summary::Summary;

/// Whether state-changing tools need the user's approval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask the user for each state-changing call.
    #[default]
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartRequest {
    /// Absolute path of the workspace folder.
    pub workspace: String,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfoDto {
    pub session_id: String,
    pub workspace: String,
    pub provider: String,
    pub model: String,
    pub permission_mode: PermissionMode,
}

/// Which session is live, so a reloaded UI can rebuild its timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveInfoDto {
    pub info: SessionInfoDto,
    pub running: bool,
    /// Highest event `seq` forwarded so far; ask for `session_events(after_seq)` to catch up.
    pub last_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequestDto {
    pub request_id: String,
    pub session_id: String,
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnFinishedDto {
    pub session_id: String,
    pub status: SessionStatus,
    /// Redacted, user-presentable error text (for `llm_error` and `internal_error`).
    pub error: Option<String>,
}

/// What the settings screen shows. The API key itself never appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsDto {
    pub model: String,
    pub default_model: String,
    pub known_models: Vec<String>,
    pub permission_mode: PermissionMode,
    pub recent_workspaces: Vec<String>,
    pub last_workspace: Option<String>,
    pub key_present: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveSettingsDto {
    pub model: String,
    pub permission_mode: PermissionMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogIssuesDto {
    pub truncated_last_line: bool,
    pub bad_lines: Vec<usize>,
}

/// A recorded session for the history view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedSessionDto {
    pub summary: Summary,
    pub events: Vec<Event>,
    pub log_issues: LogIssuesDto,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mode_is_snake_case_and_defaults_to_ask() {
        assert_eq!(serde_json::to_string(&PermissionMode::Allow).unwrap(), "\"allow\"");
        assert_eq!(PermissionMode::default(), PermissionMode::Ask);
    }

    #[test]
    fn dtos_round_trip() {
        let req = StartRequest { workspace: "C:\\w".into(), model: None, permission_mode: Some(PermissionMode::Deny) };
        let back: StartRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);
        let done = TurnFinishedDto { session_id: "s".into(), status: SessionStatus::Cancelled, error: None };
        let back: TurnFinishedDto = serde_json::from_str(&serde_json::to_string(&done).unwrap()).unwrap();
        assert_eq!(done, back);
    }
}
