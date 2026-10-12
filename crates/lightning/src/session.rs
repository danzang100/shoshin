//! The orchestrator loop (spec 001, section 8): one session, many user turns, every
//! fact recorded through the [`Recorder`].
use std::time::{Duration, Instant};

use serde_json::Value;

pub use crate::cancel::CancelToken;
use crate::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, Usage};
use crate::obs::{
    EventKind, Outcome, PayloadLog, Recorder, SessionDir, SessionStatus, Summary, ToolUseRef, read_events,
};
use crate::prompt::{PROMPT_VERSION, system_prompt};
use crate::tools::dispatch::Dispatcher;
use crate::turn::{ErrorClass, ToolOutcome, ToolResult, ToolUse, Turn};

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy { max_retries: 3, base_delay: Duration::from_millis(500), max_delay: Duration::from_secs(30) }
    }
}

impl RetryPolicy {
    /// No waiting between attempts (tests).
    pub fn immediate(max_retries: u32) -> Self {
        RetryPolicy { max_retries, base_delay: Duration::ZERO, max_delay: Duration::ZERO }
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub provider: String,
    pub model: String,
    /// LLM calls allowed per user turn.
    pub max_steps: u32,
    /// Approximate serialized history size at which the session stops (no compaction yet).
    pub max_history_bytes: usize,
    pub max_output_tokens: Option<u32>,
    pub retry: RetryPolicy,
    /// Consecutive identical failures that add a hint to the tool result.
    pub repeat_warn_after: u32,
    /// Consecutive identical failures that end the session as stuck.
    pub repeat_stop_after: u32,
    /// Secret-free config snapshot recorded in `session_started`.
    pub snapshot: Value,
}

impl SessionConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        SessionConfig {
            provider: provider.into(),
            model: model.into(),
            max_steps: 25,
            max_history_bytes: 400_000,
            max_output_tokens: None,
            retry: RetryPolicy::default(),
            repeat_warn_after: 3,
            repeat_stop_after: 5,
            snapshot: Value::Null,
        }
    }
}

enum CallError {
    Cancelled,
    Llm(LlmError),
}

enum Flow {
    Continue,
    End(SessionStatus),
}

pub struct Session<L: LlmClient> {
    llm: L,
    dispatcher: Dispatcher,
    rec: Recorder,
    config: SessionConfig,
    system: String,
    history: Vec<Turn>,
    cancel: CancelToken,
    dir: Option<SessionDir>,
    payloads: Option<PayloadLog>,
    session_span: String,
    started: Instant,
    ended: bool,
    step: u32,
    tool_calls: u32,
    tool_errors: u32,
    usage: Usage,
    last_failure: Option<(String, u32)>,
    last_error: Option<LlmError>,
}

impl<L: LlmClient> Session<L> {
    /// Creates the session and records `session_started`.
    pub fn new(llm: L, dispatcher: Dispatcher, rec: Recorder, config: SessionConfig) -> Self {
        let system = system_prompt(&dispatcher.workspace().display_root());
        let session_span = rec.new_span();
        rec.emit(
            EventKind::SessionStarted {
                workspace: dispatcher.workspace().display_root(),
                provider: config.provider.clone(),
                model: config.model.clone(),
                prompt_version: PROMPT_VERSION.to_string(),
                harness_version: env!("CARGO_PKG_VERSION").to_string(),
                config: config.snapshot.clone(),
            },
            Some(&session_span),
            None,
        );
        Session {
            llm,
            dispatcher,
            rec,
            config,
            system,
            history: Vec::new(),
            cancel: CancelToken::default(),
            dir: None,
            payloads: None,
            session_span,
            started: Instant::now(),
            ended: false,
            step: 0,
            tool_calls: 0,
            tool_errors: 0,
            usage: Usage::default(),
            last_failure: None,
            last_error: None,
        }
    }

    /// Where to write `summary.json` when the session ends.
    pub fn with_dir(mut self, dir: SessionDir) -> Self {
        self.dir = Some(dir);
        self
    }

    /// Where to write full LLM bodies (only filled when the client captures them).
    pub fn with_payloads(mut self, payloads: PayloadLog) -> Self {
        self.payloads = Some(payloads);
        self
    }

    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    pub fn history(&self) -> &[Turn] {
        &self.history
    }

