//! Presentational components. They take data and callbacks; `app.rs` owns the wiring.
use lightning_protocol::dto::{ApprovalRequestDto, PermissionMode, SettingsDto};
use lightning_protocol::view::{Item, ItemBody, NoticeLevel, RunStatus, ToolItem, ToolState, TimelineModel};
use lightning_protocol::{SessionStatus, Summary};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::fmt;
use crate::md_view::Markdown;
use crate::state::{Banner, BannerKind, HistoryView};

// ---------------------------------------------------------------- timeline

#[derive(Properties, PartialEq)]
pub struct TimelineProps {
    pub model: TimelineModel,
    /// A turn is in flight: show the "working" row.
    pub working: bool,
}

#[function_component(Timeline)]
pub fn timeline(props: &TimelineProps) -> Html {
    let scroller = use_node_ref();
    {
        let scroller = scroller.clone();
        use_effect_with((props.model.items.len(), props.working), move |_| {
            if let Some(el) = scroller.cast::<web_sys::Element>() {
                el.set_scroll_top(el.scroll_height());
            }
            || {}
        });
    }
    html! {
        <div class="timeline" ref={scroller} role="log" aria-live="polite">
            { for props.model.items.iter().map(|item| html! { <ItemView key={item.key} item={item.clone()} /> }) }
            if props.working {
                <div class="working" role="status"><span class="dots"><i></i><i></i><i></i></span>{"Working"}</div>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct ItemProps {
    item: Item,
}

#[function_component(ItemView)]
fn item_view(props: &ItemProps) -> Html {
    match &props.item.body {
        ItemBody::User { text } => html! { <div class="msg user"><div class="bubble">{ text }</div></div> },
        ItemBody::Assistant { text } => html! { <div class="msg assistant"><div class="bubble md-bubble"><Markdown source={text.clone()} /></div></div> },
        ItemBody::Tool(tool) => html! { <ToolCard tool={tool.clone()} /> },
        ItemBody::Notice { level, text } => {
            let class = match level {
                NoticeLevel::Retry => "notice retry",
                NoticeLevel::Warning => "notice warning",
            };
            html! { <div class={class} role="status">{ text }</div> }
        }
    }
}

#[derive(Properties, PartialEq)]
struct ToolProps {
    tool: ToolItem,
}

#[function_component(ToolCard)]
fn tool_card(props: &ToolProps) -> Html {
    let open = use_state(|| false);
    let tool = &props.tool;
    let (chip_class, chip_text) = match &tool.state {
        ToolState::Running => ("running", "running".to_string()),
        ToolState::Ok { .. } => ("ok", "ok".to_string()),
        ToolState::Err { class, .. } => ("bad", class.replace('_', " ")),
        ToolState::Unfinished => ("warn", "did not finish".to_string()),
    };
    let denied = tool.gate.as_ref().is_some_and(|g| !g.allowed);
    let toggle = {
        let open = open.clone();
        Callback::from(move |_| open.set(!*open))
    };
    let duration = tool.duration_ms.map(fmt::ms).unwrap_or_default();
    let args_line = fmt::args_summary(&tool.arguments);

    html! {
        <div class={classes!("tool", chip_class, denied.then_some("denied"))}>
            <button class="tool-head" onclick={toggle} aria-expanded={(*open).to_string()}>
                <span class="caret">{ if *open { "▾" } else { "▸" } }</span>
                <span class="tool-name">{ &tool.name }</span>
                <span class="tool-args">{ args_line }</span>
                <span class={classes!("chip", chip_class)}>{ chip_text }</span>
                <span class="tool-time">{ duration }</span>
            </button>
            if *open {
                <div class="tool-body">
                    <div class="label">{"Arguments"}</div>
                    <pre>{ serde_json::to_string_pretty(&tool.arguments).unwrap_or_default() }</pre>
                    if let Some(gate) = &tool.gate {
                        if !gate.reason.is_empty() && gate.reason != "read-only tool" {
                            <div class="label">{"Permission"}</div>
                            <div class="gate">{ if gate.allowed { "Allowed" } else { "Denied" } }{" - "}{ &gate.reason }</div>
                        }
                    }
                    { match &tool.state {
                        ToolState::Ok { preview, bytes, truncated } => html! {
                            <>
                                <div class="label">{ format!("Result ({bytes} bytes{})", if *truncated { ", truncated" } else { "" }) }</div>
                                <pre>{ preview }</pre>
                            </>
                        },
                        ToolState::Err { class, message } => html! {
                            <>
                                <div class="label">{ format!("Error: {}", class.replace('_', " ")) }</div>
                                <pre class="error-text">{ message }</pre>
                            </>
                        },
                        ToolState::Running => html! { <div class="label">{"Waiting for the result..."}</div> },
                        ToolState::Unfinished => html! { <div class="label">{"The log ended before this call finished."}</div> },
                    } }
                </div>
            }
        </div>
    }
}

// ---------------------------------------------------------------- banner

#[derive(Properties, PartialEq)]
pub struct BannerProps {
    pub banner: Banner,
    pub on_dismiss: Callback<()>,
    /// Shown when set, to take the user to settings.
    pub on_open_settings: Option<Callback<()>>,
}

#[function_component(BannerView)]
pub fn banner_view(props: &BannerProps) -> Html {
    let class = match props.banner.kind {
        BannerKind::Error => "banner error",
        BannerKind::Info => "banner info",
        BannerKind::Warn => "banner warn",
    };
    let dismiss = {
        let cb = props.on_dismiss.clone();
        Callback::from(move |_| cb.emit(()))
    };
    html! {
        <div class={class} role="alert">
            <span class="banner-text">{ &props.banner.text }</span>
            if let Some(open) = &props.on_open_settings {
                <button class="link" onclick={let open = open.clone(); Callback::from(move |_| open.emit(()))}>{"Open settings"}</button>
            }
            <button class="icon" aria-label="Dismiss" onclick={dismiss}>{"×"}</button>
        </div>
    }
}

// ---------------------------------------------------------------- composer

#[derive(Properties, PartialEq)]
pub struct ComposerProps {
    pub enabled: bool,
    pub running: bool,
    pub cancelling: bool,
    pub placeholder: AttrValue,
    pub on_send: Callback<String>,
    pub on_stop: Callback<()>,
}

#[function_component(Composer)]
pub fn composer(props: &ComposerProps) -> Html {
    let text = use_state(String::new);
    let area = use_node_ref();

    let oninput = {
        let text = text.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(t) = e.target().and_then(|t| t.dyn_into::<HtmlTextAreaElement>().ok()) {
                text.set(t.value());
            }
        })
    };
    let submit = {
        let text = text.clone();
        let on_send = props.on_send.clone();
        let enabled = props.enabled;
        Callback::from(move |_: ()| {
            let value = (*text).trim().to_string();
            if enabled && !value.is_empty() {
                on_send.emit(value);
                text.set(String::new());
            }
        })
    };
    let onkeydown = {
        let submit = submit.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Enter" && !e.shift_key() && !e.is_composing() {
                e.prevent_default();
                submit.emit(());
            }
        })
    };
    let send_click = {
        let submit = submit.clone();
        Callback::from(move |_| submit.emit(()))
    };
    let stop_click = {
        let on_stop = props.on_stop.clone();
        Callback::from(move |_| on_stop.emit(()))
    };
    {
        // Put the cursor back after a send finishes.
        let area = area.clone();
        use_effect_with(props.running, move |running| {
            if !*running {
                if let Some(el) = area.cast::<HtmlTextAreaElement>() {
                    let _ = el.focus();
                }
            }
            || {}
        });
    }

    html! {
        <div class="composer">
            <textarea ref={area} rows="2" value={(*text).clone()} {oninput} {onkeydown}
                disabled={!props.enabled && !props.running}
                placeholder={props.placeholder.clone()} aria-label="Message"></textarea>
            if props.running {
                <button class="stop" onclick={stop_click} disabled={props.cancelling}>{ if props.cancelling { "Stopping..." } else { "Stop" } }</button>
            } else {
                <button class="send" onclick={send_click} disabled={!props.enabled || text.trim().is_empty()}>{"Send"}</button>
            }
        </div>
    }
}

