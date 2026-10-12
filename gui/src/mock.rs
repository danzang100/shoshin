//! Browser mock backend: replays the protocol crate's fixture streams with realistic
//! timing so every UI state can be seen without Tauri, a model or an API key.
//!
//! Message text picks the scenario: "missing"/"fail" -> tool failure, "write"/"create" ->
//! an approval (allow or deny from the dialog), "retry" -> a rate-limited retry, anything
//! else -> the happy path. Add `?nokey` to the URL to start without an API key.
use std::cell::{Cell, RefCell};

use lightning_protocol::dto::{
    PermissionMode, SaveSettingsDto, SessionInfoDto, SettingsDto, StartRequest, TurnFinishedDto, ApprovalRequestDto,
    LoadedSessionDto, LogIssuesDto,
};
use lightning_protocol::fixtures;
use lightning_protocol::{Event, EventKind, SessionStatus, Summary};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::api::Handlers;

pub const WORKSPACE: &str = "C:\\Users\\you\\projects\\demo";
const MOCK_SESSION: &str = "20261010T120000Z-mock";

#[derive(Default)]
struct Mock {
    handlers: Option<Handlers>,
    key_present: Option<bool>,
    model: Option<String>,
    mode: Option<PermissionMode>,
    seq: u64,
    started: bool,
    collected: Vec<Event>,
}

thread_local! {
    static MOCK: RefCell<Mock> = RefCell::new(Mock::default());
    static CANCELLED: Cell<bool> = const { Cell::new(false) };
    static ANSWER: Cell<Option<bool>> = const { Cell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Mock) -> R) -> R {
    MOCK.with(|m| f(&mut m.borrow_mut()))
}

fn handlers() -> Option<Handlers> {
    with(|m| m.handlers.clone())
}

pub fn set_handlers(h: Handlers) {
    with(|m| m.handlers = Some(h));
}

fn no_key_requested() -> bool {
    web_sys::window().and_then(|w| w.location().search().ok()).is_some_and(|s| s.contains("nokey"))
}

pub fn get_settings() -> SettingsDto {
    with(|m| {
        let key_present = *m.key_present.get_or_insert(!no_key_requested());
        SettingsDto {
            model: m.model.clone().unwrap_or_else(|| "gemini-3.5-flash-lite".into()),
            default_model: "gemini-3.5-flash-lite".into(),
            known_models: vec!["gemini-3.5-flash-lite".into()],
            permission_mode: m.mode.unwrap_or_default(),
            recent_workspaces: vec![WORKSPACE.into(), "C:\\Users\\you\\projects\\website".into()],
            last_workspace: Some(WORKSPACE.into()),
            key_present,
        }
    })
}

pub fn save_settings(s: SaveSettingsDto) -> SettingsDto {
    with(|m| {
        m.model = Some(s.model);
        m.mode = Some(s.permission_mode);
    });
    get_settings()
}

pub fn set_api_key() -> SettingsDto {
    with(|m| m.key_present = Some(true));
    get_settings()
}

pub fn start_session(req: StartRequest) -> SessionInfoDto {
    CANCELLED.with(|c| c.set(false));
    with(|m| {
        m.seq = 0;
        m.started = false;
        m.collected.clear();
    });
    let info = SessionInfoDto {
        session_id: MOCK_SESSION.into(),
        workspace: req.workspace,
        provider: "gemini".into(),
        model: req.model.unwrap_or_else(|| "gemini-3.5-flash-lite".into()),
        permission_mode: req.permission_mode.unwrap_or_default(),
    };
    // Like the real host, the first event can arrive before the command returns.
    emit_all(vec![fixtures::happy_path().remove(0)], 0);
    info
}

pub fn send_message(_session_id: &str, text: &str) -> Result<(), String> {
    let text = text.to_string();
    CANCELLED.with(|c| c.set(false));
    spawn_local(async move { play(&text).await });
    Ok(())
}

pub fn cancel_turn() {
    CANCELLED.with(|c| c.set(true));
    ANSWER.with(|a| a.set(Some(false)));
}

pub fn respond_approval(allow: bool) {
    ANSWER.with(|a| a.set(Some(allow)));
}

pub fn end_session() -> Summary {
    let events = with(|m| m.collected.clone());
    Summary::from_events(MOCK_SESSION, &events)
}

fn past_sessions() -> Vec<(&'static str, Vec<Event>, LogIssuesDto)> {
    let none = LogIssuesDto::default();
    vec![
        ("20261010T104200Z-aaaa", fixtures::happy_path(), none.clone()),
        ("20261010T101500Z-bbbb", fixtures::tool_failure(), none.clone()),
        ("20261009T163000Z-cccc", fixtures::approval_denied(), none.clone()),
        ("20261009T150000Z-dddd", fixtures::retry_then_ok(), none),
        ("20261008T091000Z-eeee", fixtures::unfinished(), LogIssuesDto { truncated_last_line: true, bad_lines: vec![] }),
    ]
}

pub fn list_sessions() -> Vec<Summary> {
    past_sessions()
        .into_iter()
        .map(|(id, events, _)| Summary::from_events(id, &events.into_iter().map(|e| Event { session_id: id.into(), ..e }).collect::<Vec<_>>()))
        .collect()
}