    pub fn llm(&self) -> &L {
        &self.llm
    }

    /// The LLM error that ended the last turn as `LlmError`, for showing to the user.
    pub fn last_error(&self) -> Option<&LlmError> {
        self.last_error.as_ref()
    }

    pub fn recorder(&self) -> &Recorder {
        &self.rec
    }

    /// One user message, run until the model answers without tools or a limit trips.
    /// The session stays usable after `Completed`; call [`Session::end`] when done.
    pub async fn run_turn(&mut self, user_text: &str) -> SessionStatus {
        self.history.push(Turn::User { text: user_text.to_string() });
        self.rec.emit(EventKind::UserMessage { text: user_text.to_string() }, None, Some(&self.session_span.clone()));

        for turn_step in 1..=self.config.max_steps {
            if self.cancel.is_cancelled() {
                return SessionStatus::Cancelled;
            }
            if self.history_bytes() > self.config.max_history_bytes {
                self.warn("context_limit", "conversation is too large to continue");
                return SessionStatus::ContextLimit;
            }

            self.step += 1;
            let step = self.step;
            let span = self.rec.new_span();
            let session_span = self.session_span.clone();
            self.rec.emit(EventKind::StepStarted { step }, Some(&span), Some(&session_span));
            if turn_step == self.config.max_steps && self.config.max_steps > 1 {
                self.warn("last_step", "this is the final step allowed for this request");
            }
            let t0 = Instant::now();

            let flow = self.step_body(&span).await;

            self.rec.emit(
                EventKind::StepFinished { step, duration_ms: t0.elapsed().as_millis() as u64 },
                Some(&span),
                Some(&session_span),
            );
            if let Flow::End(status) = flow {
                return status;
            }
        }
        SessionStatus::MaxStepsReached
    }

    /// Records `session_ended`, flushes sinks and writes `summary.json`.
    pub fn end(&mut self, status: SessionStatus) {
        if self.ended {
            return;
        }
        self.ended = true;
        let session_span = self.session_span.clone();
        self.rec.emit(
            EventKind::SessionEnded {
                status,
                steps: self.step,
                tool_calls: self.tool_calls,
                tool_errors: self.tool_errors,
                usage: self.usage,
                duration_ms: self.started.elapsed().as_millis() as u64,
            },
            Some(&session_span),
            None,
        );
        self.rec.close();
        if let Some(dir) = &self.dir {
            match read_events(&dir.events_path()) {
                Ok((events, _)) => {
                    let summary = Summary::from_events(self.rec.session_id().as_str(), &events);
                    if let Err(err) = summary.write(&dir.summary_path()) {
                        eprintln!("warning: could not write summary.json: {err}");
                    }
                }
                Err(err) => eprintln!("warning: could not read events back for summary.json: {err}"),
            }
        }
    }

    /// Convenience for one-shot use: one turn, then end.
    pub async fn run(&mut self, task: &str) -> SessionStatus {
        let status = self.run_turn(task).await;
        self.end(status);
        status
    }

    fn warn(&self, kind: &str, detail: &str) {
        self.rec.emit(
            EventKind::LoopWarning { kind: kind.to_string(), detail: detail.to_string() },
            None,
            Some(&self.session_span),
        );
    }

    fn history_bytes(&self) -> usize {
        serde_json::to_vec(&self.history).map(|v| v.len()).unwrap_or(0)
    }

