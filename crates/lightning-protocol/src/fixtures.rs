//! Canned event streams. They back the view-model tests and the GUI's browser mock mode,
//! so what the tests assert is exactly what a designer sees.
use serde_json::{Value, json};

use crate::event::{Event, EventKind, Outcome, SCHEMA_VERSION, SessionStatus, ToolUseRef};
use crate::usage::{FinishReason, Usage};

pub const SESSION_ID: &str = "20261010T120000Z-demo";

/// Builds a consistent stream: increasing `seq` and timestamps.
pub struct Stream {
    events: Vec<Event>,
    ms: u64,
}

impl Stream {
    pub fn new() -> Self {
        Stream { events: Vec::new(), ms: 0 }
    }

    /// Advances the clock by `after_ms` and appends an event.
    pub fn push(&mut self, after_ms: u64, kind: EventKind) -> &mut Self {
        self.ms += after_ms;
        let seq = self.events.len() as u64 + 1;
        self.events.push(Event {
            v: SCHEMA_VERSION,
            ts: format!("2026-10-10T12:{:02}:{:02}.{:03}Z", self.ms / 60_000, (self.ms / 1000) % 60, self.ms % 1000),
            session_id: SESSION_ID.to_string(),
            seq,
            kind,
            span_id: None,
            parent_span_id: None,
        });
        self
    }

