//! The event envelope and typed event payloads (spec 001, section 9.3-9.4).
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage::{FinishReason, Usage};

/// Schema version written into every event.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    /// Wall clock, RFC 3339 with milliseconds, UTC.
    pub ts: String,
    pub session_id: String,
    /// Per-session, strictly increasing from 1. Use this for ordering, not `ts`.
    pub seq: u64,
    #[serde(flatten)]
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Completed,
    MaxStepsReached,
    LlmError,
    Cancelled,
    Stuck,
    ContextLimit,
    TruncatedOutput,
    InternalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUseRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted {
        workspace: String,
        provider: String,
        model: String,
        prompt_version: String,
        harness_version: String,
        /// Config snapshot; secrets are redacted by the recorder.
        config: Value,
    },
    UserMessage {
        text: String,
    },
    StepStarted {
        step: u32,
    },
    StepFinished {
        step: u32,
        duration_ms: u64,
    },
    LlmAttempt {
        attempt: u32,
        outcome: Outcome,
        error: Option<String>,
        backoff_ms: Option<u64>,
    },
    LlmRequest {
        message_count: usize,
        tool_names: Vec<String>,
        approx_input_bytes: usize,
    },
    LlmResponse {
        finish: FinishReason,
        usage: Usage,
        latency_ms: u64,
        tool_use_count: usize,
    },
    AssistantMessage {
        text: Option<String>,
        tool_uses: Vec<ToolUseRef>,
    },
    ToolStarted {
        tool_use_id: String,
        name: String,
        arguments: Value,
    },
    GateDecision {
        tool_use_id: String,
        allowed: bool,
        reason: String,
    },
    ToolFinished {
        tool_use_id: String,
        name: String,
        outcome: Outcome,
        error_class: Option<String>,
        message: Option<String>,
        /// First part of the output (or error text); full text only in the payload log.
        preview: String,
        output_bytes: usize,
        truncated: bool,
        duration_ms: u64,
    },
    LoopWarning {
        kind: String,
        detail: String,
    },
    SessionEnded {
        status: SessionStatus,
        steps: u32,
        tool_calls: u32,
        tool_errors: u32,
        usage: Usage,
        duration_ms: u64,
    },
}

impl EventKind {
    /// Boundaries where buffered sinks should flush to disk.
    pub fn is_flush_point(&self) -> bool {
        matches!(self, EventKind::StepFinished { .. } | EventKind::SessionEnded { .. })
    }
}

/// Cuts `text` to at most `max_bytes` on a char boundary.
pub fn preview(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: EventKind) -> Event {
        Event {
            v: SCHEMA_VERSION,
            ts: "2026-10-10T12:00:00.123Z".into(),
            session_id: "s".into(),
            seq: 1,
            kind,
            span_id: Some("s_1".into()),
            parent_span_id: None,
        }
    }

    #[test]
    fn envelope_has_type_and_data_at_top_level() {
        let e = event(EventKind::StepStarted { step: 3 });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "step_started");
        assert_eq!(v["data"], json!({ "step": 3 }));
        assert_eq!(v["seq"], 1);
        assert_eq!(v["span_id"], "s_1");
        assert!(v.get("parent_span_id").is_none());
    }

    #[test]
    fn every_kind_round_trips() {
        let kinds = vec![
            EventKind::UserMessage { text: "hi".into() },
            EventKind::LlmResponse {
                finish: FinishReason::ToolUse,
                usage: Usage { input_tokens: Some(1), output_tokens: None, thinking_tokens: None },
                latency_ms: 5,
                tool_use_count: 1,
            },
            EventKind::ToolFinished {
                tool_use_id: "c".into(),
                name: "read_file".into(),
                outcome: Outcome::Error,
                error_class: Some("not_found".into()),
                message: Some("m".into()),
                preview: "p".into(),
                output_bytes: 0,
                truncated: false,
                duration_ms: 2,
            },
            EventKind::SessionEnded {
                status: SessionStatus::MaxStepsReached,
                steps: 25,
                tool_calls: 3,
                tool_errors: 1,
                usage: Usage::default(),
                duration_ms: 10,
            },
        ];
        for kind in kinds {
            let e = event(kind);
            let line = serde_json::to_string(&e).unwrap();
            assert!(!line.contains('\n'));
            let back: Event = serde_json::from_str(&line).unwrap();
            assert_eq!(e, back);
        }
    }

    #[test]
    fn preview_cuts_on_char_boundary() {
        assert_eq!(preview("abc", 10), "abc");
        assert_eq!(preview("héllo", 2), "h...");
        assert_eq!(preview("abcdef", 3), "abc...");
    }
}