    async fn step_body(&mut self, step_span: &str) -> Flow {
        let response = match self.call_llm(step_span).await {
            Ok(r) => r,
            Err(CallError::Cancelled) => return Flow::End(SessionStatus::Cancelled),
            Err(CallError::Llm(err)) => {
                self.last_error = Some(err);
                return Flow::End(SessionStatus::LlmError);
            }
        };

        let turn = response.turn.clone();
        let tool_uses: Vec<ToolUse> = turn.tool_uses().to_vec();
        if let Turn::Assistant { text, .. } = &turn {
            self.rec.emit(
                EventKind::AssistantMessage {
                    text: text.clone(),
                    tool_uses: tool_uses.iter().map(|t| ToolUseRef { id: t.id.clone(), name: t.name.clone() }).collect(),
                },
                None,
                Some(step_span),
            );
        }
        self.history.push(turn);

        if tool_uses.is_empty() {
            return Flow::End(match response.finish {
                crate::llm::FinishReason::MaxTokens => SessionStatus::TruncatedOutput,
                _ => SessionStatus::Completed,
            });
        }

        // Every tool_use gets a result, in order, even after a cancel or a failure.
        let mut results = Vec::with_capacity(tool_uses.len());
        let mut stuck = false;
        for tool_use in &tool_uses {
            if self.cancel.is_cancelled() {
                results.push(ToolResult {
                    tool_use_id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    outcome: ToolOutcome::Error { class: ErrorClass::Internal, message: "Cancelled by the user.".into() },
                });
                continue;
            }
            let mut result = self.dispatcher.dispatch(&self.rec, Some(step_span), tool_use, &self.cancel).await;
            self.tool_calls += 1;
            if result.outcome.is_error() {
                self.tool_errors += 1;
            }
            stuck |= self.apply_repeat_guard(tool_use, &mut result);
            results.push(result);
        }
        self.history.push(Turn::Tool { results });

        if self.cancel.is_cancelled() {
            Flow::End(SessionStatus::Cancelled)
        } else if stuck {
            Flow::End(SessionStatus::Stuck)
        } else {
            Flow::Continue
        }
    }

    /// Tracks consecutive identical failures. Returns true when the session is stuck.
    fn apply_repeat_guard(&mut self, tool_use: &ToolUse, result: &mut ToolResult) -> bool {
        if !result.outcome.is_error() {
            self.last_failure = None;
            return false;
        }
        let key = format!("{}:{}", tool_use.name, tool_use.arguments);
        let count = match &self.last_failure {
            Some((k, n)) if *k == key => n + 1,
            _ => 1,
        };
        self.last_failure = Some((key, count));

        if count >= self.config.repeat_warn_after
            && let ToolOutcome::Error { message, .. } = &mut result.outcome
        {
            message.push_str(&format!(
                "\n[note: this exact call has failed {count} times in a row. Try something different.]"
            ));
            self.warn("repeated_failure", &format!("{} failed {count} times with identical arguments", tool_use.name));
        }
        count >= self.config.repeat_stop_after
    }