    pub fn build(&self) -> Vec<Event> {
        self.events.clone()
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

fn usage(input: u64, output: u64) -> Usage {
    Usage { input_tokens: Some(input), output_tokens: Some(output), thinking_tokens: Some(0) }
}

fn started(s: &mut Stream) {
    s.push(
        0,
        EventKind::SessionStarted {
            workspace: "C:\\work\\demo".into(),
            provider: "gemini".into(),
            model: "gemini-3.5-flash-lite".into(),
            prompt_version: "1".into(),
            harness_version: "0.1.0".into(),
            config: Value::Null,
        },
    );
}

fn llm_tool_use(s: &mut Stream, after: u64, id: &str, name: &str) {
    s.push(
        after,
        EventKind::LlmResponse { finish: FinishReason::ToolUse, usage: usage(400, 20), latency_ms: after, tool_use_count: 1 },
    );
    s.push(1, EventKind::AssistantMessage { text: None, tool_uses: vec![ToolUseRef { id: id.into(), name: name.into() }] });
}

fn tool_ok(s: &mut Stream, id: &str, name: &str, args: Value, preview: &str) {
    s.push(1, EventKind::ToolStarted { tool_use_id: id.into(), name: name.into(), arguments: args });
    s.push(0, EventKind::GateDecision { tool_use_id: id.into(), allowed: true, reason: "read-only tool".into() });
    s.push(
        2,
        EventKind::ToolFinished {
            tool_use_id: id.into(),
            name: name.into(),
            outcome: Outcome::Ok,
            error_class: None,
            message: None,
            preview: preview.into(),
            output_bytes: preview.len(),
            truncated: false,
            duration_ms: 2,
        },
    );
}

fn tool_err(s: &mut Stream, id: &str, name: &str, args: Value, class: &str, message: &str) {
    s.push(1, EventKind::ToolStarted { tool_use_id: id.into(), name: name.into(), arguments: args });
    s.push(0, EventKind::GateDecision { tool_use_id: id.into(), allowed: true, reason: "read-only tool".into() });
    s.push(
        1,
        EventKind::ToolFinished {
            tool_use_id: id.into(),
            name: name.into(),
            outcome: Outcome::Error,
            error_class: Some(class.into()),
            message: Some(message.into()),
            preview: message.into(),
            output_bytes: message.len(),
            truncated: false,
            duration_ms: 1,
        },
    );
}

fn answer(s: &mut Stream, step: u32, text: &str) {
    s.push(700, EventKind::LlmResponse { finish: FinishReason::Stop, usage: usage(520, 30), latency_ms: 700, tool_use_count: 0 });
    s.push(1, EventKind::AssistantMessage { text: Some(text.into()), tool_uses: vec![] });
    s.push(1, EventKind::StepFinished { step, duration_ms: 702 });
}

fn ended(s: &mut Stream, status: SessionStatus, steps: u32, calls: u32, errors: u32) {
    s.push(
        1,
        EventKind::SessionEnded { status, steps, tool_calls: calls, tool_errors: errors, usage: usage(0, 0), duration_ms: 0 },
    );
}

/// list_dir, read_file, then an answer.
pub fn happy_path() -> Vec<Event> {
    let mut s = Stream::new();
    started(&mut s);
    s.push(1, EventKind::UserMessage { text: "List the files and summarise Cargo.toml.".into() });
    s.push(1, EventKind::StepStarted { step: 1 });
    llm_tool_use(&mut s, 800, "c1", "list_dir");
    tool_ok(&mut s, "c1", "list_dir", json!({}), "Cargo.toml\nsrc/\n");
    s.push(1, EventKind::StepFinished { step: 1, duration_ms: 806 });
    s.push(1, EventKind::StepStarted { step: 2 });
    llm_tool_use(&mut s, 600, "c2", "read_file");
    tool_ok(&mut s, "c2", "read_file", json!({ "file_path": "Cargo.toml" }), "[package]\nname = \"demo\"\n");
    s.push(1, EventKind::StepFinished { step: 2, duration_ms: 606 });
    s.push(1, EventKind::StepStarted { step: 3 });
    answer(&mut s, 3, "It is a Rust package named demo.");
    ended(&mut s, SessionStatus::Completed, 3, 2, 0);
    s.build()
}

/// A missing file, then recovery.
pub fn tool_failure() -> Vec<Event> {
    let mut s = Stream::new();
    started(&mut s);
    s.push(1, EventKind::UserMessage { text: "Read nothing.txt".into() });
    s.push(1, EventKind::StepStarted { step: 1 });
    llm_tool_use(&mut s, 700, "c1", "read_file");
    tool_err(
        &mut s,
        "c1",
        "read_file",
        json!({ "file_path": "nothing.txt" }),
        "not_found",
        "Not found: nothing.txt. Use list_dir to check the path.",
    );
    s.push(1, EventKind::StepFinished { step: 1, duration_ms: 705 });
    s.push(1, EventKind::StepStarted { step: 2 });
    answer(&mut s, 2, "That file does not exist.");
    ended(&mut s, SessionStatus::Completed, 2, 1, 1);
    s.build()
}

/// A write that the user denies.
pub fn approval_denied() -> Vec<Event> {
    let mut s = Stream::new();
    started(&mut s);
    s.push(1, EventKind::UserMessage { text: "Create notes.txt".into() });
    s.push(1, EventKind::StepStarted { step: 1 });
    llm_tool_use(&mut s, 700, "c1", "write_file");
    s.push(
        1,
        EventKind::ToolStarted {
            tool_use_id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({ "file_path": "notes.txt", "content": "hello" }),
        },
    );
    s.push(4000, EventKind::GateDecision { tool_use_id: "c1".into(), allowed: false, reason: "declined by user".into() });
    s.push(
        1,
        EventKind::ToolFinished {
            tool_use_id: "c1".into(),
            name: "write_file".into(),
            outcome: Outcome::Error,
            error_class: Some("denied".into()),
            message: Some("The call to write_file was not allowed (declined by user).".into()),
            preview: "The call to write_file was not allowed (declined by user).".into(),
            output_bytes: 58,
            truncated: false,
            duration_ms: 4001,
        },
    );
    s.push(1, EventKind::StepFinished { step: 1, duration_ms: 4705 });
    s.push(1, EventKind::StepStarted { step: 2 });
    answer(&mut s, 2, "Understood, I did not create the file.");
    ended(&mut s, SessionStatus::Completed, 2, 1, 1);
    s.build()
}

/// A rate-limited call that succeeds on retry.
pub fn retry_then_ok() -> Vec<Event> {
    let mut s = Stream::new();
    started(&mut s);
    s.push(1, EventKind::UserMessage { text: "hello".into() });
    s.push(1, EventKind::StepStarted { step: 1 });
    s.push(
        300,
        EventKind::LlmAttempt {
            attempt: 1,
            outcome: Outcome::Error,
            error: Some("rate limited, retry after 16s".into()),
            backoff_ms: Some(16000),
        },
    );
    s.push(16000, EventKind::LlmAttempt { attempt: 2, outcome: Outcome::Ok, error: None, backoff_ms: None });
    answer(&mut s, 1, "Hi there!");
    ended(&mut s, SessionStatus::Completed, 1, 0, 0);
    s.build()
}

/// A crash: the log stops after the tool started, with no `session_ended`.
pub fn unfinished() -> Vec<Event> {
    let mut s = Stream::new();
    started(&mut s);
    s.push(1, EventKind::UserMessage { text: "Read big.log".into() });
    s.push(1, EventKind::StepStarted { step: 1 });
    llm_tool_use(&mut s, 700, "c1", "read_file");
    s.push(
        1,
        EventKind::ToolStarted { tool_use_id: "c1".into(), name: "read_file".into(), arguments: json!({ "file_path": "big.log" }) },
    );
    s.build()
}
