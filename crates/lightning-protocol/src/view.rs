//! The timeline view model: a pure fold over events, shared by the live GUI stream and
//! by history loaded from `events.jsonl` (spec 002, section 9.2).
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{Event, EventKind, Outcome, SessionStatus};
use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// `seq` of the event that created the item: stable and unique, for keyed lists.
    pub key: u64,
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    User { text: String },
    Assistant { text: String },
    Tool(ToolItem),
    Notice { level: NoticeLevel, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Retry,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolItem {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub state: ToolState,
    pub duration_ms: Option<u64>,
    pub gate: Option<GateInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolState {
    Running,
    Ok { preview: String, bytes: usize, truncated: bool },
    Err { class: String, message: String },
    /// The log ended while this call was running (crash). Only set by [`TimelineModel::mark_unfinished`].
    Unfinished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateInfo {
    pub allowed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "status", rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Ended(SessionStatus),
    /// Historical log with no `session_ended`.
    Unfinished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineModel {
    pub session_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub workspace: Option<String>,
    pub started_ts: Option<String>,
    pub items: Vec<Item>,
    pub status: RunStatus,
    pub steps: u32,
    pub llm_calls: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub usage: Usage,
    /// Wall-clock duration, known only once the session has ended.
    pub duration_ms: Option<u64>,
    /// Highest `seq` applied; events at or below it are ignored.
    pub last_seq: u64,
}

impl Default for TimelineModel {
    fn default() -> Self {
        TimelineModel {
            session_id: String::new(),
            model: None,
            provider: None,
            workspace: None,
            started_ts: None,
            items: Vec::new(),
            status: RunStatus::Running,
            steps: 0,
            llm_calls: 0,
            tool_calls: 0,
            tool_errors: 0,
            usage: Usage::default(),
            duration_ms: None,
            last_seq: 0,
        }
    }
}

impl TimelineModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Live semantics: a stream without `session_ended` is still running.
    pub fn fold(events: &[Event]) -> TimelineModel {
        let mut model = TimelineModel::new();
        events.iter().for_each(|e| model.apply(e));
        model
    }

    /// History semantics: a complete log without `session_ended` did not finish.
    pub fn fold_history(events: &[Event]) -> TimelineModel {
        let mut model = Self::fold(events);
        model.mark_unfinished();
        model
    }

    /// Applies the log-is-complete reading to a model built from events.
    pub fn mark_unfinished(&mut self) {
        if self.status == RunStatus::Running {
            self.status = RunStatus::Unfinished;
            for item in &mut self.items {
                if let ItemBody::Tool(tool) = &mut item.body
                    && tool.state == ToolState::Running
                {
                    tool.state = ToolState::Unfinished;
                }
            }
        }
    }

    /// Incremental and idempotent: replaying or reordering old events changes nothing.
    pub fn apply(&mut self, event: &Event) {
        if event.seq <= self.last_seq {
            return;
        }
        self.last_seq = event.seq;
        if self.session_id.is_empty() {
            self.session_id = event.session_id.clone();
        }
        let key = event.seq;
        match &event.kind {
            EventKind::SessionStarted { workspace, provider, model, .. } => {
                self.workspace = Some(workspace.clone());
                self.provider = Some(provider.clone());
                self.model = Some(model.clone());
                self.started_ts = Some(event.ts.clone());
            }
            EventKind::UserMessage { text } => self.items.push(Item { key, body: ItemBody::User { text: text.clone() } }),
            EventKind::StepStarted { step } => self.steps = self.steps.max(*step),
            EventKind::StepFinished { .. } | EventKind::LlmRequest { .. } => {}
            EventKind::LlmAttempt { attempt, outcome: Outcome::Error, error, backoff_ms } => {
                let reason = error.as_deref().unwrap_or("unknown error");
                let text = match backoff_ms {
                    Some(ms) => format!(
                        "Model call failed (attempt {attempt}): {reason}. Retrying in {} s.",
                        (*ms as f64 / 1000.0).round()
                    ),
                    None => format!("Model call failed (attempt {attempt}): {reason}. Giving up."),
                };
                self.items.push(Item { key, body: ItemBody::Notice { level: NoticeLevel::Retry, text } });
            }
            EventKind::LlmAttempt { .. } => {}
            EventKind::LlmResponse { usage, .. } => {
                self.llm_calls += 1;
                self.usage.add(usage);
            }
            EventKind::AssistantMessage { text: Some(text), .. } => {
                self.items.push(Item { key, body: ItemBody::Assistant { text: text.clone() } })
            }
            EventKind::AssistantMessage { text: None, .. } => {}
            EventKind::ToolStarted { tool_use_id, name, arguments } => self.items.push(Item {
                key,
                body: ItemBody::Tool(ToolItem {
                    id: tool_use_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                    state: ToolState::Running,
                    duration_ms: None,
                    gate: None,
                }),
            }),
            EventKind::GateDecision { tool_use_id, allowed, reason } => {
                if let Some(tool) = self.tool_mut(tool_use_id) {
                    tool.gate = Some(GateInfo { allowed: *allowed, reason: reason.clone() });
                }
            }
            EventKind::ToolFinished {
                tool_use_id,
                outcome,
                error_class,
                message,
                preview,
                output_bytes,
                truncated,
                duration_ms,
                ..
            } => {
                self.tool_calls += 1;
                if *outcome == Outcome::Error {
                    self.tool_errors += 1;
                }
                if let Some(tool) = self.tool_mut(tool_use_id) {
                    tool.duration_ms = Some(*duration_ms);
                    tool.state = match outcome {
                        Outcome::Ok => ToolState::Ok { preview: preview.clone(), bytes: *output_bytes, truncated: *truncated },
                        Outcome::Error => ToolState::Err {
                            class: error_class.clone().unwrap_or_else(|| "unknown".into()),
                            message: message.clone().unwrap_or_else(|| preview.clone()),
                        },
                    };
                }
            }
            EventKind::LoopWarning { kind, detail } => self.items.push(Item {
                key,
                body: ItemBody::Notice { level: NoticeLevel::Warning, text: format!("{kind}: {detail}") },
            }),
            EventKind::SessionEnded { status, duration_ms, .. } => {
                self.status = RunStatus::Ended(*status);
                self.duration_ms = Some(*duration_ms);
            }
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|item| match &mut item.body {
            ItemBody::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn tools(model: &TimelineModel) -> Vec<&ToolItem> {
        model
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Tool(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn happy_path_builds_the_expected_timeline() {
        let m = TimelineModel::fold(&fixtures::happy_path());
        assert_eq!(m.session_id, fixtures::SESSION_ID);
        assert_eq!(m.status, RunStatus::Ended(SessionStatus::Completed));
        assert_eq!((m.steps, m.llm_calls, m.tool_calls, m.tool_errors), (3, 3, 2, 0));
        assert_eq!(m.model.as_deref(), Some("gemini-3.5-flash-lite"));
        assert_eq!(m.usage.input_tokens, Some(400 + 400 + 520));

        let shape: Vec<&str> = m
            .items
            .iter()
            .map(|i| match &i.body {
                ItemBody::User { .. } => "user",
                ItemBody::Assistant { .. } => "assistant",
                ItemBody::Tool(_) => "tool",
                ItemBody::Notice { .. } => "notice",
            })
            .collect();
        assert_eq!(shape, vec!["user", "tool", "tool", "assistant"]);

        let t = tools(&m);
        assert_eq!(t[0].name, "list_dir");
        assert!(matches!(&t[0].state, ToolState::Ok { preview, .. } if preview.contains("Cargo.toml")));
        assert_eq!(t[1].arguments["file_path"], "Cargo.toml");
        assert_eq!(t[0].gate, Some(GateInfo { allowed: true, reason: "read-only tool".into() }));
        assert_eq!(t[0].duration_ms, Some(2));
    }

    #[test]
    fn tool_failure_shows_class_and_message() {
        let m = TimelineModel::fold(&fixtures::tool_failure());
        assert_eq!((m.tool_calls, m.tool_errors), (1, 1));
        let t = tools(&m);
        assert!(matches!(&t[0].state, ToolState::Err { class, message } if class == "not_found" && message.contains("nothing.txt")));
    }

    #[test]
    fn allowed_write_and_denied_write_share_a_prefix_up_to_the_gate() {
        let allowed = fixtures::approval_allowed();
        let denied = fixtures::approval_denied();
        let gate = |events: &[Event]| events.iter().position(|e| matches!(e.kind, EventKind::GateDecision { .. })).unwrap();
        assert_eq!(gate(&allowed), gate(&denied));
        let g = gate(&allowed);
        assert_eq!(&allowed[..g], &denied[..g]);
        let m = TimelineModel::fold(&allowed);
        assert!(matches!(&tools(&m)[0].state, ToolState::Ok { .. }));
        assert_eq!(m.tool_errors, 0);
    }

    #[test]
    fn denied_write_carries_the_gate_decision() {
        let m = TimelineModel::fold(&fixtures::approval_denied());
        let t = tools(&m);
        assert_eq!(t[0].gate, Some(GateInfo { allowed: false, reason: "declined by user".into() }));
        assert!(matches!(&t[0].state, ToolState::Err { class, .. } if class == "denied"));
    }

    #[test]
    fn retry_becomes_a_notice_and_only_the_response_counts_as_a_call() {
        let m = TimelineModel::fold(&fixtures::retry_then_ok());
        let notices: Vec<&String> = m
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Notice { level: NoticeLevel::Retry, text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Retrying in 16 s"));
        assert_eq!(m.llm_calls, 1);
    }

    #[test]
    fn live_fold_keeps_running_but_history_fold_marks_unfinished() {
        let events = fixtures::unfinished();
        let live = TimelineModel::fold(&events);
        assert_eq!(live.status, RunStatus::Running);
        assert!(matches!(&tools(&live)[0].state, ToolState::Running));

        let history = TimelineModel::fold_history(&events);
        assert_eq!(history.status, RunStatus::Unfinished);
        assert!(matches!(&tools(&history)[0].state, ToolState::Unfinished));

        // A finished session is unaffected by the history reading.
        let done = TimelineModel::fold_history(&fixtures::happy_path());
        assert_eq!(done.status, RunStatus::Ended(SessionStatus::Completed));
    }

    #[test]
    fn incremental_apply_equals_fold_and_is_idempotent() {
        let events = fixtures::happy_path();
        let folded = TimelineModel::fold(&events);

        let mut incremental = TimelineModel::new();
        for e in &events {
            incremental.apply(e);
        }
        assert_eq!(incremental, folded);

        // Replaying everything (a reconnect) changes nothing.
        for e in &events {
            incremental.apply(e);
        }
        assert_eq!(incremental, folded);

        // Stale and duplicate events in the middle of a stream are ignored too.
        let mut noisy = TimelineModel::new();
        for (i, e) in events.iter().enumerate() {
            noisy.apply(e);
            if i > 2 {
                noisy.apply(&events[i - 2]);
            }
        }
        assert_eq!(noisy, folded);
        assert_eq!(folded.last_seq, events.last().unwrap().seq);
    }

    #[test]
    fn item_keys_are_unique() {
        let m = TimelineModel::fold(&fixtures::happy_path());
        let mut keys: Vec<u64> = m.items.iter().map(|i| i.key).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n);
    }

    #[test]
    fn loop_warnings_become_notices() {
        let mut s = fixtures::Stream::new();
        s.push(0, EventKind::LoopWarning { kind: "repeated_failure".into(), detail: "read_file failed 3 times".into() });
        let m = TimelineModel::fold(&s.build());
        assert!(matches!(&m.items[0].body, ItemBody::Notice { level: NoticeLevel::Warning, text } if text.starts_with("repeated_failure")));
    }

    #[test]
    fn model_survives_json_round_trip() {
        let m = TimelineModel::fold(&fixtures::tool_failure());
        let text = serde_json::to_string(&m).unwrap();
        let back: TimelineModel = serde_json::from_str(&text).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn summary_from_fixture_agrees_with_the_view_model() {
        let events = fixtures::happy_path();
        let summary = crate::Summary::from_events(fixtures::SESSION_ID, &events);
        let m = TimelineModel::fold(&events);
        assert_eq!(
            (summary.steps, summary.llm_calls, summary.tool_calls, summary.tool_errors),
            (m.steps, m.llm_calls, m.tool_calls, m.tool_errors)
        );
        assert_eq!(summary.usage, m.usage);
    }
}
