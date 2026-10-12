//! Opt-in end-to-end driver. When the app is started with `LIGHTNING_GUI_SELFTEST=<plan.json>`
//! the UI plays the plan through its own callbacks (so it exercises the real webview, the
//! real IPC and the real host) and prints one `SELFTEST {...}` line per step to the app's
//! console. Without that variable nothing here runs.
use std::cell::RefCell;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use crate::api;
use crate::mock::sleep_ms;
use crate::state::{Action, AppState};

#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    pub workspace: String,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub step_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub message: String,
    /// Answer approvals with this; `None` leaves them open (the step should not need one).
    #[serde(default)]
    pub approval: Option<bool>,
    #[serde(default)]
    pub new_session_first: bool,
    /// Press Stop this many milliseconds after sending.
    #[serde(default)]
    pub stop_after_ms: Option<u32>,
}

/// The newest callbacks, refreshed on every render so the driver never uses stale state.
#[derive(Clone)]
pub struct Latest {
    pub on_send: Callback<String>,
    pub on_answer: Callback<bool>,
    pub on_stop: Callback<()>,
    pub on_new: Callback<()>,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

async fn call(cmd: &str, args: Value) -> Result<JsValue, JsValue> {
    let text = args.to_string();
    let js = js_sys::JSON::parse(&text)?;
    invoke(cmd, js).await
}

pub async fn fetch_plan() -> Option<Plan> {
    if !api::in_tauri() {
        return None;
    }
    let reply = call("selftest_plan", json!({})).await.ok()?;
    let text = js_sys::JSON::stringify(&reply).ok()?.as_string()?;
    serde_json::from_str::<Option<Plan>>(&text).ok().flatten()
}

async fn report(value: Value) {
    let _ = call("selftest_report", json!({ "line": value.to_string() })).await;
}

async fn done(code: i32) {
    let _ = call("selftest_done", json!({ "code": code })).await;
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

pub async fn run(plan: Plan, mirror: Rc<RefCell<AppState>>, latest: Rc<RefCell<Option<Latest>>>, d: UseReducerDispatcher<AppState>) {
    let timeout_ms = plan.step_timeout_secs.unwrap_or(120) as f64 * 1000.0;
    report(json!({ "event": "start", "steps": plan.steps.len() })).await;

    // Wait for settings (and an API key) to load.
    let began = now_ms();
    loop {
        let ready = mirror.borrow().settings.as_ref().is_some_and(|s| s.key_present);
        if ready {
            break;
        }
        if now_ms() - began > 15_000.0 {
            report(json!({ "event": "fail", "reason": "no API key available to the app" })).await;
            done(1).await;
            return;
        }
        sleep_ms(100).await;
    }
    d.dispatch(Action::WorkspaceChosen(plan.workspace.clone()));
    sleep_ms(300).await;

    let mut failures = 0;
    for (index, step) in plan.steps.iter().enumerate() {
        let Some(cb) = latest.borrow().clone() else { continue };
        if step.new_session_first {
            cb.on_new.emit(());
            sleep_ms(600).await;
        }
        let cb = latest.borrow().clone().unwrap_or(cb);
        cb.on_send.emit(step.message.clone());
        sleep_ms(150).await;

        let started = now_ms();
        let mut stopped = false;
        let mut timed_out = false;
        let mut approvals = std::collections::BTreeSet::new();
        loop {
            let (running, approval_id) = {
                let st = mirror.borrow();
                (st.running, st.approval.as_ref().map(|a| a.request_id.clone()))
            };
            if let Some(id) = approval_id {
                approvals.insert(id);
                if let Some(allow) = step.approval {
                    if let Some(latest_cb) = latest.borrow().clone() {
                        latest_cb.on_answer.emit(allow);
                    }
                }
            }
            if !running {
                break;
            }
            let elapsed = now_ms() - started;
            if let Some(ms) = step.stop_after_ms {
                if !stopped && elapsed >= ms as f64 {
                    if let Some(latest_cb) = latest.borrow().clone() {
                        latest_cb.on_stop.emit(());
                    }
                    stopped = true;
                }
            }
            if elapsed > timeout_ms {
                timed_out = true;
                break;
            }
            sleep_ms(100).await;
        }
        sleep_ms(400).await;

        let snapshot = {
            let st = mirror.borrow();
            let live = st.live.as_ref();
            let model = live.map(|l| &l.model);
            let last_assistant = model
                .and_then(|m| {
                    m.items.iter().rev().find_map(|i| match &i.body {
                        lightning_protocol::view::ItemBody::Assistant { text } => Some(text.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            json!({
                "event": "step",
                "index": index,
                "message": step.message,
                "timed_out": timed_out,
                "stopped": stopped,
                "approvals_seen": approvals.len(),
                "running": st.running,
                "last_status": live.and_then(|l| l.last_status).map(|s| format!("{s:?}")),
                "items": model.map(|m| m.items.len()),
                "tool_calls": model.map(|m| m.tool_calls),
                "tool_errors": model.map(|m| m.tool_errors),
                "steps": model.map(|m| m.steps),
                "last_seq": model.map(|m| m.last_seq),
                "tokens_in": model.and_then(|m| m.usage.input_tokens),
                "last_assistant": last_assistant,
                "banner": st.banner.as_ref().map(|b| b.text.clone()),
            })
        };
        if timed_out {
            failures += 1;
        }
        report(snapshot).await;
    }

    // History: the sessions just recorded can be listed and opened.
    sleep_ms(500).await;
    match api::list_sessions(&plan.workspace).await {
        Ok(list) => {
            let first = list.first().map(|s| s.session_id.clone());
            let opened = match &first {
                Some(id) => api::load_session(&plan.workspace, id).await.map(|d| d.events.len()).ok(),
                None => None,
            };
            report(json!({ "event": "history", "sessions": list.len(), "newest": first, "newest_events": opened })).await;
        }
        Err(e) => {
            failures += 1;
            report(json!({ "event": "history_error", "error": e })).await;
        }
    }

    // Finalise the live session's logs, as closing the window would.
    let live_id = mirror.borrow().live.as_ref().and_then(|l| l.info.as_ref().map(|i| i.session_id.clone()));
    if let Some(id) = live_id {
        match api::end_session(&id).await {
            Ok(s) => report(json!({ "event": "ended", "session": id, "status": s.status.map(|x| format!("{x:?}")), "tool_calls": s.tool_calls })).await,
            Err(e) => report(json!({ "event": "end_error", "error": e })).await,
        }
    }
    report(json!({ "event": "finished", "failures": failures })).await;
    done(if failures == 0 { 0 } else { 1 }).await;
}
