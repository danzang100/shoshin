//! Root component: owns the state and wires components to the backend.
use std::cell::RefCell;
use std::rc::Rc;

use lightning_protocol::SessionStatus;
use lightning_protocol::dto::{PermissionMode, SaveSettingsDto, StartRequest};
use lightning_protocol::view::TimelineModel;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api;
use crate::components::{
    ApprovalModal, BannerView, Composer, HistoryBar, SettingsPanel, Sidebar, StatusBar, Timeline,
};
use crate::fmt;
use crate::state::{Action, AppState};

type Dispatcher = UseReducerDispatcher<AppState>;

async fn refresh_sessions(d: &Dispatcher, workspace: &str) {
    match api::list_sessions(workspace).await {
        Ok(list) => d.dispatch(Action::SessionsLoaded(list)),
        Err(e) => d.dispatch(Action::Error(e)),
    }
}

async fn startup(d: Dispatcher) {
    match api::get_settings().await {
        Ok(settings) => {
            let ws = settings.last_workspace.clone();
            d.dispatch(Action::SettingsLoaded(settings));
            if let Some(ws) = ws {
                refresh_sessions(&d, &ws).await;
            }
        }
        Err(e) => d.dispatch(Action::Error(e)),
    }
    // A reloaded window re-attaches to a session that is still alive in the backend.
    if let Ok(Some(active)) = api::get_active().await {
        let ws = active.info.workspace.clone();
        let events = api::session_events(&ws, &active.info.session_id, 0).await.unwrap_or_default();
        d.dispatch(Action::WorkspaceChosen(ws.clone()));
        d.dispatch(Action::Restored { info: active.info, events, running: active.running });
        refresh_sessions(&d, &ws).await;
    }
}