    async fn call_llm(&mut self, step_span: &str) -> Result<LlmResponse, CallError> {
        let llm_span = self.rec.new_span();
        let request = LlmRequest {
            system: self.system.clone(),
            turns: self.history.clone(),
            tools: self.dispatcher.specs(),
            max_output_tokens: self.config.max_output_tokens,
        };
        self.rec.emit(
            EventKind::LlmRequest {
                message_count: request.turns.len(),
                tool_names: request.tools.iter().map(|t| t.name.to_string()).collect(),
                approx_input_bytes: self.history_bytes(),
            },
            Some(&llm_span),
            Some(step_span),
        );

        let max_attempts = 1 + self.config.retry.max_retries;
        let mut bad_response_retried = false;
        for attempt in 1..=max_attempts {
            let outcome = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(CallError::Cancelled),
                r = self.llm.complete(request.clone()) => r,
            };
            match outcome {
                Ok(response) => {
                    self.rec.emit(
                        EventKind::LlmAttempt { attempt, outcome: Outcome::Ok, error: None, backoff_ms: None },
                        Some(&llm_span),
                        Some(step_span),
                    );
                    let seq = self.rec.emit(
                        EventKind::LlmResponse {
                            finish: response.finish.clone(),
                            usage: response.usage,
                            latency_ms: response.latency_ms,
                            tool_use_count: response.turn.tool_uses().len(),
                        },
                        Some(&llm_span),
                        Some(step_span),
                    );
                    self.usage.add(&response.usage);
                    if let (Some(log), Some(exchange)) = (&self.payloads, &response.exchange) {
                        log.record(seq, "request", exchange.request.clone());
                        log.record(seq, "response", exchange.response.clone());
                    }
                    return Ok(response);
                }
                Err(err) => {
                    let is_bad = matches!(err, LlmError::BadResponse(_));
                    let can_retry = attempt < max_attempts && (err.is_retryable() || (is_bad && !bad_response_retried));
                    if is_bad {
                        bad_response_retried = true;
                    }
                    let backoff = can_retry.then(|| self.backoff(attempt, &err));
                    self.rec.emit(
                        EventKind::LlmAttempt {
                            attempt,
                            outcome: Outcome::Error,
                            error: Some(err.to_string()),
                            backoff_ms: backoff.map(|d| d.as_millis() as u64),
                        },
                        Some(&llm_span),
                        Some(step_span),
                    );
                    let Some(delay) = backoff else {
                        return Err(CallError::Llm(err));
                    };
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return Err(CallError::Cancelled),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
        Err(CallError::Llm(LlmError::BadResponse("no attempts were made".into())))
    }

    fn backoff(&self, attempt: u32, err: &LlmError) -> Duration {
        let policy = self.config.retry;
        if let LlmError::RateLimited { retry_after: Some(wait) } = err {
            return (*wait).min(Duration::from_secs(60));
        }
        let exp = policy.base_delay.saturating_mul(1u32 << (attempt - 1).min(10)).min(policy.max_delay);
        // Up to 25% jitter so parallel sessions do not retry in lockstep.
        let jitter_max = exp.as_millis() as u64 / 4;
        let jitter = if jitter_max == 0 { 0 } else { (uuid::Uuid::new_v4().as_u128() as u64) % jitter_max };
        exp + Duration::from_millis(jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::llm::{FakeLlm, FinishReason};
    use crate::obs::{Event, JsonlSink, MemorySink, Redactor, SessionId};
    use crate::tools::dispatch::ToolLimits;
    use crate::tools::gate::{FixedPrompter, Gate, PermissionMode};
    use crate::tools::test_support::temp_workspace;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    struct Harness {
        session: Session<FakeLlm>,
        sink: MemorySink,
        dir: PathBuf,
    }

    fn harness_with(script: Vec<Result<LlmResponse, LlmError>>, mode: PermissionMode, approve: bool, tweak: impl FnOnce(&mut SessionConfig)) -> Harness {
        let (ws, dir) = temp_workspace();
        let sink = MemorySink::default();
        let rec = Recorder::new(SessionId::from("test"), Redactor::default(), vec![Box::new(sink.clone())]);
        let gate = Gate::new(mode, Arc::new(FixedPrompter(approve)));
        let dispatcher = Dispatcher::new(ws, ToolLimits::default(), gate);
        let mut config = SessionConfig::new("fake", "fake-model");
        config.retry = RetryPolicy::immediate(3);
        tweak(&mut config);
        Harness { session: Session::new(FakeLlm::new(script), dispatcher, rec, config), sink, dir }
    }

    fn harness(script: Vec<Result<LlmResponse, LlmError>>) -> Harness {
        harness_with(script, PermissionMode::Ask, false, |_| {})
    }

    fn call(id: &str, name: &str, args: Value) -> Result<LlmResponse, LlmError> {
        FakeLlm::tool_call(id, name, args)
    }

    fn results_of(turn: &Turn) -> &[ToolResult] {
        match turn {
            Turn::Tool { results } => results,
            other => panic!("expected tool turn, got {other:?}"),
        }
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match &e.kind {
                EventKind::SessionStarted { .. } => "session_started",
                EventKind::UserMessage { .. } => "user_message",
                EventKind::StepStarted { .. } => "step_started",
                EventKind::StepFinished { .. } => "step_finished",
                EventKind::LlmAttempt { .. } => "llm_attempt",
                EventKind::LlmRequest { .. } => "llm_request",
                EventKind::LlmResponse { .. } => "llm_response",
                EventKind::AssistantMessage { .. } => "assistant_message",
                EventKind::ToolStarted { .. } => "tool_started",
                EventKind::GateDecision { .. } => "gate_decision",
                EventKind::ToolFinished { .. } => "tool_finished",
                EventKind::LoopWarning { .. } => "loop_warning",
                EventKind::SessionEnded { .. } => "session_ended",
            })
            .collect()
    }

    fn count(events: &[Event], kind: &str) -> usize {
        kinds(events).iter().filter(|k| **k == kind).count()
    }

    #[tokio::test]
    async fn happy_path_lists_reads_and_answers() {
        let mut h = harness(vec![
            call("c1", "list_dir", json!({})),
            call("c2", "read_file", json!({ "file_path": "Cargo.toml" })),
            FakeLlm::text("It is a package."),
        ]);
        fs::write(h.dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let status = h.session.run("Summarise Cargo.toml").await;
        assert_eq!(status, SessionStatus::Completed);

        let hist = h.session.history();
        assert_eq!(hist.len(), 6); // user, asst, tool, asst, tool, asst
        assert!(matches!(&results_of(&hist[2])[0].outcome, ToolOutcome::Ok { content, .. } if content.contains("Cargo.toml")));
        assert!(matches!(&results_of(&hist[4])[0].outcome, ToolOutcome::Ok { content, .. } if content.contains("[package]")));
        assert!(matches!(&hist[5], Turn::Assistant { text: Some(t), .. } if t == "It is a package."));

        // The model saw the whole conversation so far on its last call.
        let reqs = h.session.llm().requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[2].turns.len(), 5);
        assert!(reqs[0].system.contains("workspace"));
        assert_eq!(reqs[0].tools.len(), 5);

        let events = h.sink.events();
        assert_eq!(count(&events, "step_started"), 3);
        assert_eq!(count(&events, "step_finished"), 3);
        assert_eq!(count(&events, "tool_started"), count(&events, "tool_finished"));
        assert_eq!(kinds(&events).last(), Some(&"session_ended"));
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn missing_file_error_reaches_the_model_and_session_recovers() {
        let mut h = harness(vec![
            call("c1", "read_file", json!({ "file_path": "nope.txt" })),
            FakeLlm::text("That file does not exist."),
        ]);
        let status = h.session.run("read nope.txt").await;
        assert_eq!(status, SessionStatus::Completed);
        let r = &results_of(&h.session.history()[2])[0];
        assert!(matches!(&r.outcome, ToolOutcome::Error { class: ErrorClass::NotFound, message } if message.contains("nope.txt")));
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn sandbox_bad_args_and_unknown_tool_are_recoverable() {
        let mut h = harness(vec![
            call("c1", "read_file", json!({ "file_path": "../outside" })),
            call("c2", "read_file", json!({ "file_path": 42 })),
            call("c3", "delete_everything", json!({})),
            FakeLlm::text("done"),
        ]);
        assert_eq!(h.session.run("go").await, SessionStatus::Completed);
        let classes: Vec<ErrorClass> = [2, 4, 6]
            .iter()
            .map(|i| match &results_of(&h.session.history()[*i])[0].outcome {
                ToolOutcome::Error { class, .. } => *class,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(classes, vec![ErrorClass::OutsideWorkspace, ErrorClass::InvalidArguments, ErrorClass::UnknownTool]);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn multi_turn_memory_is_kept_across_user_messages() {
        let mut h = harness(vec![FakeLlm::text("Teal noted."), FakeLlm::text("Teal.")]);
        assert_eq!(h.session.run_turn("My colour is teal").await, SessionStatus::Completed);
        assert_eq!(h.session.run_turn("What is my colour?").await, SessionStatus::Completed);
        let reqs = h.session.llm().requests();
        assert_eq!(reqs[1].turns.len(), 3);
        assert!(matches!(&reqs[1].turns[0], Turn::User { text } if text == "My colour is teal"));
        h.session.end(SessionStatus::Completed);
        let events = h.sink.events();
        assert_eq!(count(&events, "user_message"), 2);
        assert_eq!(count(&events, "session_ended"), 1);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn runaway_loop_stops_at_max_steps_with_every_call_answered() {
        let script = (0..10).map(|i| call(&format!("c{i}"), "list_dir", json!({}))).collect();
        let mut h = harness_with(script, PermissionMode::Ask, false, |c| c.max_steps = 3);
        assert_eq!(h.session.run("loop").await, SessionStatus::MaxStepsReached);
        assert_eq!(h.session.llm().requests().len(), 3);
        let hist = h.session.history();
        assert_eq!(hist.len(), 7); // user + 3 * (assistant, tool)
        assert!(matches!(hist.last(), Some(Turn::Tool { .. })));
        let events = h.sink.events();
        assert_eq!(count(&events, "step_started"), count(&events, "step_finished"));
        assert!(count(&events, "loop_warning") >= 1);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn repeated_identical_failures_get_a_hint_then_end_stuck() {
        let script = (0..10).map(|i| call(&format!("c{i}"), "read_file", json!({ "file_path": "missing.txt" }))).collect();
        let mut h = harness(script);
        assert_eq!(h.session.run("try").await, SessionStatus::Stuck);
        assert_eq!(h.session.llm().requests().len(), 5);
        let msg = |i: usize| match &results_of(&h.session.history()[i])[0].outcome {
            ToolOutcome::Error { message, .. } => message.clone(),
            other => panic!("{other:?}"),
        };
        assert!(!msg(2).contains("[note:"));
        assert!(!msg(4).contains("[note:"));
        assert!(msg(6).contains("failed 3 times in a row"));
        assert!(h.sink.events().iter().any(|e| matches!(&e.kind, EventKind::LoopWarning { kind, .. } if kind == "repeated_failure")));
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn a_success_resets_the_repeat_counter() {
        let bad = |id: &str| call(id, "read_file", json!({ "file_path": "missing.txt" }));
        let script = vec![bad("1"), bad("2"), call("3", "list_dir", json!({})), bad("4"), bad("5"), FakeLlm::text("ok")];
        let mut h = harness(script);
        assert_eq!(h.session.run("try").await, SessionStatus::Completed);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn transient_llm_error_is_retried_and_logged() {
        let script = vec![Err(LlmError::Transport("reset".into())), Err(LlmError::Http { status: 503, body: "x".into() }), FakeLlm::text("hi")];
        let mut h = harness(script);
        assert_eq!(h.session.run("hello").await, SessionStatus::Completed);
        let attempts: Vec<(u32, Outcome)> = h
            .sink
            .events()
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::LlmAttempt { attempt, outcome, .. } => Some((attempt, outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![(1, Outcome::Error), (2, Outcome::Error), (3, Outcome::Ok)]);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn exhausted_retries_end_as_llm_error() {
        let script = (0..10).map(|_| Err(LlmError::Transport("down".into()))).collect();
        let mut h = harness(script);
        assert_eq!(h.session.run("hello").await, SessionStatus::LlmError);
        assert_eq!(h.session.llm().requests().len(), 4); // 1 + 3 retries
        let events = h.sink.events();
        assert_eq!(count(&events, "step_started"), count(&events, "step_finished"));
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn auth_errors_are_not_retried() {
        let mut h = harness(vec![Err(LlmError::Auth), FakeLlm::text("never")]);
        assert_eq!(h.session.run("hello").await, SessionStatus::LlmError);
        assert_eq!(h.session.llm().requests().len(), 1);
        assert_eq!(h.session.last_error(), Some(&LlmError::Auth));
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn bad_response_gets_exactly_one_retry() {
        let script = vec![Err(LlmError::BadResponse("empty".into())), Err(LlmError::BadResponse("empty".into())), FakeLlm::text("late")];
        let mut h = harness(script);
        assert_eq!(h.session.run("hello").await, SessionStatus::LlmError);
        assert_eq!(h.session.llm().requests().len(), 2);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn cancel_before_start_ends_cancelled_without_calling_the_model() {
        let mut h = harness(vec![FakeLlm::text("unused")]);
        h.session.cancel_token().cancel();
        assert_eq!(h.session.run("hello").await, SessionStatus::Cancelled);
        assert!(h.session.llm().requests().is_empty());
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn cancel_while_waiting_on_the_model_interrupts_the_call() {
        struct Hang;
        impl LlmClient for Hang {
            async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(LlmError::Auth)
            }
        }
        let (ws, dir) = crate::tools::test_support::temp_workspace();
        let rec = Recorder::new(SessionId::from("t"), Redactor::default(), vec![]);
        let gate = Gate::new(PermissionMode::Ask, Arc::new(FixedPrompter(false)));
        let mut session = Session::new(Hang, Dispatcher::new(ws, ToolLimits::default(), gate), rec, SessionConfig::new("p", "m"));
        let token = session.cancel_token();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });
        let status = tokio::time::timeout(Duration::from_secs(5), session.run("hi")).await.expect("cancel should interrupt");
        assert_eq!(status, SessionStatus::Cancelled);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn token_cap_without_tool_calls_is_truncated_output() {
        let mut resp = FakeLlm::text("cut off mid-sen").unwrap();
        resp.finish = FinishReason::MaxTokens;
        let mut h = harness(vec![Ok(resp)]);
        assert_eq!(h.session.run("write an essay").await, SessionStatus::TruncatedOutput);
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn oversized_history_ends_as_context_limit() {
        let mut h = harness_with(vec![FakeLlm::text("unused")], PermissionMode::Ask, false, |c| c.max_history_bytes = 20);
        assert_eq!(h.session.run("this message is longer than twenty bytes").await, SessionStatus::ContextLimit);
        assert!(h.session.llm().requests().is_empty());
        fs::remove_dir_all(&h.dir).ok();
    }

    #[tokio::test]
    async fn write_file_is_approved_or_denied_through_the_gate() {
        let write = || call("w", "write_file", json!({ "file_path": "notes.txt", "content": "summary" }));

        let mut ok = harness_with(vec![write(), FakeLlm::text("written")], PermissionMode::Ask, true, |_| {});
        assert_eq!(ok.session.run("write notes").await, SessionStatus::Completed);
        assert_eq!(fs::read_to_string(ok.dir.join("notes.txt")).unwrap(), "summary");

        let mut no = harness_with(vec![write(), FakeLlm::text("understood")], PermissionMode::Ask, false, |_| {});
        assert_eq!(no.session.run("write notes").await, SessionStatus::Completed);
        assert!(!no.dir.join("notes.txt").exists());
        assert!(matches!(
            &results_of(&no.session.history()[2])[0].outcome,
            ToolOutcome::Error { class: ErrorClass::Denied, .. }
        ));

        let mut out = harness_with(
            vec![call("w", "write_file", json!({ "file_path": "../x.txt", "content": "x" })), FakeLlm::text("sorry")],
            PermissionMode::Allow,
            true,
            |_| {},
        );
        assert_eq!(out.session.run("write outside").await, SessionStatus::Completed);
        assert!(matches!(
            &results_of(&out.session.history()[2])[0].outcome,
            ToolOutcome::Error { class: ErrorClass::OutsideWorkspace, .. }
        ));
        for h in [ok, no, out] {
            fs::remove_dir_all(&h.dir).ok();
        }
    }

    #[tokio::test]
    async fn logs_on_disk_are_well_formed_and_summary_matches() {
        let (ws, dir) = temp_workspace();
        let home = std::env::temp_dir().join(format!("lightning-home-{}", uuid::Uuid::new_v4()));
        let id = SessionId::from("20261010T000000Z-test");
        let sdir = SessionDir::create(&home, &id).unwrap();
        let sink = JsonlSink::create(sdir.events_path()).unwrap();
        let rec = Recorder::new(id, Redactor::default(), vec![Box::new(sink)]);
        let gate = Gate::new(PermissionMode::Ask, Arc::new(FixedPrompter(false)));
        let llm = FakeLlm::new(vec![
            call("c1", "read_file", json!({ "file_path": "missing.txt" })),
            call("c2", "list_dir", json!({})),
            FakeLlm::text("done"),
        ]);
        let mut session = Session::new(llm, Dispatcher::new(ws, ToolLimits::default(), gate), rec, SessionConfig::new("fake", "m")).with_dir(sdir.clone());
        assert_eq!(session.run("task").await, SessionStatus::Completed);

        let (events, report) = read_events(&sdir.events_path()).unwrap();
        assert_eq!(report, Default::default());
        assert!(events.windows(2).all(|w| w[1].seq == w[0].seq + 1), "seq must increase by one");
        assert_eq!(count(&events, "step_started"), count(&events, "step_finished"));
        assert_eq!(count(&events, "tool_started"), count(&events, "tool_finished"));

        let summary = Summary::read(&sdir.summary_path()).unwrap();
        assert_eq!(summary.status, Some(SessionStatus::Completed));
        assert_eq!((summary.steps, summary.llm_calls, summary.tool_calls, summary.tool_errors), (3, 3, 2, 1));
        assert_eq!(summary.task.as_deref(), Some("task"));
        assert_eq!(summary, Summary::from_events("20261010T000000Z-test", &events));

        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(home).ok();
    }

    /// The GUI host spawns turns onto the runtime, so the session and its futures must be `Send`.
    #[test]
    fn session_and_run_turn_future_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<Session<crate::llm::GeminiClient>>();
        assert_send_static::<Session<FakeLlm>>();
        let _check = |session: &mut Session<crate::llm::GeminiClient>| {
            let fut = session.run_turn("x");
            assert_send(&fut);
        };
    }
}
