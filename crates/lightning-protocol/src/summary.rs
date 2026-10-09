//! `summary.json`, always derived by folding events so it cannot disagree with them
//! (spec 001, sections 9.2 and 13).
use serde::{Deserialize, Serialize};

use crate::event::{Event, EventKind, Outcome, SessionStatus};
use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub session_id: String,
    /// `None` when the log has no `session_ended` event (crash or still running).
    pub status: Option<SessionStatus>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub workspace: Option<String>,
    /// First user message.
    pub task: Option<String>,
    pub steps: u32,
    pub llm_calls: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub usage: Usage,
    pub warnings: u32,
    pub event_count: usize,
}

impl Summary {
    /// Folds an event stream. Counts come from the individual events; only `status`
    /// and `duration_ms` come from `session_ended`.
    pub fn from_events(session_id: &str, events: &[Event]) -> Summary {
        let mut s = Summary {
            session_id: session_id.to_string(),
            status: None,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            provider: None,
            model: None,
            workspace: None,
            task: None,
            steps: 0,
            llm_calls: 0,
            tool_calls: 0,
            tool_errors: 0,
            usage: Usage::default(),
            warnings: 0,
            event_count: events.len(),
        };
        for e in events {
            match &e.kind {
                EventKind::SessionStarted { workspace, provider, model, .. } => {
                    s.started_at = Some(e.ts.clone());
                    s.workspace = Some(workspace.clone());
                    s.provider = Some(provider.clone());
                    s.model = Some(model.clone());
                }
                EventKind::UserMessage { text } if s.task.is_none() => s.task = Some(text.clone()),
                EventKind::StepStarted { .. } => s.steps += 1,
                EventKind::LlmResponse { usage, .. } => {
                    s.llm_calls += 1;
                    s.usage.add(usage);
                }
                EventKind::ToolFinished { outcome, .. } => {
                    s.tool_calls += 1;
                    if *outcome == Outcome::Error {
                        s.tool_errors += 1;
                    }
                }
                EventKind::LoopWarning { .. } => s.warnings += 1,
                EventKind::SessionEnded { status, duration_ms, .. } => {
                    s.status = Some(*status);
                    s.ended_at = Some(e.ts.clone());
                    s.duration_ms = Some(*duration_ms);
                }
                _ => {}
            }
        }
        s
    }

}

#[cfg(feature = "fs")]
impl Summary {
    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::from)?;
        std::fs::write(path, text)
    }

    pub fn read(path: &std::path::Path) -> std::io::Result<Summary> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SCHEMA_VERSION;
    use crate::usage::FinishReason;
    use serde_json::json;

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            v: SCHEMA_VERSION,
            ts: format!("2026-10-10T12:00:0{seq}.000Z"),
            session_id: "s".into(),
            seq,
            kind,
            span_id: None,
            parent_span_id: None,
        }
    }

    fn tool_finished(outcome: Outcome) -> EventKind {
        EventKind::ToolFinished {
            tool_use_id: "c".into(),
            name: "read_file".into(),
            outcome,
            error_class: None,
            message: None,
            preview: String::new(),
            output_bytes: 0,
            truncated: false,
            duration_ms: 1,
        }
    }

    fn usage(i: u64, o: u64) -> Usage {
        Usage { input_tokens: Some(i), output_tokens: Some(o), thinking_tokens: None }
    }

    #[test]
    fn folds_counts_and_totals() {
        let events = vec![
            ev(1, EventKind::SessionStarted {
                workspace: "/w".into(),
                provider: "gemini".into(),
                model: "m".into(),
                prompt_version: "1".into(),
                harness_version: "0".into(),
                config: json!({}),
            }),
            ev(2, EventKind::UserMessage { text: "do it".into() }),
            ev(3, EventKind::UserMessage { text: "later".into() }),
            ev(4, EventKind::StepStarted { step: 1 }),
            ev(5, EventKind::LlmResponse {
                finish: FinishReason::ToolUse,
                usage: usage(10, 2),
                latency_ms: 5,
                tool_use_count: 1,
            }),
            ev(6, tool_finished(Outcome::Error)),
            ev(7, EventKind::StepStarted { step: 2 }),
            ev(8, EventKind::LlmResponse {
                finish: FinishReason::Stop,
                usage: usage(20, 4),
                latency_ms: 5,
                tool_use_count: 0,
            }),
            ev(9, tool_finished(Outcome::Ok)),
            ev(9, EventKind::LoopWarning { kind: "k".into(), detail: "d".into() }),
            ev(9, EventKind::SessionEnded {
                status: SessionStatus::Completed,
                steps: 2,
                tool_calls: 2,
                tool_errors: 1,
                usage: usage(30, 6),
                duration_ms: 99,
            }),
        ];
        let s = Summary::from_events("s", &events);
        assert_eq!(s.status, Some(SessionStatus::Completed));
        assert_eq!(s.task.as_deref(), Some("do it"));
        assert_eq!((s.steps, s.llm_calls, s.tool_calls, s.tool_errors, s.warnings), (2, 2, 2, 1, 1));
        assert_eq!(s.usage, Usage { input_tokens: Some(30), output_tokens: Some(6), thinking_tokens: None });
        assert_eq!(s.duration_ms, Some(99));
        assert_eq!(s.model.as_deref(), Some("m"));
    }

    #[test]
    fn missing_session_ended_means_no_status() {
        let events = vec![ev(1, EventKind::StepStarted { step: 1 })];
        let s = Summary::from_events("s", &events);
        assert_eq!(s.status, None);
        assert_eq!(s.duration_ms, None);
        assert_eq!(s.steps, 1);
    }

    #[cfg(feature = "fs")]
    #[test]
    fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join(format!("lightning-test-summary-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Summary::from_events("s", &[ev(1, EventKind::StepStarted { step: 1 })]);
        let path = dir.join("summary.json");
        s.write(&path).unwrap();
        assert_eq!(Summary::read(&path).unwrap(), s);
        std::fs::remove_dir_all(dir).ok();
    }
}