#[function_component(App)]
pub fn app() -> Html {
    let state = use_reducer(AppState::default);

    // Subscribe to backend events once, and load settings and history.
    {
        let dispatcher = state.dispatcher();
        use_effect_with((), move |_| {
            let holder: Rc<RefCell<Option<api::Subscription>>> = Rc::new(RefCell::new(None));
            let slot = holder.clone();
            let d = dispatcher.clone();
            spawn_local(async move {
                let handlers = api::Handlers {
                    event: { let d = d.clone(); Callback::from(move |e| d.dispatch(Action::Event(e))) },
                    approval: { let d = d.clone(); Callback::from(move |a| d.dispatch(Action::Approval(a))) },
                    approval_closed: { let d = d.clone(); Callback::from(move |id| d.dispatch(Action::ApprovalClosed(id))) },
                    turn_finished: { let d = d.clone(); Callback::from(move |t| d.dispatch(Action::TurnFinished(t))) },
                };
                let sub = api::subscribe(handlers).await;
                *slot.borrow_mut() = Some(sub);
            });
            spawn_local(startup(dispatcher));
            move || {
                holder.borrow_mut().take();
            }
        });
    }

    // Refresh the session list whenever a turn ends.
    {
        let dispatcher = state.dispatcher();
        let workspace = state.workspace.clone();
        use_effect_with(state.running, move |running| {
            if !*running {
                if let Some(ws) = workspace {
                    spawn_local(async move { refresh_sessions(&dispatcher, &ws).await });
                }
            }
            || {}
        });
    }

    let on_pick = {
        let d = state.dispatcher();
        Callback::from(move |_: ()| {
            let d = d.clone();
            spawn_local(async move {
                match api::pick_workspace().await {
                    Ok(Some(ws)) => {
                        d.dispatch(Action::WorkspaceChosen(ws.clone()));
                        refresh_sessions(&d, &ws).await;
                    }
                    Ok(None) => {}
                    Err(e) => d.dispatch(Action::Error(e)),
                }
            });
        })
    };
    let on_choose_recent = {
        let d = state.dispatcher();
        Callback::from(move |ws: String| {
            let d = d.clone();
            spawn_local(async move {
                d.dispatch(Action::WorkspaceChosen(ws.clone()));
                refresh_sessions(&d, &ws).await;
            });
        })
    };
    let on_send = {
        let state = state.clone();
        Callback::from(move |text: String| {
            let Some(workspace) = state.workspace.clone() else { return };
            let reuse = if state.live_can_continue() {
                state.live.as_ref().and_then(|l| l.info.as_ref()).map(|i| i.session_id.clone())
            } else {
                None
            };
            let d = state.dispatcher();
            d.dispatch(Action::Sending);
            spawn_local(async move {
                let session_id = match reuse {
                    Some(id) => id,
                    None => match api::start_session(StartRequest { workspace, model: None, permission_mode: None }).await {
                        Ok(info) => {
                            let id = info.session_id.clone();
                            d.dispatch(Action::SessionStarted(info));
                            id
                        }
                        Err(e) => {
                            d.dispatch(Action::SendFailed(e));
                            return;
                        }
                    },
                };
                if let Err(e) = api::send_message(&session_id, &text).await {
                    d.dispatch(Action::SendFailed(e));
                }
            });
        })
    };
    let on_stop = {
        let state = state.clone();
        Callback::from(move |_: ()| {
            let Some(id) = state.live.as_ref().and_then(|l| l.info.as_ref()).map(|i| i.session_id.clone()) else { return };
            let d = state.dispatcher();
            d.dispatch(Action::Cancelling);
            spawn_local(async move {
                if let Err(e) = api::cancel_turn(&id).await {
                    d.dispatch(Action::Error(e));
                }
            });
        })
    };
    let on_answer = {
        let state = state.clone();
        Callback::from(move |allow: bool| {
            let Some(req) = state.approval.clone() else { return };
            let d = state.dispatcher();
            d.dispatch(Action::ApprovalClosed(req.request_id.clone()));
            spawn_local(async move {
                if let Err(e) = api::respond_approval(&req.request_id, allow).await {
                    d.dispatch(Action::Error(e));
                }
            });
        })
    };
    let on_select = {
        let state = state.clone();
        Callback::from(move |id: String| {
            let Some(ws) = state.workspace.clone() else { return };
            let d = state.dispatcher();
            spawn_local(async move {
                match api::load_session(&ws, &id).await {
                    Ok(dto) => d.dispatch(Action::HistoryLoaded(dto)),
                    Err(e) => d.dispatch(Action::Error(e)),
                }
            });
        })
    };
    let on_show_live = {
        let d = state.dispatcher();
        Callback::from(move |_: ()| d.dispatch(Action::CloseHistory))
    };
    let on_new = {
        let state = state.clone();
        Callback::from(move |_: ()| {
            let d = state.dispatcher();
            let ended = state.live.as_ref().and_then(|l| l.info.as_ref()).map(|i| i.session_id.clone());
            d.dispatch(Action::NewSession);
            if let Some(id) = ended.filter(|_| !state.running) {
                // Finalise the old session's logs; failures here do not matter to the user.
                spawn_local(async move {
                    let _ = api::end_session(&id).await;
                });
            }
        })
    };
    let on_settings = {
        let d = state.dispatcher();
        Callback::from(move |_: ()| d.dispatch(Action::SettingsOpen(true)))
    };
    let on_close_settings = {
        let d = state.dispatcher();
        Callback::from(move |_: ()| d.dispatch(Action::SettingsOpen(false)))
    };
    let on_save_settings = {
        let d = state.dispatcher();
        Callback::from(move |(model, mode): (String, PermissionMode)| {
            let d = d.clone();
            spawn_local(async move {
                match api::save_settings(SaveSettingsDto { model, permission_mode: mode }).await {
                    Ok(s) => {
                        d.dispatch(Action::SettingsLoaded(s));
                        d.dispatch(Action::Notice("Settings saved.".into()));
                    }
                    Err(e) => d.dispatch(Action::Error(e)),
                }
            });
        })
    };
    let on_set_key = {
        let d = state.dispatcher();
        Callback::from(move |key: String| {
            let d = d.clone();
            spawn_local(async move {
                match api::set_api_key(key).await {
                    Ok(s) => {
                        d.dispatch(Action::SettingsLoaded(s));
                        d.dispatch(Action::Notice("API key saved.".into()));
                    }
                    Err(e) => d.dispatch(Action::Error(e)),
                }
            });
        })
    };
    let on_dismiss = {
        let d = state.dispatcher();
        Callback::from(move |_: ()| d.dispatch(Action::DismissBanner))
    };

    let st = &*state;
    let (model, read_only, last_status) = match (&st.history, &st.live) {
        (Some(h), _) => (h.model.clone(), true, None),
        (None, Some(l)) => (l.model.clone(), false, l.last_status),
        (None, None) => (TimelineModel::new(), false, None),
    };
    let live_id = st.live.as_ref().and_then(|l| l.info.as_ref().map(|i| i.session_id.clone()).or_else(|| Some(l.model.session_id.clone()).filter(|s| !s.is_empty())));
    let selected_history = st.history.as_ref().map(|h| h.model.session_id.clone());
    let needs_new = st.live.is_some() && !st.live_can_continue();
    let empty = model.items.is_empty() && !st.running;
    let ws_label = st.workspace.as_deref().map(fmt::folder_name);

    let placeholder = if st.workspace.is_none() {
        "Choose a folder first"
    } else if !st.key_present() {
        "Add your API key in Settings first"
    } else {
        "Ask the assistant to look at or change files. Enter sends, Shift+Enter adds a line."
    };

    let suggestions = ["List the files and summarise Cargo.toml", "Read missing.txt and tell me what it says", "Create notes.txt with a one-line summary"];

    html! {
        <div class="app">
            <Sidebar
                workspace={st.workspace.clone()}
                recent={st.settings.as_ref().map(|s| s.recent_workspaces.clone()).unwrap_or_default()}
                sessions={st.sessions.clone()}
                live_id={live_id.clone()}
                live_active={st.live.is_some()}
                selected_history={selected_history}
                busy={st.running}
                on_pick={on_pick.clone()}
                on_choose_recent={on_choose_recent}
                on_select={on_select}
                on_show_live={on_show_live.clone()}
                on_new={on_new.clone()}
                on_settings={on_settings.clone()}
            />
            <main class="main">
                if let Some(banner) = st.banner.clone().or_else(|| st.missing_key_banner()) {
                    <BannerView banner={banner} on_dismiss={on_dismiss}
                        on_open_settings={(!st.key_present() && st.settings.is_some()).then(|| on_settings.clone())} />
                }
                if let Some(h) = &st.history {
                    <HistoryBar view={h.clone()} on_back={on_show_live} />
                }
                if empty {
                    <div class="empty">
                        if st.workspace.is_none() {
                            <h1>{"Pick a folder to work in"}</h1>
                            <p>{"Lightning can read and change files inside the folder you choose, and nothing outside it."}</p>
                            <button class="primary" onclick={let cb = on_pick.clone(); Callback::from(move |_| cb.emit(()))}>{"Choose folder..."}</button>
                        } else if st.history.is_some() {
                            <p class="muted">{"This recorded session has no messages."}</p>
                        } else {
                            <h1>{ format!("What should we do in {}?", ws_label.clone().unwrap_or_default()) }</h1>
                            <div class="suggestions">
                                { for suggestions.iter().map(|text| {
                                    let on_send = on_send.clone();
                                    let t = text.to_string();
                                    let enabled = st.can_send();
                                    html! { <button class="chip-btn" disabled={!enabled} onclick={Callback::from(move |_| on_send.emit(t.clone()))}>{ *text }</button> }
                                }) }
                            </div>
                        }
                    </div>
                } else {
                    <Timeline model={model.clone()} working={st.running && !read_only} />
                }

                if !read_only {
                    if needs_new {
                        <div class="newbar">
                            <span>{ format!("This session ended ({}).", last_status.map(fmt::status_label).unwrap_or("stopped")) }</span>
                            <button class="primary" onclick={let cb = on_new.clone(); Callback::from(move |_| cb.emit(()))}>{"New session"}</button>
                        </div>
                    } else {
                        <Composer enabled={st.can_send()} running={st.running} cancelling={st.cancelling}
                            placeholder={placeholder} on_send={on_send.clone()} on_stop={on_stop} />
                    }
                }
                <StatusBar model={model} running={st.running && !read_only} read_only={read_only} last_status={last_status.filter(|s| *s != SessionStatus::Completed || st.live.is_some())} />
            </main>

            if let Some(req) = &st.approval {
                <ApprovalModal request={req.clone()} on_answer={on_answer} />
            }
            if st.settings_open {
                if let Some(settings) = &st.settings {
                    <SettingsPanel settings={settings.clone()} on_save={on_save_settings} on_set_key={on_set_key} on_close={on_close_settings} />
                }
            }
        </div>
    }
}
