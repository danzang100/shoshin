//! G0 spike UI: runs a self-test of the Tauri 2 APIs on load and lists PASS/FAIL lines.
//! Replaced by the real UI in task G6.
use std::cell::Cell;
use std::rc::Rc;

use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use yew::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    /// Resolves to an "unlisten" function.
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "event"])]
    async fn listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> Result<JsValue, JsValue>;
}

#[derive(Default, PartialEq, Clone)]
struct Log(Vec<String>);

enum LogAction {
    Push(String),
}

impl Reducible for Log {
    type Action = LogAction;

    fn reduce(self: Rc<Self>, action: LogAction) -> Rc<Self> {
        let LogAction::Push(line) = action;
        let mut lines = self.0.clone();
        lines.push(line);
        Rc::new(Log(lines))
    }
}

#[derive(Serialize)]
struct Named<'a> {
    name: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EchoArgs {
    session_id: &'static str,
    after_seq: u64,
}

#[derive(Serialize)]
struct TickArgs {
    count: u32,
}

fn to_js<T: Serialize>(value: &T) -> JsValue {
    serde_wasm_bindgen::to_value(value).unwrap_or(JsValue::NULL)
}

fn tauri_present() -> bool {
    let global = js_sys::global();
    js_sys::Reflect::get(&global, &JsValue::from_str("__TAURI__")).map(|v| !v.is_undefined()).unwrap_or(false)
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let global = js_sys::global();
        if let Ok(set_timeout) = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout")) {
            let f: js_sys::Function = set_timeout.unchecked_into();
            let _ = f.call2(&global, &resolve, &JsValue::from_f64(ms as f64));
        }
    });
    let _ = JsFuture::from(promise).await;
}

fn describe(v: &JsValue) -> String {
    js_sys::JSON::stringify(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| format!("{v:?}"))
}

async fn run_self_test(log: UseReducerHandle<Log>) {
    let say = |ok: bool, name: &str, detail: String| {
        let line = format!("{} {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        // Also print on the Rust side so a headless run can read it.
        let l = line.clone();
        spawn_local(async move {
            let _ = invoke("report", to_js(&serde_json::json!({ "line": l }))).await;
        });
        log.dispatch(LogAction::Push(line));
    };

    // 1. plain command with a string argument and return value
    match invoke("ping", to_js(&Named { name: "Daniel" })).await {
        Ok(v) => say(v.as_string().as_deref() == Some("pong:Daniel"), "command ping", describe(&v)),
        Err(e) => say(false, "command ping", describe(&e)),
    }

    // 2. managed state
    let a = invoke("bump", JsValue::NULL).await.ok().and_then(|v| v.as_f64());
    let b = invoke("bump", JsValue::NULL).await.ok().and_then(|v| v.as_f64());
    say(a == Some(1.0) && b == Some(2.0), "managed State counter", format!("{a:?} then {b:?}"));

    // 3. camelCase argument names map to snake_case parameters, struct result comes back as an object
    let echo = invoke("echo_args", to_js(&EchoArgs { session_id: "s1", after_seq: 41 })).await;
    match echo {
        Ok(v) => {
            let text = describe(&v);
            say(text.contains("\"session_id\":\"s1\"") && text.contains("\"after_seq\":41"), "camelCase args", text);
        }
        Err(e) => say(false, "camelCase args", describe(&e)),
    }

    // 4. Err(String) rejects the promise with that string
    match invoke("always_fails", JsValue::NULL).await {
        Ok(v) => say(false, "error result", format!("unexpectedly ok: {}", describe(&v))),
        Err(e) => say(e.as_string().as_deref() == Some("expected failure from always_fails"), "error result", describe(&e)),
    }

    // 5. events: listen, trigger 3 emits from a background task, count them
    let seen = Rc::new(Cell::new(0u32));
    let seen_in_handler = seen.clone();
    let handler = Closure::<dyn FnMut(JsValue)>::new(move |_event: JsValue| seen_in_handler.set(seen_in_handler.get() + 1));
    match listen("spike://tick", &handler).await {
        Ok(unlisten) => {
            let _ = invoke("emit_ticks", to_js(&TickArgs { count: 3 })).await;
            sleep_ms(800).await;
            say(seen.get() == 3, "event listen", format!("received {} of 3", seen.get()));

            // 6. unlisten stops delivery
            if let Ok(f) = unlisten.dyn_into::<js_sys::Function>() {
                let _ = f.call0(&JsValue::NULL);
                let _ = invoke("emit_ticks", to_js(&TickArgs { count: 2 })).await;
                sleep_ms(500).await;
                say(seen.get() == 3, "event unlisten", format!("count stayed {}", seen.get()));
            } else {
                say(false, "event unlisten", "listen did not return a function".into());
            }
        }
        Err(e) => say(false, "event listen", describe(&e)),
    }
    drop(handler);

    // 7. the core crate links and runs inside the shell
    match invoke("core_session_id", JsValue::NULL).await {
        Ok(v) => say(v.as_string().is_some_and(|s| s.len() == 21 && s.contains('T')), "lightning crate", describe(&v)),
        Err(e) => say(false, "lightning crate", describe(&e)),
    }

    say(true, "done", "self-test finished".into());
}

#[function_component(App)]
pub fn app() -> Html {
    let log = use_reducer(Log::default);
    let folder = use_state(|| String::from("(not picked yet)"));
    let present = tauri_present();

    {
        let log = log.clone();
        use_effect_with((), move |_| {
            if present {
                spawn_local(run_self_test(log));
            } else {
                log.dispatch(LogAction::Push("MOCK MODE: window.__TAURI__ is absent (plain browser)".into()));
            }
            || {}
        });
    }

    let pick = {
        let folder = folder.clone();
        Callback::from(move |_| {
            let folder = folder.clone();
            spawn_local(async move {
                match invoke("pick_folder", JsValue::NULL).await {
                    Ok(v) => folder.set(v.as_string().unwrap_or_else(|| "(cancelled)".into())),
                    Err(e) => folder.set(format!("error: {}", describe(&e))),
                }
            });
        })
    };

    html! {
        <main class="container">
            <h1>{"Lightning G0 spike"}</h1>
            <p>{ if present { "Running inside Tauri." } else { "Plain browser: Tauri APIs unavailable." } }</p>
            <ul id="results">
                { for log.0.iter().map(|l| html! { <li class={ if l.starts_with("FAIL") { "fail" } else { "pass" } }>{ l }</li> }) }
            </ul>
            <div class="row">
                <button onclick={pick} disabled={!present}>{"Pick folder (manual dialog test)"}</button>
            </div>
            <p id="folder">{ &*folder }</p>
        </main>
    }
}
