//! The bridge to the backend: Tauri commands and events in the app, a scripted mock in a
//! plain browser (so layouts can be checked without Tauri or a model).
//!
//! Everything crosses the boundary as JSON text and goes through `serde_json`, so the
//! types are exactly the ones in `lightning-protocol`.
use lightning_protocol::dto::{
    ActiveInfoDto, ApprovalRequestDto, LoadedSessionDto, SaveSettingsDto, SessionInfoDto, SettingsDto, StartRequest,
    TurnFinishedDto,
};
use lightning_protocol::{Event, Summary};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;
use yew::Callback;

use crate::mock;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    /// Resolves to an "unlisten" function.
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "event"])]
    async fn listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> Result<JsValue, JsValue>;
}

pub fn in_tauri() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__TAURI__")).map(|v| !v.is_undefined()).unwrap_or(false)
}

fn to_js<T: Serialize>(value: &T) -> Result<JsValue, String> {
    let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
    js_sys::JSON::parse(&text).map_err(|e| format!("{e:?}"))
}

fn from_js<T: DeserializeOwned>(value: &JsValue) -> Result<T, String> {
    // `undefined` (a command returning `()`) becomes `null`.
    let text = js_sys::JSON::stringify(value).ok().and_then(|s| s.as_string()).unwrap_or_else(|| "null".to_string());
    serde_json::from_str(&text).map_err(|e| format!("unexpected reply from the app: {e}"))
}

fn error_text(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| js_sys::JSON::stringify(&e).ok().and_then(|s| s.as_string()).unwrap_or_else(|| "unknown error".into()))
}

async fn call<R: DeserializeOwned>(cmd: &str, args: serde_json::Value) -> Result<R, String> {
    let reply = invoke(cmd, to_js(&args)?).await.map_err(error_text)?;
    from_js(&reply)
}

pub async fn get_settings() -> Result<SettingsDto, String> {
    if !in_tauri() {
        return Ok(mock::get_settings());
    }
    call("get_settings", serde_json::json!({})).await
}

pub async fn save_settings(settings: SaveSettingsDto) -> Result<SettingsDto, String> {
    if !in_tauri() {
        return Ok(mock::save_settings(settings));
    }
    call("save_settings", serde_json::json!({ "settings": settings })).await
}

pub async fn set_api_key(key: String) -> Result<SettingsDto, String> {
    if !in_tauri() {
        return Ok(mock::set_api_key());
    }
    call("set_api_key", serde_json::json!({ "key": key })).await
}

pub async fn pick_workspace() -> Result<Option<String>, String> {
    if !in_tauri() {
        return Ok(Some(mock::WORKSPACE.to_string()));
    }
    call("pick_workspace", serde_json::json!({})).await
}

pub async fn start_session(req: StartRequest) -> Result<SessionInfoDto, String> {
    if !in_tauri() {
        return Ok(mock::start_session(req));
    }
    call("start_session", serde_json::json!({ "req": req })).await
}

pub async fn send_message(session_id: &str, text: &str) -> Result<(), String> {
    if !in_tauri() {
        return mock::send_message(session_id, text);
    }
    call("send_message", serde_json::json!({ "sessionId": session_id, "text": text })).await
}

pub async fn cancel_turn(session_id: &str) -> Result<(), String> {
    if !in_tauri() {
        mock::cancel_turn();
        return Ok(());
    }
    call("cancel_turn", serde_json::json!({ "sessionId": session_id })).await
}

pub async fn end_session(session_id: &str) -> Result<Summary, String> {
    if !in_tauri() {
        return Ok(mock::end_session());
    }
    call("end_session", serde_json::json!({ "sessionId": session_id })).await
}

pub async fn respond_approval(request_id: &str, allow: bool) -> Result<(), String> {
    if !in_tauri() {
        mock::respond_approval(allow);
        return Ok(());
    }
    call("respond_approval", serde_json::json!({ "requestId": request_id, "allow": allow })).await
}

pub async fn get_active() -> Result<Option<ActiveInfoDto>, String> {
    if !in_tauri() {
        return Ok(None);
    }
    call("get_active", serde_json::json!({})).await
}

pub async fn list_sessions(workspace: &str) -> Result<Vec<Summary>, String> {
    if !in_tauri() {
        return Ok(mock::list_sessions());
    }
    call("list_sessions", serde_json::json!({ "workspace": workspace })).await
}

pub async fn load_session(workspace: &str, session_id: &str) -> Result<LoadedSessionDto, String> {
    if !in_tauri() {
        return mock::load_session(session_id);
    }
    call("load_session", serde_json::json!({ "workspace": workspace, "sessionId": session_id })).await
}

pub async fn session_events(workspace: &str, session_id: &str, after_seq: u64) -> Result<Vec<Event>, String> {
    if !in_tauri() {
        return Ok(Vec::new());
    }
    call("session_events", serde_json::json!({ "workspace": workspace, "sessionId": session_id, "afterSeq": after_seq })).await
}

/// What the UI wants to hear about.
#[derive(Clone)]
pub struct Handlers {
    pub event: Callback<Event>,
    pub approval: Callback<ApprovalRequestDto>,
    pub approval_closed: Callback<String>,
    pub turn_finished: Callback<TurnFinishedDto>,
}

/// Keeps the JS closures alive; dropping it unsubscribes.
pub struct Subscription {
    _closures: Vec<Closure<dyn FnMut(JsValue)>>,
    unlisten: Vec<js_sys::Function>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        for f in &self.unlisten {
            let _ = f.call0(&JsValue::NULL);
        }
    }
}

fn payload_of<T: DeserializeOwned>(event: &JsValue) -> Option<T> {
    let payload = js_sys::Reflect::get(event, &JsValue::from_str("payload")).ok()?;
    from_js(&payload).ok()
}

pub async fn subscribe(h: Handlers) -> Subscription {
    if !in_tauri() {
        mock::set_handlers(h);
        return Subscription { _closures: Vec::new(), unlisten: Vec::new() };
    }
    let mut closures = Vec::new();
    let mut unlisten = Vec::new();

    macro_rules! hook {
        ($name:expr, $ty:ty, $cb:expr) => {{
            let cb = $cb;
            let closure = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                if let Some(p) = payload_of::<$ty>(&event) {
                    cb.emit(p);
                }
            });
            if let Ok(f) = listen($name, &closure).await {
                if let Ok(f) = f.dyn_into::<js_sys::Function>() {
                    unlisten.push(f);
                }
            }
            closures.push(closure);
        }};
    }

    hook!("lightning://event", Event, h.event.clone());
    hook!("lightning://approval", ApprovalRequestDto, h.approval.clone());
    hook!("lightning://turn-finished", TurnFinishedDto, h.turn_finished.clone());

    // `approval-closed` carries `{ request_id }`.
    let closed = h.approval_closed.clone();
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        if let Some(v) = payload_of::<serde_json::Value>(&event) {
            if let Some(id) = v["request_id"].as_str() {
                closed.emit(id.to_string());
            }
        }
    });
    if let Ok(f) = listen("lightning://approval-closed", &closure).await {
        if let Ok(f) = f.dyn_into::<js_sys::Function>() {
            unlisten.push(f);
        }
    }
    closures.push(closure);

    Subscription { _closures: closures, unlisten }
}
