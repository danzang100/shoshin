//! Session host: everything a GUI needs to drive one session, with no Tauri dependency
//! (spec 002, section 7). The Tauri shell only forwards its calls and its emitter.
//!
//! One active session at a time; at most one running turn per session. Events, approval
//! requests and turn results leave through a [`HostEmitter`].
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightning_protocol::dto::{
    ActiveInfoDto, ApprovalRequestDto, SessionInfoDto, StartRequest, TurnFinishedDto,
};
use tokio::sync::{Notify, oneshot};

use crate::cancel::CancelToken;
use crate::llm::LlmClient;
use crate::obs::{
    Event, EventSink, JsonlSink, PayloadLog, Recorder, Redactor, SessionDir, SessionId, SessionStatus, Summary, read_events,
};
use crate::session::{RetryPolicy, Session, SessionConfig};
use crate::tools::dispatch::{Dispatcher, ToolLimits};
use crate::tools::gate::{ApprovalRequest, BoxFuture, Gate, Prompter};
use crate::tools::workspace::Workspace;

/// How long `end` waits for a running turn to wind down after cancelling it.
const END_WAIT: Duration = Duration::from_secs(5);

/// Where the host sends things the UI must see.
pub trait HostEmitter: Send + Sync {
    /// Every session event, already redacted, in `seq` order.
    fn event(&self, event: &Event);
    fn approval_requested(&self, request: &ApprovalRequestDto);
    /// The approval was answered or abandoned; close its dialog.
    fn approval_closed(&self, request_id: &str);
    fn turn_finished(&self, result: &TurnFinishedDto);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// A turn is running in the current session.
    Busy,
    NoActiveSession,
    TurnInProgress,
    UnknownApproval,
    InvalidWorkspace(String),
    /// Could not set up or finish a session (disk, provider client).
    Setup(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Busy => write!(f, "A request is still running. Stop it or wait for it to finish."),
            HostError::NoActiveSession => write!(f, "There is no active session."),
            HostError::TurnInProgress => write!(f, "The assistant is still working on the previous message."),
            HostError::UnknownApproval => write!(f, "That approval request is no longer pending."),
            HostError::InvalidWorkspace(why) => write!(f, "Invalid workspace: {why}"),
            HostError::Setup(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for HostError {}

/// Static settings for every session the host starts.
#[derive(Clone)]
pub struct HostConfig {
    pub provider: String,
    pub default_model: String,
    /// Used only to redact it from logs and events; never sent anywhere by the host.
    pub api_key: Option<String>,
    pub max_steps: u32,
    pub tool_limits: ToolLimits,
    pub retry: RetryPolicy,
    /// Also write `llm_payloads.jsonl` (the client must capture bodies too).
    pub debug_payloads: bool,
}

impl HostConfig {
    pub fn new(provider: impl Into<String>, default_model: impl Into<String>) -> Self {
        HostConfig {
            provider: provider.into(),
            default_model: default_model.into(),
            api_key: None,
            max_steps: 25,
            tool_limits: ToolLimits::default(),
            retry: RetryPolicy::default(),
            debug_payloads: false,
        }
    }
}

/// Pending approvals, keyed by request id.
#[derive(Default)]
pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalBroker {
    fn register(&self) -> (String, oneshot::Receiver<bool>) {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
        (id, rx)
    }

    fn remove(&self, id: &str) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }

    fn respond(&self, id: &str, allow: bool) -> Result<(), HostError> {
        let tx = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id).ok_or(HostError::UnknownApproval)?;
        // The prompt may have been dropped (cancelled) between lookup and send.
        tx.send(allow).map_err(|_| HostError::UnknownApproval)
    }
}

/// Asks the UI, through the emitter, and waits for `respond_approval`.
struct BrokerPrompter {
    broker: Arc<ApprovalBroker>,
    emitter: Arc<dyn HostEmitter>,
    session_id: String,
}

/// Closes the dialog and forgets the request whether the prompt was answered or dropped.
struct PendingApproval {
    broker: Arc<ApprovalBroker>,
    emitter: Arc<dyn HostEmitter>,
    id: String,
}

impl Drop for PendingApproval {
    fn drop(&mut self) {
        self.broker.remove(&self.id);
        self.emitter.approval_closed(&self.id);
    }
}

impl Prompter for BrokerPrompter {
    fn confirm<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, bool> {
        let (id, rx) = self.broker.register();
        let guard = PendingApproval { broker: self.broker.clone(), emitter: self.emitter.clone(), id: id.clone() };
        self.emitter.approval_requested(&ApprovalRequestDto {
            request_id: id,
            session_id: self.session_id.clone(),
            tool: request.tool.clone(),
            summary: request.summary.clone(),
        });
        Box::pin(async move {
            let _guard = guard;
            rx.await.unwrap_or(false)
        })
    }
}

/// Forwards recorded events to the emitter and tracks the last `seq`.
struct EmitterSink {
    emitter: Arc<dyn HostEmitter>,
    last_seq: Arc<AtomicU64>,
}

impl EventSink for EmitterSink {
    fn emit(&self, event: &Event) {
        self.last_seq.store(event.seq, Ordering::SeqCst);
        self.emitter.event(event);
    }
}

type LlmFactory<L> = Arc<dyn Fn(&StartRequest) -> Result<L, HostError> + Send + Sync>;

struct Active<L: LlmClient> {
    info: SessionInfoDto,
    session: Arc<tokio::sync::Mutex<Session<L>>>,
    cancel: CancelToken,
    running: Arc<AtomicBool>,
    idle: Arc<Notify>,
    last_status: Arc<Mutex<SessionStatus>>,
    last_seq: Arc<AtomicU64>,
    dir: SessionDir,
}

pub struct SessionHost<L: LlmClient + Send + Sync + 'static> {
    emitter: Arc<dyn HostEmitter>,
    config: HostConfig,
    make_llm: LlmFactory<L>,
    rt: tokio::runtime::Handle,
    broker: Arc<ApprovalBroker>,
    secrets: Mutex<Vec<String>>,
    active: Mutex<Option<Active<L>>>,
}

