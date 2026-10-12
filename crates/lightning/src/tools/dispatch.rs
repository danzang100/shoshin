//! The single tool dispatch function (spec 001, sections 4 and 7.1).
//!
//! Pipeline: lookup -> parse args -> gate -> run (timeout, panic-safe) -> map error
//! -> truncate -> events -> `ToolResult`. Nothing else executes tools.
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cancel::CancelToken;
use crate::obs::{EventKind, Outcome, Recorder, preview};
use crate::tools::gate::{Decision, Gate};
use crate::tools::registry::{self, ToolCall, ToolKind, ToolSpec};
use crate::tools::run_shell::run_shell;
use crate::tools::workspace::Workspace;
use crate::tools::{ToolError, ToolOutput};
use crate::turn::{ErrorClass, ToolOutcome, ToolResult, ToolUse};

const PREVIEW_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy)]
pub struct ToolLimits {
    pub timeout: Duration,
    /// Maximum bytes of tool output handed to the model.
    pub output_cap_bytes: usize,
}

impl Default for ToolLimits {
    fn default() -> Self {
        ToolLimits { timeout: Duration::from_secs(30), output_cap_bytes: 32 * 1024 }
    }
}

pub struct Dispatcher {
    workspace: Workspace,
    limits: ToolLimits,
    gate: Gate,
}

impl Dispatcher {
    pub fn new(workspace: Workspace, limits: ToolLimits, gate: Gate) -> Self {
        Dispatcher { workspace, limits, gate }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Tool declarations to send to the model.
    pub fn specs(&self) -> Vec<ToolSpec> {
        registry::tool_specs()
    }

    /// Runs one tool call and always returns a result, never an error: failures are
    /// `ToolOutcome::Error` so the model can react to them.
    pub async fn dispatch(
        &self,
        rec: &Recorder,
        parent_span: Option<&str>,
        tool_use: &ToolUse,
        cancel: &CancelToken,
    ) -> ToolResult {
        let span = rec.new_span();
        rec.emit(
            EventKind::ToolStarted {
                tool_use_id: tool_use.id.clone(),
                name: tool_use.name.clone(),
                arguments: tool_use.arguments.clone(),
            },
            Some(&span),
            parent_span,
        );
        let started = Instant::now();

        let outcome = self.run(rec, &span, tool_use, cancel).await;
        let outcome = truncate(outcome, self.limits.output_cap_bytes);

        let (kind, error_class, message, text, truncated) = match &outcome {
            ToolOutcome::Ok { content, truncated, .. } => (Outcome::Ok, None, None, content.as_str(), *truncated),
            ToolOutcome::Error { class, message } => (Outcome::Error, Some(class_name(*class)), Some(message.clone()), message.as_str(), false),
        };
        rec.emit(
            EventKind::ToolFinished {
                tool_use_id: tool_use.id.clone(),
                name: tool_use.name.clone(),
                outcome: kind,
                error_class,
                message,
                preview: preview(text, PREVIEW_BYTES),
                output_bytes: text.len(),
                truncated,
                duration_ms: started.elapsed().as_millis() as u64,
            },
            Some(&span),
            parent_span,
        );

        ToolResult { tool_use_id: tool_use.id.clone(), name: tool_use.name.clone(), outcome }
    }

    async fn run(&self, rec: &Recorder, span: &str, tool_use: &ToolUse, cancel: &CancelToken) -> ToolOutcome {
        let Some(kind) = ToolKind::from_name(&tool_use.name) else {
            let names: Vec<&'static str> = registry::tool_specs().iter().map(|s| s.name).collect();
            return ToolOutcome::Error {
                class: ErrorClass::UnknownTool,
                message: format!("Unknown tool '{}'. Available tools: {}.", tool_use.name, names.join(", ")),
            };
        };

        // A missing/null arguments object is fine for tools whose parameters all have defaults.
        let arguments = if tool_use.arguments.is_null() { json!({}) } else { tool_use.arguments.clone() };
        let call: ToolCall = match serde_json::from_value(json!({ "name": tool_use.name, "arguments": arguments })) {
            Ok(call) => call,
            Err(err) => return ToolError::from(err).into(),
        };

        // The approval may take minutes (GUI dialog); cancelling denies it and drops the prompt.
        let decision = tokio::select! {
            biased;
            _ = cancel.cancelled() => Decision { allowed: false, reason: "cancelled".to_string() },
            decision = self.gate.check(kind, tool_use) => decision,
        };
        rec.emit(
            EventKind::GateDecision {
                tool_use_id: tool_use.id.clone(),
                allowed: decision.allowed,
                reason: decision.reason.clone(),
            },
            None,
            Some(span),
        );
        if !decision.allowed {
            return ToolOutcome::Error {
                class: ErrorClass::Denied,
                message: format!("The call to {} was not allowed ({}). Do not retry it; ask the user how to proceed.", tool_use.name, decision.reason),
            };
        }

        // The shell is the one async tool: it must be able to kill its process.
        if let ToolCall::RunShell(args) = call {
            return match run_shell(&self.workspace, args, self.limits.timeout, cancel).await {
                Ok(output) => output.into(),
                Err(err) => err.into(),
            };
        }
        let ws = self.workspace.clone();
        guarded(move || registry::execute(&ws, call), self.limits.timeout, cancel).await
    }
}

/// Runs blocking tool code off the async threads, with a deadline, mapping panics
/// and timeouts to outcomes. A timed-out closure keeps running in the background
/// (threads cannot be killed); `run_shell` will additionally kill its child process.
async fn guarded<F>(work: F, timeout: Duration, cancel: &CancelToken) -> ToolOutcome
where
    F: FnOnce() -> Result<ToolOutput, ToolError> + Send + 'static,
{
    let handle = tokio::task::spawn_blocking(work);
    let finished = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return ToolOutcome::Error { class: ErrorClass::Internal, message: "Cancelled by the user.".into() };
        }
        r = tokio::time::timeout(timeout, handle) => r,
    };
    match finished {
        Err(_) => ToolOutcome::Error {
            class: ErrorClass::Timeout,
            message: format!("The tool did not finish within {} seconds and was abandoned.", timeout.as_secs_f32()),
        },
        Ok(Err(join_err)) => ToolOutcome::Error {
            class: ErrorClass::Internal,
            message: if join_err.is_panic() { "The tool crashed unexpectedly.".into() } else { "The tool was cancelled.".into() },
        },
        Ok(Ok(Ok(output))) => output.into(),
        Ok(Ok(Err(err))) => err.into(),
    }
}