pub fn load_session(id: &str) -> Result<LoadedSessionDto, String> {
    let (id, events, issues) = past_sessions().into_iter().find(|(s, _, _)| *s == id).ok_or("no such session")?;
    let events: Vec<Event> = events.into_iter().map(|e| Event { session_id: id.into(), ..e }).collect();
    Ok(LoadedSessionDto { summary: Summary::from_events(id, &events), events, log_issues: issues })
}

pub(crate) async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        if let Ok(set_timeout) = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout")) {
            let f: js_sys::Function = set_timeout.unchecked_into();
            let _ = f.call2(&global, &resolve, &JsValue::from_f64(ms as f64));
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Renumbers `events` to continue the mock session and emits them with `gap_ms` between.
fn emit_all(events: Vec<Event>, _gap: u32) {
    for e in events {
        emit_one(e);
    }
}

fn emit_one(mut e: Event) {
    let event = with(|m| {
        m.seq += 1;
        e.seq = m.seq;
        e.session_id = MOCK_SESSION.into();
        // Real timestamps, so the live elapsed timer behaves like the real thing.
        e.ts = String::from(js_sys::Date::new_0().to_iso_string());
        m.collected.push(e.clone());
        e
    });
    if let Some(h) = handlers() {
        h.event.emit(event);
    }
}

fn delay_for(kind: &EventKind) -> u32 {
    match kind {
        EventKind::LlmResponse { .. } => 900,
        EventKind::ToolFinished { .. } => 350,
        EventKind::LlmAttempt { backoff_ms: Some(_), .. } => 1400,
        EventKind::AssistantMessage { .. } => 250,
        _ => 60,
    }
}

fn pick_scenario(text: &str) -> Vec<Event> {
    let t = text.to_lowercase();
    let first = !with(|m| std::mem::replace(&mut m.started, true));
    let mut events = if t.contains("retry") || t.contains("rate") {
        fixtures::retry_then_ok()
    } else if t.contains("write") || t.contains("create") {
        fixtures::approval_allowed()
    } else if t.contains("missing") || t.contains("fail") || t.contains("nothing") {
        fixtures::tool_failure()
    } else {
        fixtures::happy_path()
    };
    // The session start is emitted by `start_session`; the end only by `end_session`.
    events.retain(|e| !matches!(e.kind, EventKind::SessionStarted { .. } | EventKind::SessionEnded { .. }));
    let _ = first;
    // Show what the user actually typed.
    for e in &mut events {
        if let EventKind::UserMessage { text: t } = &mut e.kind {
            *t = text.to_string();
        }
    }
    events
}

async fn play(text: &str) {
    let events = pick_scenario(text);
    let gate_at = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::GateDecision { .. }))
        .filter(|&i| matches!(&events[i].kind, EventKind::GateDecision { reason, .. } if reason == "approved by user" || reason == "declined by user"));

    let mode = with(|m| m.mode.unwrap_or_default());
    let mut denied_tail: Option<Vec<Event>> = None;
    let mut index = 0;
    while index < events.len() {
        if CANCELLED.with(Cell::get) {
            finish(SessionStatus::Cancelled);
            return;
        }
        if Some(index) == gate_at && mode == PermissionMode::Ask {
            // Ask in a dialog, then continue with the matching ending.
            ANSWER.with(|a| a.set(None));
            if let Some(h) = handlers() {
                h.approval.emit(ApprovalRequestDto {
                    request_id: "mock-approval".into(),
                    session_id: MOCK_SESSION.into(),
                    tool: "write_file".into(),
                    summary: "write notes.txt (5 bytes)".into(),
                });
            }
            let allow = loop {
                if let Some(answer) = ANSWER.with(Cell::get) {
                    break answer;
                }
                sleep_ms(80).await;
            };
            if let Some(h) = handlers() {
                h.approval_closed.emit("mock-approval".into());
            }
            if CANCELLED.with(Cell::get) {
                finish(SessionStatus::Cancelled);
                return;
            }
            if !allow {
                let mut denied = fixtures::approval_denied();
                denied.retain(|e| !matches!(e.kind, EventKind::SessionStarted { .. } | EventKind::SessionEnded { .. }));
                let g = denied.iter().position(|e| matches!(e.kind, EventKind::GateDecision { .. })).unwrap_or(0);
                denied_tail = Some(denied.split_off(g));
                break;
            }
        }
        let e = events[index].clone();
        sleep_ms(delay_for(&e.kind)).await;
        emit_one(e);
        index += 1;
    }
    if let Some(tail) = denied_tail {
        for e in tail {
            if CANCELLED.with(Cell::get) {
                finish(SessionStatus::Cancelled);
                return;
            }
            sleep_ms(delay_for(&e.kind)).await;
            emit_one(e);
        }
    }
    finish(SessionStatus::Completed);
}

fn finish(status: SessionStatus) {
    if let Some(h) = handlers() {
        h.turn_finished.emit(TurnFinishedDto { session_id: MOCK_SESSION.into(), status, error: None });
    }
}