impl<L: LlmClient + Send + Sync + 'static> SessionHost<L> {
    /// `make_llm` builds the provider client for each new session (it may read the API key).
    pub fn new(
        emitter: Arc<dyn HostEmitter>,
        config: HostConfig,
        make_llm: impl Fn(&StartRequest) -> Result<L, HostError> + Send + Sync + 'static,
        rt: tokio::runtime::Handle,
    ) -> Self {
        SessionHost {
            emitter,
            make_llm: Arc::new(make_llm),
            rt,
            broker: Arc::new(ApprovalBroker::default()),
            secrets: Mutex::new(config.api_key.iter().cloned().collect()),
            config,
            active: Mutex::new(None),
        }
    }

    /// Replaces the secrets redacted from logs and events (call when the API key changes).
    pub fn set_secrets(&self, secrets: Vec<String>) {
        *self.secrets.lock().unwrap_or_else(|e| e.into_inner()) = secrets;
    }

    /// Creates a session and its log folder. The `session_started` event reaches the
    /// emitter before this returns, so the UI must accept events for a session it has
    /// not been told about yet.
    pub async fn start(&self, req: StartRequest) -> Result<SessionInfoDto, HostError> {
        let workspace_path = PathBuf::from(&req.workspace);
        if !workspace_path.is_dir() {
            return Err(HostError::InvalidWorkspace(format!("{} is not a folder", req.workspace)));
        }

        let previous = self.lock_active().as_ref().map(|a| (a.info.session_id.clone(), a.running.load(Ordering::SeqCst)));
        if let Some((id, running)) = previous {
            if running {
                return Err(HostError::Busy);
            }
            self.end(&id).await?;
        }

        let llm = (self.make_llm)(&req)?;
        let workspace = Workspace::new(&workspace_path).map_err(|e| HostError::InvalidWorkspace(e.to_string()))?;
        let model = req.model.clone().unwrap_or_else(|| self.config.default_model.clone());
        let mode = req.permission_mode.unwrap_or_default();

        let home = workspace_path.join(".lightning");
        let id = SessionId::new();
        let dir = SessionDir::create(&home, &id).map_err(|e| HostError::Setup(format!("cannot create the log folder: {e}")))?;
        let jsonl = JsonlSink::create(dir.events_path()).map_err(|e| HostError::Setup(format!("cannot open the event log: {e}")))?;

        let last_seq = Arc::new(AtomicU64::new(0));
        let sinks: Vec<Box<dyn EventSink>> =
            vec![Box::new(jsonl), Box::new(EmitterSink { emitter: self.emitter.clone(), last_seq: last_seq.clone() })];
        let redactor = Redactor::new(self.secrets.lock().unwrap_or_else(|e| e.into_inner()).clone());
        let rec = Recorder::new(id.clone(), redactor.clone(), sinks);

        let prompter = Arc::new(BrokerPrompter {
            broker: self.broker.clone(),
            emitter: self.emitter.clone(),
            session_id: id.to_string(),
        });
        let gate = Gate::new(mode, prompter);
        let dispatcher = Dispatcher::new(workspace, self.config.tool_limits, gate);

        let mut scfg = SessionConfig::new(self.config.provider.clone(), model.clone());
        scfg.max_steps = self.config.max_steps;
        scfg.retry = self.config.retry;
        scfg.snapshot = serde_json::json!({
            "workspace": req.workspace,
            "model": model,
            "permission_mode": mode,
            "max_steps": self.config.max_steps,
        });

        let cancel = CancelToken::default();
        let mut session = Session::new(llm, dispatcher, rec, scfg).with_dir(dir.clone()).with_cancel(cancel.clone());
        if self.config.debug_payloads {
            let payloads = PayloadLog::create(dir.payloads_path(), redactor)
                .map_err(|e| HostError::Setup(format!("cannot open the payload log: {e}")))?;
            session = session.with_payloads(payloads);
        }

        let info = SessionInfoDto {
            session_id: id.to_string(),
            workspace: req.workspace,
            provider: self.config.provider.clone(),
            model,
            permission_mode: mode,
        };
        let active = Active {
            info: info.clone(),
            session: Arc::new(tokio::sync::Mutex::new(session)),
            cancel,
            running: Arc::new(AtomicBool::new(false)),
            idle: Arc::new(Notify::new()),
            last_status: Arc::new(Mutex::new(SessionStatus::Completed)),
            last_seq,
            dir,
        };

        let mut slot = self.lock_active();
        if slot.is_some() {
            return Err(HostError::Busy);
        }
        *slot = Some(active);
        Ok(info)
    }

    /// Starts a turn and returns immediately; the result arrives as `turn_finished`.
    pub fn send(&self, session_id: &str, text: String) -> Result<(), HostError> {
        let (session, running, idle, last_status) = {
            let slot = self.lock_active();
            let a = slot.as_ref().filter(|a| a.info.session_id == session_id).ok_or(HostError::NoActiveSession)?;
            (a.session.clone(), a.running.clone(), a.idle.clone(), a.last_status.clone())
        };
        if running.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err(HostError::TurnInProgress);
        }

        let emitter = self.emitter.clone();
        let id = session_id.to_string();
        self.rt.spawn(async move {
            // Run in a child task so a panic is contained and reported instead of lost.
            let turn = tokio::spawn(async move {
                let mut s = session.lock().await;
                let status = s.run_turn(&text).await;
                let error = match status {
                    SessionStatus::LlmError => s.last_error().map(|e| s.recorder().redactor().redact_str(&e.to_string())),
                    _ => None,
                };
                (status, error)
            });
            let (status, error) = match turn.await {
                Ok(done) => done,
                Err(_) => (SessionStatus::InternalError, Some("The assistant stopped unexpectedly.".to_string())),
            };
            *last_status.lock().unwrap_or_else(|e| e.into_inner()) = status;
            running.store(false, Ordering::SeqCst);
            idle.notify_waiters();
            emitter.turn_finished(&TurnFinishedDto { session_id: id, status, error });
        });
        Ok(())
    }

    /// Cancels the running turn. Harmless when idle. A cancelled session cannot take
    /// further turns; start a new one.
    pub fn cancel(&self, session_id: &str) -> Result<(), HostError> {
        let slot = self.lock_active();
        let a = slot.as_ref().filter(|a| a.info.session_id == session_id).ok_or(HostError::NoActiveSession)?;
        a.cancel.cancel();
        Ok(())
    }

    pub fn respond_approval(&self, request_id: &str, allow: bool) -> Result<(), HostError> {
        self.broker.respond(request_id, allow)
    }

    pub fn get_active(&self) -> Option<ActiveInfoDto> {
        self.lock_active().as_ref().map(|a| ActiveInfoDto {
            info: a.info.clone(),
            running: a.running.load(Ordering::SeqCst),
            last_seq: a.last_seq.load(Ordering::SeqCst),
        })
    }

    /// Ends the session: cancels a running turn (waiting a few seconds), writes
    /// `session_ended`, flushes the logs and returns the summary.
    pub async fn end(&self, session_id: &str) -> Result<Summary, HostError> {
        let active = {
            let mut slot = self.lock_active();
            match slot.as_ref() {
                Some(a) if a.info.session_id == session_id => slot.take().ok_or(HostError::NoActiveSession)?,
                _ => return Err(HostError::NoActiveSession),
            }
        };

        if active.running.load(Ordering::SeqCst) {
            active.cancel.cancel();
            let _ = tokio::time::timeout(END_WAIT, async {
                while active.running.load(Ordering::SeqCst) {
                    let woken = active.idle.notified();
                    if !active.running.load(Ordering::SeqCst) {
                        break;
                    }
                    woken.await;
                }
            })
            .await;
        }

        let status = *active.last_status.lock().unwrap_or_else(|e| e.into_inner());
        let mut session = tokio::time::timeout(END_WAIT, active.session.lock())
            .await
            .map_err(|_| HostError::Setup("The session did not stop in time.".into()))?;
        session.end(status);
        drop(session);

        match Summary::read(&active.dir.summary_path()) {
            Ok(summary) => Ok(summary),
            Err(_) => {
                let (events, _) = read_events(&active.dir.events_path()).map_err(|e| HostError::Setup(e.to_string()))?;
                Ok(Summary::from_events(&active.info.session_id, &events))
            }
        }
    }

    /// For app exit: end the active session if there is one, ignoring failures.
    pub async fn shutdown(&self) {
        let id = self.lock_active().as_ref().map(|a| a.info.session_id.clone());
        if let Some(id) = id {
            let _ = self.end(&id).await;
        }
    }

    fn lock_active(&self) -> std::sync::MutexGuard<'_, Option<Active<L>>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning_protocol::dto::PermissionMode;
    use crate::llm::{FakeLlm, LlmError, LlmRequest, LlmResponse};
    use crate::obs::EventKind;
    use serde_json::json;
    use std::fs;
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Event(Event),
        Approval(ApprovalRequestDto),
        ApprovalClosed(String),
        TurnFinished(TurnFinishedDto),
    }

    struct Recording(mpsc::UnboundedSender<Msg>);

    impl HostEmitter for Recording {
        fn event(&self, event: &Event) {
            let _ = self.0.send(Msg::Event(event.clone()));
        }
        fn approval_requested(&self, request: &ApprovalRequestDto) {
            let _ = self.0.send(Msg::Approval(request.clone()));
        }
        fn approval_closed(&self, request_id: &str) {
            let _ = self.0.send(Msg::ApprovalClosed(request_id.to_string()));
        }
        fn turn_finished(&self, result: &TurnFinishedDto) {
            let _ = self.0.send(Msg::TurnFinished(result.clone()));
        }
    }

    type Script = Vec<Result<LlmResponse, LlmError>>;

    fn host_with<L: LlmClient + Send + Sync + 'static>(
        make: impl Fn(&StartRequest) -> Result<L, HostError> + Send + Sync + 'static,
    ) -> (SessionHost<L>, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut config = HostConfig::new("fake", "fake-model");
        config.retry = RetryPolicy::immediate(0);
        (SessionHost::new(Arc::new(Recording(tx)), config, make, tokio::runtime::Handle::current()), rx)
    }

    /// A host whose sessions replay `script` (the same script for every session).
    fn fake_host(script: impl Fn() -> Script + Send + Sync + 'static) -> (SessionHost<FakeLlm>, mpsc::UnboundedReceiver<Msg>) {
        host_with(move |_| Ok(FakeLlm::new(script())))
    }

    fn temp_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightning-host-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn req(dir: &std::path::Path, mode: PermissionMode) -> StartRequest {
        StartRequest { workspace: dir.display().to_string(), model: None, permission_mode: Some(mode) }
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Msg {
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out waiting for the host").expect("emitter closed")
    }

    /// Reads messages until `pick` returns a value; everything skipped is returned too.
    async fn until<T>(rx: &mut mpsc::UnboundedReceiver<Msg>, mut pick: impl FnMut(&Msg) -> Option<T>) -> (T, Vec<Msg>) {
        let mut skipped = Vec::new();
        loop {
            let msg = next(rx).await;
            if let Some(v) = pick(&msg) {
                return (v, skipped);
            }
            skipped.push(msg);
        }
    }

    fn turn_finished(msg: &Msg) -> Option<TurnFinishedDto> {
        match msg {
            Msg::TurnFinished(t) => Some(t.clone()),
            _ => None,
        }
    }

    fn event_kinds(msgs: &[Msg]) -> Vec<&EventKind> {
        msgs.iter()
            .filter_map(|m| match m {
                Msg::Event(e) => Some(&e.kind),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_turn_streams_events_and_finishes() {
        let dir = temp_workspace();
        fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        let (host, mut rx) = fake_host(|| {
            vec![
                FakeLlm::tool_call("c1", "read_file", json!({ "file_path": "Cargo.toml" })),
                FakeLlm::text("It is a package."),
            ]
        });
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        assert_eq!(info.model, "fake-model");
        assert!(host.get_active().is_some_and(|a| !a.running && a.info.session_id == info.session_id));

        host.send(&info.session_id, "summarise".into()).unwrap();
        let (done, seen) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::Completed);
        assert_eq!(done.session_id, info.session_id);
        assert_eq!(done.error, None);

        let kinds = event_kinds(&seen);
        assert!(matches!(kinds.first(), Some(EventKind::SessionStarted { .. })));
        assert!(kinds.iter().any(|k| matches!(k, EventKind::ToolFinished { outcome: crate::obs::Outcome::Ok, .. })));
        assert!(kinds.iter().any(|k| matches!(k, EventKind::AssistantMessage { text: Some(t), .. } if t == "It is a package.")));

        // seq strictly increases and the host reports where it got to.
        let seqs: Vec<u64> = seen.iter().filter_map(|m| if let Msg::Event(e) = m { Some(e.seq) } else { None }).collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
        let active = host.get_active().unwrap();
        assert!(!active.running);
        assert_eq!(active.last_seq, *seqs.last().unwrap());
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn only_one_turn_and_one_busy_session_at_a_time() {
        /// Blocks the model call until released.
        struct Slow(Arc<Notify>);
        impl LlmClient for Slow {
            async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                self.0.notified().await;
                FakeLlm::text("done")
            }
        }
        let release = Arc::new(Notify::new());
        let r = release.clone();
        let (host, mut rx) = host_with(move |_| Ok(Slow(r.clone())));
        let dir = temp_workspace();
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();

        host.send(&info.session_id, "one".into()).unwrap();
        assert_eq!(host.send(&info.session_id, "two".into()), Err(HostError::TurnInProgress));
        assert_eq!(host.start(req(&dir, PermissionMode::Ask)).await.unwrap_err(), HostError::Busy);
        assert!(host.get_active().unwrap().running);

        release.notify_one();
        let (done, _) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::Completed);
        // Idle again: the next turn is accepted.
        host.send(&info.session_id, "three".into()).unwrap();
        release.notify_one();
        let (done, _) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::Completed);
        fs::remove_dir_all(dir).ok();
    }

    async fn run_write_with_answer(allow: bool) -> (PathBuf, TurnFinishedDto, Vec<Msg>) {
        let dir = temp_workspace();
        let (host, mut rx) = fake_host(|| {
            vec![
                FakeLlm::tool_call("w", "write_file", json!({ "file_path": "notes.txt", "content": "hello" })),
                FakeLlm::text("finished"),
            ]
        });
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "write notes".into()).unwrap();

        let (request, mut seen) = until(&mut rx, |m| if let Msg::Approval(a) = m { Some(a.clone()) } else { None }).await;
        assert_eq!(request.tool, "write_file");
        assert_eq!(request.summary, "write notes.txt (5 bytes)");
        assert_eq!(request.session_id, info.session_id);
        host.respond_approval(&request.request_id, allow).unwrap();
        // A second answer to the same request is rejected.
        assert_eq!(host.respond_approval(&request.request_id, allow), Err(HostError::UnknownApproval));

        let (done, rest) = until(&mut rx, turn_finished).await;
        seen.extend(rest);
        assert!(seen.contains(&Msg::ApprovalClosed(request.request_id.clone())));
        (dir, done, seen)
    }

    #[tokio::test]
    async fn approval_allow_writes_the_file() {
        let (dir, done, _) = run_write_with_answer(true).await;
        assert_eq!(done.status, SessionStatus::Completed);
        assert_eq!(fs::read_to_string(dir.join("notes.txt")).unwrap(), "hello");
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn approval_deny_leaves_the_file_absent_and_tells_the_model() {
        let (dir, done, seen) = run_write_with_answer(false).await;
        assert_eq!(done.status, SessionStatus::Completed);
        assert!(!dir.join("notes.txt").exists());
        assert!(event_kinds(&seen).iter().any(|k| matches!(k, EventKind::GateDecision { allowed: false, .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn cancel_while_an_approval_is_pending_closes_the_dialog_and_ends_cancelled() {
        let dir = temp_workspace();
        let (host, mut rx) = fake_host(|| {
            vec![FakeLlm::tool_call("w", "write_file", json!({ "file_path": "n.txt", "content": "x" })), FakeLlm::text("unused")]
        });
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "write".into()).unwrap();
        let (request, _) = until(&mut rx, |m| if let Msg::Approval(a) = m { Some(a.clone()) } else { None }).await;

        host.cancel(&info.session_id).unwrap();
        let (done, seen) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::Cancelled);
        assert!(seen.contains(&Msg::ApprovalClosed(request.request_id.clone())));
        assert!(!dir.join("n.txt").exists());
        assert_eq!(host.respond_approval(&request.request_id, true), Err(HostError::UnknownApproval));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_panicking_provider_is_contained_and_the_host_stays_usable() {
        struct Boom;
        impl LlmClient for Boom {
            async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                panic!("provider exploded");
            }
        }
        let (host, mut rx) = host_with(|_| Ok(Boom));
        let dir = temp_workspace();
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "hi".into()).unwrap();
        let (done, _) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::InternalError);
        assert!(done.error.is_some());
        // The app is still alive: a fresh session can be started.
        assert!(host.end(&info.session_id).await.is_ok());
        assert!(host.start(req(&dir, PermissionMode::Ask)).await.is_ok());
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn llm_errors_are_reported_redacted() {
        let dir = temp_workspace();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut config = HostConfig::new("fake", "m");
        config.retry = RetryPolicy::immediate(0);
        config.api_key = Some("sk-secret-123456".into());
        let host = SessionHost::new(
            Arc::new(Recording(tx)),
            config,
            |_| Ok(FakeLlm::new(vec![Err(LlmError::Http { status: 500, body: "boom sk-secret-123456".into() })])),
            tokio::runtime::Handle::current(),
        );
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "hi".into()).unwrap();
        let (done, _) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::LlmError);
        let error = done.error.unwrap();
        assert!(error.contains("boom"));
        assert!(!error.contains("sk-secret-123456"));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn end_writes_summary_and_clears_the_session() {
        let dir = temp_workspace();
        let (host, mut rx) = fake_host(|| vec![FakeLlm::text("hello")]);
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "hi".into()).unwrap();
        until(&mut rx, turn_finished).await;

        let summary = host.end(&info.session_id).await.unwrap();
        assert_eq!(summary.session_id, info.session_id);
        assert_eq!(summary.status, Some(SessionStatus::Completed));
        assert_eq!(summary.task.as_deref(), Some("hi"));
        let on_disk = dir.join(".lightning").join("sessions").join(&info.session_id).join("summary.json");
        assert!(on_disk.is_file());
        assert!(host.get_active().is_none());
        assert_eq!(host.send(&info.session_id, "again".into()), Err(HostError::NoActiveSession));
        assert_eq!(host.cancel(&info.session_id), Err(HostError::NoActiveSession));
        assert_eq!(host.end(&info.session_id).await.unwrap_err(), HostError::NoActiveSession);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn ending_a_running_turn_cancels_it_and_still_finalises_logs() {
        struct Hang;
        impl LlmClient for Hang {
            async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                FakeLlm::text("never")
            }
        }
        let (host, mut rx) = host_with(|_| Ok(Hang));
        let dir = temp_workspace();
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "hi".into()).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let summary = tokio::time::timeout(Duration::from_secs(8), host.end(&info.session_id)).await.unwrap().unwrap();
        assert_eq!(summary.status, Some(SessionStatus::Cancelled));
        let (done, _) = until(&mut rx, turn_finished).await;
        assert_eq!(done.status, SessionStatus::Cancelled);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn starting_while_idle_ends_the_previous_session_first() {
        let dir = temp_workspace();
        let (host, _rx) = fake_host(|| vec![FakeLlm::text("x")]);
        let first = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        let second = host.start(req(&dir, PermissionMode::Allow)).await.unwrap();
        assert_ne!(first.session_id, second.session_id);
        assert_eq!(second.permission_mode, PermissionMode::Allow);
        assert_eq!(host.get_active().unwrap().info.session_id, second.session_id);
        // The first session's log was finalised.
        let summary = Summary::read(&dir.join(".lightning/sessions").join(&first.session_id).join("summary.json")).unwrap();
        assert!(summary.status.is_some());
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn bad_workspace_and_wrong_session_ids_are_rejected() {
        let (host, _rx) = fake_host(Vec::new);
        let missing = std::env::temp_dir().join(format!("no-such-{}", uuid::Uuid::new_v4()));
        assert!(matches!(host.start(req(&missing, PermissionMode::Ask)).await, Err(HostError::InvalidWorkspace(_))));

        let dir = temp_workspace();
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        assert_eq!(host.send("not-the-session", "x".into()), Err(HostError::NoActiveSession));
        assert_eq!(host.respond_approval("nope", true), Err(HostError::UnknownApproval));
        host.end(&info.session_id).await.unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn the_log_folder_ignores_itself_and_matches_what_the_cli_writes() {
        let dir = temp_workspace();
        let (host, mut rx) = fake_host(|| vec![FakeLlm::text("hi")]);
        let info = host.start(req(&dir, PermissionMode::Ask)).await.unwrap();
        host.send(&info.session_id, "hello".into()).unwrap();
        until(&mut rx, turn_finished).await;
        host.end(&info.session_id).await.unwrap();

        assert_eq!(fs::read_to_string(dir.join(".lightning/.gitignore")).unwrap(), "*\n");
        let events = dir.join(".lightning/sessions").join(&info.session_id).join("events.jsonl");
        let (parsed, report) = read_events(&events).unwrap();
        assert_eq!(report, Default::default());
        assert!(matches!(parsed.last().map(|e| &e.kind), Some(EventKind::SessionEnded { .. })));
        fs::remove_dir_all(dir).ok();
    }
}