// ---------------------------------------------------------------- status bar

#[derive(Properties, PartialEq)]
pub struct StatusProps {
    pub model: TimelineModel,
    pub running: bool,
    pub read_only: bool,
    pub last_status: Option<SessionStatus>,
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[function_component(StatusBar)]
pub fn status_bar(props: &StatusProps) -> Html {
    let tick = use_state(|| 0u32);
    {
        let tick = tick.clone();
        use_effect_with(props.running, move |running| {
            let mut guard: Option<(i32, Closure<dyn FnMut()>)> = None;
            if *running {
                if let Some(window) = web_sys::window() {
                    let tick2 = tick.clone();
                    let closure = Closure::<dyn FnMut()>::new(move || tick2.set(*tick2 + 1));
                    if let Ok(id) = window.set_interval_with_callback_and_timeout_and_arguments_0(closure.as_ref().unchecked_ref(), 1000) {
                        guard = Some((id, closure));
                    }
                }
            }
            move || {
                if let (Some((id, _closure)), Some(window)) = (guard, web_sys::window()) {
                    window.clear_interval_with_handle(id);
                }
            }
        });
    }

    let m = &props.model;
    let elapsed = match (&m.duration_ms, &m.started_ts) {
        (Some(d), _) => Some(*d),
        (None, Some(ts)) if props.running => {
            let started = js_sys::Date::parse(ts);
            if started.is_nan() { None } else { Some((now_ms() - started).max(0.0) as u64) }
        }
        _ => None,
    };
    let (state_class, state_text) = match (&m.status, props.last_status, props.running) {
        (_, _, true) => ("running", "working".to_string()),
        (RunStatus::Ended(s), _, _) => (fmt::status_class(*s), fmt::status_label(*s).to_string()),
        (RunStatus::Unfinished, _, _) => ("warn", "did not finish".to_string()),
        (_, Some(s), _) => (fmt::status_class(s), fmt::status_label(s).to_string()),
        _ => ("idle", if props.read_only { "recorded".to_string() } else { "ready".to_string() }),
    };
    let _ = *tick;

    html! {
        <footer class="statusbar">
            <span class={classes!("dot", state_class)}></span>
            <span class="state">{ state_text }</span>
            <span class="sep">{"·"}</span>
            <span>{ m.model.clone().unwrap_or_else(|| "-".into()) }</span>
            <span class="sep">{"·"}</span>
            <span>{ fmt::plural(m.steps as u64, "step") }</span>
            <span class="sep">{"·"}</span>
            <span>{ format!("{}{}", fmt::plural(m.tool_calls as u64, "tool call"), if m.tool_errors > 0 { format!(" ({} failed)", m.tool_errors) } else { String::new() }) }</span>
            <span class="sep">{"·"}</span>
            <span title="input / output tokens">{ format!("{} in / {} out", fmt::tokens(m.usage.input_tokens), fmt::tokens(m.usage.output_tokens)) }</span>
            if let Some(ms) = elapsed {
                <span class="sep">{"·"}</span>
                <span>{ fmt::ms(ms) }</span>
            }
        </footer>
    }
}

// ---------------------------------------------------------------- sidebar

#[derive(Properties, PartialEq)]
pub struct SidebarProps {
    pub workspace: Option<String>,
    pub recent: Vec<String>,
    pub sessions: Vec<Summary>,
    pub live_id: Option<String>,
    pub live_active: bool,
    pub selected_history: Option<String>,
    pub busy: bool,
    pub on_pick: Callback<()>,
    pub on_choose_recent: Callback<String>,
    pub on_select: Callback<String>,
    pub on_show_live: Callback<()>,
    pub on_new: Callback<()>,
    pub on_settings: Callback<()>,
}

#[function_component(Sidebar)]
pub fn sidebar(props: &SidebarProps) -> Html {
    let pick = { let cb = props.on_pick.clone(); Callback::from(move |_| cb.emit(())) };
    let settings = { let cb = props.on_settings.clone(); Callback::from(move |_| cb.emit(())) };
    let new = { let cb = props.on_new.clone(); Callback::from(move |_| cb.emit(())) };
    let show_live = { let cb = props.on_show_live.clone(); Callback::from(move |_| cb.emit(())) };
    let choose = {
        let cb = props.on_choose_recent.clone();
        Callback::from(move |e: Event| {
            if let Some(sel) = e.target().and_then(|t| t.dyn_into::<HtmlSelectElement>().ok()) {
                let v = sel.value();
                if !v.is_empty() {
                    cb.emit(v);
                }
            }
        })
    };
    let live_selected = props.selected_history.is_none();

    html! {
        <aside class="sidebar">
            <div class="brand"><span class="bolt">{"⚡"}</span>{"Lightning"}</div>

            <section class="workspace">
                <div class="label">{"Workspace"}</div>
                <div class="ws-name" title={props.workspace.clone().unwrap_or_default()}>
                    { props.workspace.as_deref().map(fmt::folder_name).unwrap_or_else(|| "No folder chosen".into()) }
                </div>
                <div class="ws-actions">
                    <button onclick={pick} disabled={props.busy}>{"Choose folder..."}</button>
                    if props.recent.len() > 1 {
                        <select onchange={choose} disabled={props.busy} aria-label="Recent workspaces">
                            <option value="" selected=true>{"Recent"}</option>
                            { for props.recent.iter().map(|w| html! { <option value={w.clone()}>{ fmt::folder_name(w) }</option> }) }
                        </select>
                    }
                </div>
            </section>

            <section class="sessions">
                <div class="sessions-head">
                    <div class="label">{"Sessions"}</div>
                    <button class="small" onclick={new} disabled={props.busy || props.workspace.is_none()}>{"New session"}</button>
                </div>
                <ul>
                    if props.live_active {
                        <li class={classes!("session", "live", live_selected.then_some("selected"))}>
                            <button onclick={show_live}>
                                <span class="s-top"><span class="chip running">{"live"}</span><span class="s-time">{"this session"}</span></span>
                                <span class="s-task">{ props.live_id.clone().map(|id| fmt::session_time(&id)).unwrap_or_default() }</span>
                            </button>
                        </li>
                    }
                    { for props.sessions.iter().filter(|s| Some(&s.session_id) != props.live_id.as_ref() || !props.live_active).map(|s| {
                        let id = s.session_id.clone();
                        let selected = props.selected_history.as_deref() == Some(id.as_str());
                        let on_select = props.on_select.clone();
                        let (cls, label) = match s.status {
                            Some(st) => (fmt::status_class(st), fmt::status_label(st).to_string()),
                            None => ("warn", "unfinished".to_string()),
                        };
                        html! {
                            <li class={classes!("session", selected.then_some("selected"))} key={id.clone()}>
                                <button onclick={Callback::from(move |_| on_select.emit(id.clone()))}>
                                    <span class="s-top">
                                        <span class={classes!("chip", cls)}>{ label }</span>
                                        <span class="s-time">{ fmt::session_time(&s.session_id) }</span>
                                    </span>
                                    <span class="s-task">{ fmt::shorten(s.task.as_deref().unwrap_or("(no message)"), 70) }</span>
                                    <span class="s-meta">
                                        { fmt::plural(s.tool_calls as u64, "tool") }
                                        if s.tool_errors > 0 { { format!(" · {} failed", s.tool_errors) } }
                                        if let Some(d) = s.duration_ms { { format!(" · {}", fmt::ms(d)) } }
                                    </span>
                                </button>
                            </li>
                        }
                    }) }
                </ul>
                if props.sessions.is_empty() && !props.live_active {
                    <p class="muted">{"No sessions yet in this folder."}</p>
                }
            </section>

            <button class="settings-btn" onclick={settings}>{"Settings"}</button>
        </aside>
    }
}

// ---------------------------------------------------------------- history header

#[derive(Properties, PartialEq)]
pub struct HistoryBarProps {
    pub view: HistoryView,
    pub on_back: Callback<()>,
}

#[function_component(HistoryBar)]
pub fn history_bar(props: &HistoryBarProps) -> Html {
    let back = { let cb = props.on_back.clone(); Callback::from(move |_| cb.emit(())) };
    let notice = if props.view.issues.truncated_last_line {
        Some("This log ends with a partial line, probably because the app was closed or crashed. The last line was ignored.")
    } else if props.view.model.status == RunStatus::Unfinished {
        Some("This session did not end cleanly.")
    } else {
        None
    };
    html! {
        <div class="history-bar">
            <div>
                <strong>{"Recorded session"}</strong>
                <span class="muted">{ format!(" · {}", fmt::session_time(&props.view.model.session_id)) }</span>
                if let Some(text) = notice { <div class="muted warn-text">{ text }</div> }
            </div>
            <button onclick={back}>{"Back to live"}</button>
        </div>
    }
}

// ---------------------------------------------------------------- approval modal

#[derive(Properties, PartialEq)]
pub struct ApprovalProps {
    pub request: ApprovalRequestDto,
    pub on_answer: Callback<bool>,
}

#[function_component(ApprovalModal)]
pub fn approval_modal(props: &ApprovalProps) -> Html {
    let root = use_node_ref();
    {
        let root = root.clone();
        use_effect_with(props.request.request_id.clone(), move |_| {
            if let Some(el) = root.cast::<web_sys::HtmlElement>() {
                let _ = el.focus();
            }
            || {}
        });
    }
    let allow = { let cb = props.on_answer.clone(); Callback::from(move |_| cb.emit(true)) };
    let deny = { let cb = props.on_answer.clone(); Callback::from(move |_| cb.emit(false)) };
    let onkeydown = {
        let cb = props.on_answer.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Escape" {
                cb.emit(false);
            }
        })
    };
    html! {
        <div class="overlay">
            <div class="modal" role="alertdialog" aria-modal="true" aria-labelledby="approval-title" tabindex="0" ref={root} {onkeydown}>
                <h2 id="approval-title">{"Allow this change?"}</h2>
                <p>{"The assistant wants to "}<strong>{ &props.request.summary }</strong>{"."}</p>
                <div class="modal-actions">
                    <button class="primary" onclick={allow}>{"Allow"}</button>
                    <button onclick={deny}>{"Deny"}</button>
                </div>
                <p class="muted">{"Esc denies."}</p>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------- settings

#[derive(Properties, PartialEq)]
pub struct SettingsProps {
    pub settings: SettingsDto,
    pub on_save: Callback<(String, PermissionMode)>,
    pub on_set_key: Callback<String>,
    pub on_close: Callback<()>,
}

#[function_component(SettingsPanel)]
pub fn settings_panel(props: &SettingsProps) -> Html {
    let model = use_state(|| props.settings.model.clone());
    let mode = use_state(|| props.settings.permission_mode);
    let key = use_state(String::new);

    let on_model = {
        let model = model.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(i) = e.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
                model.set(i.value());
            }
        })
    };
    let on_mode = {
        let mode = mode.clone();
        Callback::from(move |e: Event| {
            if let Some(s) = e.target().and_then(|t| t.dyn_into::<HtmlSelectElement>().ok()) {
                mode.set(match s.value().as_str() {
                    "allow" => PermissionMode::Allow,
                    "deny" => PermissionMode::Deny,
                    _ => PermissionMode::Ask,
                });
            }
        })
    };
    let on_key = {
        let key = key.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(i) = e.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
                key.set(i.value());
            }
        })
    };
    let save = {
        let cb = props.on_save.clone();
        let (model, mode) = (model.clone(), mode.clone());
        Callback::from(move |_| cb.emit(((*model).clone(), *mode)))
    };
    let set_key = {
        let cb = props.on_set_key.clone();
        let key = key.clone();
        Callback::from(move |_| {
            cb.emit((*key).clone());
            key.set(String::new());
        })
    };
    let close = { let cb = props.on_close.clone(); Callback::from(move |_| cb.emit(())) };
    let mode_value = match *mode {
        PermissionMode::Ask => "ask",
        PermissionMode::Allow => "allow",
        PermissionMode::Deny => "deny",
    };

    html! {
        <div class="overlay">
            <div class="modal wide" role="dialog" aria-modal="true" aria-labelledby="settings-title">
                <h2 id="settings-title">{"Settings"}</h2>

                <label class="field">
                    <span class="label">{"Gemini API key"}</span>
                    <span class="hint">{ if props.settings.key_present { "A key is saved. Paste a new one to replace it." } else { "No key yet. Paste your key; it is stored in the Windows credential store, never in a file." } }</span>
                    <span class="row">
                        <input type="password" autocomplete="off" placeholder="paste key" value={(*key).clone()} oninput={on_key} />
                        <button onclick={set_key} disabled={key.trim().is_empty()}>{"Save key"}</button>
                    </span>
                </label>

                <label class="field">
                    <span class="label">{"Model"}</span>
                    <input type="text" list="models" value={(*model).clone()} oninput={on_model} />
                    <datalist id="models">
                        { for props.settings.known_models.iter().map(|m| html! { <option value={m.clone()}></option> }) }
                    </datalist>
                </label>

                <label class="field">
                    <span class="label">{"When the assistant wants to change a file"}</span>
                    <select onchange={on_mode} value={mode_value}>
                        <option value="ask" selected={*mode == PermissionMode::Ask}>{"Ask me each time"}</option>
                        <option value="allow" selected={*mode == PermissionMode::Allow}>{"Allow without asking"}</option>
                        <option value="deny" selected={*mode == PermissionMode::Deny}>{"Never allow"}</option>
                    </select>
                </label>

                <div class="modal-actions">
                    <button class="primary" onclick={save}>{"Save"}</button>
                    <button onclick={close}>{"Close"}</button>
                </div>
            </div>
        </div>
    }
}
