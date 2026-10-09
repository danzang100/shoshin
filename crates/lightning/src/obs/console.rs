//! Live, human-readable view of a running session. Assistant text goes to stdout (so it
//! can be piped); everything else (tool lines, retries, warnings) goes to stderr.
use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use super::EventSink;
use super::event::{Event, EventKind, Outcome, preview};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

pub struct ConsoleSink {
    tool_args: Mutex<HashMap<String, Value>>,
}

impl ConsoleSink {
    pub fn new() -> Self {
        ConsoleSink { tool_args: Mutex::new(HashMap::new()) }
    }

    fn lookup_args(&self, id: &str) -> String {
        self.tool_args
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|v| preview(&v.to_string(), 80))
            .unwrap_or_default()
    }
}

impl Default for ConsoleSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for ConsoleSink {
    fn emit(&self, event: &Event) {
        if let EventKind::ToolStarted { tool_use_id, arguments, .. } = &event.kind {
            self.tool_args.lock().unwrap_or_else(|e| e.into_inner()).insert(tool_use_id.clone(), arguments.clone());
        }
        let args = match &event.kind {
            EventKind::ToolFinished { tool_use_id, .. } => self.lookup_args(tool_use_id),
            _ => String::new(),
        };
        match render(event, &args) {
            Some((Stream::Out, line)) => println!("{line}"),
            Some((Stream::Err, line)) => eprintln!("{line}"),
            None => {}
        }
    }
}

/// What to show for one event, if anything. Pure so it can be tested.
pub fn render(event: &Event, tool_args: &str) -> Option<(Stream, String)> {
    match &event.kind {
        EventKind::AssistantMessage { text: Some(text), .. } => Some((Stream::Out, text.clone())),
        EventKind::ToolFinished { name, outcome, error_class, preview: text, duration_ms, .. } => Some((
            Stream::Err,
            match outcome {
                Outcome::Ok => format!("  [tool] {name} {tool_args} -> ok ({duration_ms} ms)"),
                Outcome::Error => format!(
                    "  [tool] {name} {tool_args} -> ERROR {}: {} ({duration_ms} ms)",
                    error_class.as_deref().unwrap_or("?"),
                    preview(&text.replace('\n', " "), 100)
                ),
            },
        )),
        EventKind::GateDecision { allowed: false, reason, .. } => Some((Stream::Err, format!("  [gate] denied ({reason})"))),
        EventKind::LlmAttempt { attempt, outcome: Outcome::Error, error, backoff_ms } => Some((
            Stream::Err,
            format!(
                "  [llm] attempt {attempt} failed: {}{}",
                error.as_deref().unwrap_or("unknown error"),
                backoff_ms.map(|b| format!(", retrying in {b} ms")).unwrap_or_default()
            ),
        )),
        EventKind::LoopWarning { kind, detail } => Some((Stream::Err, format!("  [warning] {kind}: {detail}"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::event::SCHEMA_VERSION;
    use serde_json::json;

    fn ev(kind: EventKind) -> Event {
        Event { v: SCHEMA_VERSION, ts: "t".into(), session_id: "s".into(), seq: 1, kind, span_id: None, parent_span_id: None }
    }

    #[test]
    fn assistant_text_goes_to_stdout_and_tools_to_stderr() {
        let (stream, line) = render(&ev(EventKind::AssistantMessage { text: Some("hello".into()), tool_uses: vec![] }), "").unwrap();
        assert_eq!((stream, line.as_str()), (Stream::Out, "hello"));

        let finished = ev(EventKind::ToolFinished {
            tool_use_id: "c".into(),
            name: "read_file".into(),
            outcome: Outcome::Error,
            error_class: Some("not_found".into()),
            message: None,
            preview: "Not found: x".into(),
            output_bytes: 0,
            truncated: false,
            duration_ms: 3,
        });
        let (stream, line) = render(&finished, r#"{"file_path":"x"}"#).unwrap();
        assert_eq!(stream, Stream::Err);
        assert_eq!(line, r#"  [tool] read_file {"file_path":"x"} -> ERROR not_found: Not found: x (3 ms)"#);
    }

    #[test]
    fn quiet_events_render_nothing() {
        assert!(render(&ev(EventKind::StepStarted { step: 1 }), "").is_none());
        assert!(render(&ev(EventKind::AssistantMessage { text: None, tool_uses: vec![] }), "").is_none());
        assert!(render(&ev(EventKind::GateDecision { tool_use_id: "c".into(), allowed: true, reason: "r".into() }), "").is_none());
        assert!(render(&ev(EventKind::ToolStarted { tool_use_id: "c".into(), name: "n".into(), arguments: json!({}) }), "").is_none());
    }

    #[test]
    fn denials_retries_and_warnings_are_shown() {
        let d = render(&ev(EventKind::GateDecision { tool_use_id: "c".into(), allowed: false, reason: "declined by user".into() }), "").unwrap();
        assert!(d.1.contains("denied"));
        let r = render(&ev(EventKind::LlmAttempt { attempt: 1, outcome: Outcome::Error, error: Some("boom".into()), backoff_ms: Some(500) }), "").unwrap();
        assert!(r.1.contains("retrying in 500 ms"));
        assert!(render(&ev(EventKind::LoopWarning { kind: "k".into(), detail: "d".into() }), "").is_some());
    }
}