fn truncate(outcome: ToolOutcome, cap: usize) -> ToolOutcome {
    match outcome {
        ToolOutcome::Ok { content, metadata, .. } if content.len() > cap => {
            let total = content.len();
            let mut end = cap;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            let content = format!("{}\n[output truncated: showing {end} of {total} bytes]", &content[..end]);
            ToolOutcome::Ok { content, metadata, truncated: true }
        }
        ToolOutcome::Error { class, message } if message.len() > cap => {
            ToolOutcome::Error { class, message: preview(&message, cap) }
        }
        other => other,
    }
}

fn class_name(class: ErrorClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{class:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::{Event, MemorySink, Redactor, SessionId};
    use crate::tools::gate::{FixedPrompter, PermissionMode};
    use crate::tools::test_support::temp_workspace;
    use std::fs;
    use std::sync::Arc;

    fn setup(mode: PermissionMode, approve: bool, limits: ToolLimits) -> (Dispatcher, Recorder, MemorySink, std::path::PathBuf) {
        let (ws, dir) = temp_workspace();
        let sink = MemorySink::default();
        let rec = Recorder::new(SessionId::from("t"), Redactor::default(), vec![Box::new(sink.clone())]);
        let gate = Gate::new(mode, Arc::new(FixedPrompter(approve)));
        (Dispatcher::new(ws, limits, gate), rec, sink, dir)
    }

    fn tu(name: &str, args: serde_json::Value) -> ToolUse {
        ToolUse { id: "call_1".into(), name: name.into(), arguments: args }
    }

    fn error_class(r: &ToolResult) -> ErrorClass {
        match &r.outcome {
            ToolOutcome::Error { class, .. } => *class,
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn happy_path_returns_content_and_emits_matching_events() {
        let (d, rec, sink, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        fs::write(dir.join("a.txt"), "hello").unwrap();
        let r = d.dispatch(&rec, Some("s_parent"), &tu("read_file", json!({ "file_path": "a.txt" })), &CancelToken::default()).await;
        assert!(matches!(&r.outcome, ToolOutcome::Ok { content, .. } if content == "hello"));
        assert_eq!(r.tool_use_id, "call_1");

        let events: Vec<Event> = sink.events();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match &e.kind {
                EventKind::ToolStarted { .. } => "started",
                EventKind::GateDecision { .. } => "gate",
                EventKind::ToolFinished { .. } => "finished",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["started", "gate", "finished"]);
        assert_eq!(events[0].span_id, events[2].span_id);
        assert_eq!(events[0].parent_span_id.as_deref(), Some("s_parent"));
        assert_eq!(events[1].parent_span_id, events[0].span_id);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn tool_failures_become_typed_results() {
        let (d, rec, sink, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        let missing = d.dispatch(&rec, None, &tu("read_file", json!({ "file_path": "nope.txt" })), &CancelToken::default()).await;
        assert_eq!(error_class(&missing), ErrorClass::NotFound);

        let outside = d.dispatch(&rec, None, &tu("read_file", json!({ "file_path": "../x" })), &CancelToken::default()).await;
        assert_eq!(error_class(&outside), ErrorClass::OutsideWorkspace);

        let unknown = d.dispatch(&rec, None, &tu("rm_rf", json!({})), &CancelToken::default()).await;
        assert_eq!(error_class(&unknown), ErrorClass::UnknownTool);
        assert!(matches!(&unknown.outcome, ToolOutcome::Error { message, .. } if message.contains("read_file")));

        let bad = d.dispatch(&rec, None, &tu("read_file", json!({ "file_path": 5 })), &CancelToken::default()).await;
        assert_eq!(error_class(&bad), ErrorClass::InvalidArguments);
        let missing_arg = d.dispatch(&rec, None, &tu("read_file", json!({})), &CancelToken::default()).await;
        assert_eq!(error_class(&missing_arg), ErrorClass::InvalidArguments);

        let finished = sink
            .events()
            .into_iter()
            .filter(|e| matches!(e.kind, EventKind::ToolFinished { outcome: Outcome::Error, .. }))
            .count();
        assert_eq!(finished, 5);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn null_arguments_use_defaults() {
        let (d, rec, _sink, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        let r = d.dispatch(&rec, None, &tu("list_dir", serde_json::Value::Null), &CancelToken::default()).await;
        assert!(!r.outcome.is_error());
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn gate_denial_blocks_the_write_and_tells_the_model() {
        let (d, rec, sink, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        let r = d.dispatch(&rec, None, &tu("write_file", json!({ "file_path": "n.txt", "content": "x" })), &CancelToken::default()).await;
        assert_eq!(error_class(&r), ErrorClass::Denied);
        assert!(!dir.join("n.txt").exists());
        let gate = sink.events().into_iter().find_map(|e| match e.kind {
            EventKind::GateDecision { allowed, reason, .. } => Some((allowed, reason)),
            _ => None,
        });
        assert_eq!(gate, Some((false, "declined by user".to_string())));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn approved_write_happens_and_deny_mode_overrides_prompter() {
        let (d, rec, _s, dir) = setup(PermissionMode::Ask, true, ToolLimits::default());
        let r = d.dispatch(&rec, None, &tu("write_file", json!({ "file_path": "n.txt", "content": "x" })), &CancelToken::default()).await;
        assert!(!r.outcome.is_error());
        assert_eq!(fs::read_to_string(dir.join("n.txt")).unwrap(), "x");

        let (d, rec, _s, dir2) = setup(PermissionMode::Deny, true, ToolLimits::default());
        let r = d.dispatch(&rec, None, &tu("write_file", json!({ "file_path": "n.txt", "content": "x" })), &CancelToken::default()).await;
        assert_eq!(error_class(&r), ErrorClass::Denied);
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(dir2).ok();
    }

    #[tokio::test]
    async fn write_outside_workspace_is_rejected_even_when_allowed() {
        let (d, rec, _s, dir) = setup(PermissionMode::Allow, true, ToolLimits::default());
        let r = d.dispatch(&rec, None, &tu("write_file", json!({ "file_path": "../evil.txt", "content": "x" })), &CancelToken::default()).await;
        assert_eq!(error_class(&r), ErrorClass::OutsideWorkspace);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn large_output_is_truncated_with_marker() {
        let limits = ToolLimits { output_cap_bytes: 10, ..ToolLimits::default() };
        let (d, rec, sink, dir) = setup(PermissionMode::Ask, false, limits);
        fs::write(dir.join("big.txt"), "0123456789abcdefghij").unwrap();
        let r = d.dispatch(&rec, None, &tu("read_file", json!({ "file_path": "big.txt" })), &CancelToken::default()).await;
        match &r.outcome {
            ToolOutcome::Ok { content, truncated, .. } => {
                assert!(*truncated);
                assert!(content.starts_with("0123456789\n[output truncated"));
            }
            other => panic!("{other:?}"),
        }
        assert!(sink.events().iter().any(|e| matches!(e.kind, EventKind::ToolFinished { truncated: true, .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn guarded_maps_timeout_and_panic() {
        let slow = guarded(
            || {
                std::thread::sleep(Duration::from_millis(400));
                Ok(ToolOutput { content: String::new(), metadata: json!({}) })
            },
            Duration::from_millis(30),
            &CancelToken::default(),
        )
        .await;
        assert!(matches!(slow, ToolOutcome::Error { class: ErrorClass::Timeout, .. }));

        let boom = guarded(|| panic!("boom"), Duration::from_secs(5), &CancelToken::default()).await;
        assert!(matches!(boom, ToolOutcome::Error { class: ErrorClass::Internal, .. }));
    }

    /// A prompter that never answers and records when its pending prompt is dropped.
    struct Hanging(Arc<std::sync::atomic::AtomicBool>);

    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl crate::tools::gate::Prompter for Hanging {
        fn confirm<'a>(&'a self, _r: &'a crate::tools::gate::ApprovalRequest) -> crate::tools::gate::BoxFuture<'a, bool> {
            let flag = DropFlag(self.0.clone());
            Box::pin(async move {
                let _flag = flag;
                std::future::pending::<bool>().await
            })
        }
    }

    #[tokio::test]
    async fn cancel_while_approval_is_pending_denies_and_drops_the_prompt() {
        let (ws, dir) = temp_workspace();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = Gate::new(PermissionMode::Ask, Arc::new(Hanging(dropped.clone())));
        let d = Dispatcher::new(ws, ToolLimits::default(), gate);
        let rec = Recorder::new(SessionId::from("t"), Redactor::default(), vec![]);
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        let call = tu("write_file", json!({ "file_path": "n.txt", "content": "x" }));
        let r = tokio::time::timeout(Duration::from_secs(5), d.dispatch(&rec, None, &call, &cancel))
            .await
            .expect("cancel must interrupt the pending approval");
        assert_eq!(error_class(&r), ErrorClass::Denied);
        assert!(matches!(&r.outcome, ToolOutcome::Error { message, .. } if message.contains("cancelled")));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst), "pending prompt must be dropped");
        assert!(!dir.join("n.txt").exists());
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn cancel_during_a_running_tool_returns_promptly() {
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            trigger.cancel();
        });
        let started = Instant::now();
        let out = guarded(
            || {
                std::thread::sleep(Duration::from_millis(600));
                Ok(ToolOutput { content: String::new(), metadata: json!({}) })
            },
            Duration::from_secs(30),
            &cancel,
        )
        .await;
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(matches!(out, ToolOutcome::Error { class: ErrorClass::Internal, ref message } if message.contains("Cancelled")));
    }

    #[tokio::test]
    async fn grep_runs_without_approval_and_reports_matches() {
        let (d, rec, sink, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        fs::write(dir.join("a.txt"), "alpha
needle here
").unwrap();
        let r = d.dispatch(&rec, None, &tu("grep", json!({ "pattern": "needle" })), &CancelToken::default()).await;
        assert!(matches!(&r.outcome, ToolOutcome::Ok { content, .. } if content == "a.txt:2: needle here"));
        assert!(sink.events().iter().any(|e| matches!(&e.kind, EventKind::GateDecision { allowed: true, reason, .. } if reason == "read-only tool")));
        let bad = d.dispatch(&rec, None, &tu("grep", json!({ "pattern": "(" })), &CancelToken::default()).await;
        assert_eq!(error_class(&bad), ErrorClass::InvalidArguments);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn run_shell_is_gated_and_reports_success_failure_and_timeout() {
        // Denied unless the user (or mode) allows it.
        let (d, rec, _s, dir) = setup(PermissionMode::Ask, false, ToolLimits::default());
        let denied = d.dispatch(&rec, None, &tu("run_shell", json!({ "command": "echo hi" })), &CancelToken::default()).await;
        assert_eq!(error_class(&denied), ErrorClass::Denied);
        fs::remove_dir_all(dir).ok();

        let limits = ToolLimits { timeout: Duration::from_secs(1), ..ToolLimits::default() };
        let (d, rec, sink, dir) = setup(PermissionMode::Allow, true, limits);
        let ok = d.dispatch(&rec, None, &tu("run_shell", json!({ "command": "echo hi" })), &CancelToken::default()).await;
        assert!(matches!(&ok.outcome, ToolOutcome::Ok { content, .. } if content == "hi"), "{:?}", ok.outcome);

        let script = if cfg!(windows) { "Write-Output oops; exit 4" } else { "echo oops; exit 4" };
        let failed = d.dispatch(&rec, None, &tu("run_shell", json!({ "command": script })), &CancelToken::default()).await;
        assert_eq!(error_class(&failed), ErrorClass::ExecutionFailed);
        assert!(matches!(&failed.outcome, ToolOutcome::Error { message, .. } if message.contains("code 4") && message.contains("oops")));

        let slow = d.dispatch(&rec, None, &tu("run_shell", json!({ "command": "sleep 20" })), &CancelToken::default()).await;
        assert_eq!(error_class(&slow), ErrorClass::Timeout);

        let finished = sink.events().into_iter().filter(|e| matches!(e.kind, EventKind::ToolFinished { .. })).count();
        assert_eq!(finished, 3);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn the_shell_approval_prompt_shows_the_command() {
        let req = crate::tools::gate::ApprovalRequest { tool: "run_shell".into(), summary: String::new() };
        let _ = req;
        let summary = crate::tools::gate::describe_for_test("run_shell", &json!({ "command": "cargo test" }));
        assert_eq!(summary, "run the command: cargo test");
    }
}
