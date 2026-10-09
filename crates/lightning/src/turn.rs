//! Provider-neutral conversation types (spec 001, section 5).
//!
//! Adapters translate to and from these; the session only ever sees these.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolError, ToolOutput};

/// One entry of the conversation history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Turn {
    User {
        text: String,
    },
    Assistant {
        text: Option<String>,
        tool_uses: Vec<ToolUse>,
        /// Provider items kept verbatim and sent back unmodified on the next request
        /// (for Gemini: thought, function_call and model_output steps).
        raw: Vec<RawItem>,
    },
    Tool {
        results: Vec<ToolResult>,
    },
}

impl Turn {
    /// Tool calls requested by this turn; empty for anything but an assistant turn.
    pub fn tool_uses(&self) -> &[ToolUse] {
        match self {
            Turn::Assistant { tool_uses, .. } => tool_uses,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub name: String,
    pub outcome: ToolOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolOutcome {
    Ok {
        content: String,
        metadata: Value,
        truncated: bool,
    },
    Error {
        class: ErrorClass,
        message: String,
    },
}

impl ToolOutcome {
    pub fn is_error(&self) -> bool {
        matches!(self, ToolOutcome::Error { .. })
    }
}

impl From<ToolOutput> for ToolOutcome {
    fn from(out: ToolOutput) -> Self {
        ToolOutcome::Ok { content: out.content, metadata: out.metadata, truncated: false }
    }
}

impl From<ToolError> for ToolOutcome {
    fn from(err: ToolError) -> Self {
        ToolOutcome::Error { class: err.class(), message: err.to_string() }
    }
}

/// Failure category shown to both the model and the logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    NotFound,
    PermissionDenied,
    OutsideWorkspace,
    InvalidArguments,
    UnknownTool,
    Io,
    Timeout,
    ExecutionFailed,
    /// The permission gate (or the user) declined the call.
    Denied,
    Internal,
}

/// Opaque provider item. Never inspected by the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawItem(pub Value);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_history() -> Vec<Turn> {
        vec![
            Turn::User { text: "list files".into() },
            Turn::Assistant {
                text: None,
                tool_uses: vec![ToolUse {
                    id: "call_1".into(),
                    name: "list_dir".into(),
                    arguments: json!({ "dir_path": "." }),
                }],
                raw: vec![RawItem(json!({ "type": "thought", "signature": "<sig>" }))],
            },
            Turn::Tool {
                results: vec![ToolResult {
                    tool_use_id: "call_1".into(),
                    name: "list_dir".into(),
                    outcome: ToolOutcome::Error {
                        class: ErrorClass::OutsideWorkspace,
                        message: "nope".into(),
                    },
                }],
            },
        ]
    }

    #[test]
    fn history_round_trips_through_json() {
        let history = sample_history();
        let text = serde_json::to_string(&history).unwrap();
        let back: Vec<Turn> = serde_json::from_str(&text).unwrap();
        assert_eq!(history, back);
    }

    #[test]
    fn raw_item_serializes_as_the_bare_value() {
        let item = RawItem(json!({ "type": "thought" }));
        assert_eq!(serde_json::to_value(&item).unwrap(), json!({ "type": "thought" }));
    }

    #[test]
    fn tool_uses_only_on_assistant_turns() {
        let history = sample_history();
        assert!(history[0].tool_uses().is_empty());
        assert_eq!(history[1].tool_uses().len(), 1);
        assert!(history[2].tool_uses().is_empty());
    }

    #[test]
    fn tool_output_becomes_ok_outcome() {
        let out = ToolOutput { content: "a".into(), metadata: json!({ "n": 1 }) };
        let outcome = ToolOutcome::from(out);
        assert_eq!(
            outcome,
            ToolOutcome::Ok { content: "a".into(), metadata: json!({ "n": 1 }), truncated: false }
        );
        assert!(!outcome.is_error());
    }
}
