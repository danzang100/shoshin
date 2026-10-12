//! Tauri shell for lightning (spec 002, section 8): thin glue between the webview and
//! `lightning::host`. Anything testable without Tauri lives in `logic`.
mod logic;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lightning::host::{HostConfig, HostEmitter, HostError, SessionHost};
use lightning::llm::GeminiClient;
use lightning::obs::Event;
use lightning_protocol::Summary;
use lightning_protocol::dto::{
    ActiveInfoDto, ApprovalRequestDto, LoadedSessionDto, SaveSettingsDto, SessionInfoDto, SettingsDto, StartRequest,
    TurnFinishedDto,
};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_dialog::DialogExt;

use logic::{CredentialStore, KeyStore, StoredSettings};

const EVENT: &str = "lightning://event";
const APPROVAL: &str = "lightning://approval";
const APPROVAL_CLOSED: &str = "lightning://approval-closed";
const TURN_FINISHED: &str = "lightning://turn-finished";

/// Forwards host output to the webview.
struct TauriEmitter(AppHandle);

impl HostEmitter for TauriEmitter {
    fn event(&self, event: &Event) {
        let _ = self.0.emit(EVENT, event);
    }

    fn approval_requested(&self, request: &ApprovalRequestDto) {
        let _ = self.0.emit(APPROVAL, request);
    }

    fn approval_closed(&self, request_id: &str) {
        let _ = self.0.emit(APPROVAL_CLOSED, serde_json::json!({ "request_id": request_id }));
    }

    fn turn_finished(&self, result: &TurnFinishedDto) {
        let _ = self.0.emit(TURN_FINISHED, result);
    }
}

struct AppState {
    host: Arc<SessionHost<GeminiClient>>,
    settings: Mutex<StoredSettings>,
    settings_path: PathBuf,
    keys: Arc<dyn KeyStore>,
}

impl AppState {
    fn api_key(&self) -> Option<String> {
        logic::resolve_api_key(self.keys.as_ref(), std::env::var("GEMINI_API_KEY").ok())
    }

    fn settings(&self) -> std::sync::MutexGuard<'_, StoredSettings> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self) {
        let snapshot = self.settings().clone();
        if let Err(err) = snapshot.save(&self.settings_path) {
            eprintln!("warning: could not save settings: {err}");
        }
    }
}

fn host_err(err: HostError) -> String {
    err.to_string()
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> SettingsDto {
    let key_present = state.api_key().is_some();
    state.settings().to_dto(key_present)
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: SaveSettingsDto) -> SettingsDto {
    state.settings().apply(&settings);
    state.persist();
    get_settings(state)
}

/// Stores the key in the Windows credential store. The value is never returned or logged.
#[tauri::command]
fn set_api_key(state: State<'_, AppState>, key: String) -> Result<SettingsDto, String> {
    let key = logic::clean_api_key(&key)?;
    state.keys.set(&key)?;
    Ok(get_settings(state))
}

#[tauri::command]
async fn pick_workspace(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked.map(|p| p.to_string()))
}

#[tauri::command]
async fn start_session(state: State<'_, AppState>, req: StartRequest) -> Result<SessionInfoDto, String> {
    logic::validate_workspace(&req.workspace)?;
    let key = state
        .api_key()
        .ok_or_else(|| "No API key yet. Open Settings and add your Gemini API key.".to_string())?;
    state.host.set_secrets(vec![key]);

    let (model, mode) = {
        let s = state.settings();
        (req.model.clone().unwrap_or_else(|| s.effective_model()), req.permission_mode.unwrap_or(s.permission_mode))
    };
    let req = StartRequest { model: Some(model), permission_mode: Some(mode), ..req };
    let workspace = req.workspace.clone();

    let info = state.host.start(req).await.map_err(host_err)?;
    state.settings().remember_workspace(&workspace);
    state.persist();
    Ok(info)
}

#[tauri::command]
fn send_message(state: State<'_, AppState>, session_id: String, text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Type a message first.".into());
    }
    state.host.send(&session_id, text).map_err(host_err)
}

#[tauri::command]
fn cancel_turn(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.host.cancel(&session_id).map_err(host_err)
}

#[tauri::command]
async fn end_session(state: State<'_, AppState>, session_id: String) -> Result<Summary, String> {
    state.host.end(&session_id).await.map_err(host_err)
}

#[tauri::command]
fn respond_approval(state: State<'_, AppState>, request_id: String, allow: bool) -> Result<(), String> {
    state.host.respond_approval(&request_id, allow).map_err(host_err)
}

#[tauri::command]
fn get_active(state: State<'_, AppState>) -> Option<ActiveInfoDto> {
    state.host.get_active()
}

#[tauri::command]
fn list_sessions(workspace: String) -> Result<Vec<Summary>, String> {
    logic::list_sessions(&workspace)
}

#[tauri::command]
fn load_session(workspace: String, session_id: String) -> Result<LoadedSessionDto, String> {
    logic::load_session(&workspace, &session_id)
}

#[tauri::command]
fn session_events(workspace: String, session_id: String, after_seq: u64) -> Result<Vec<Event>, String> {
    logic::session_events(&workspace, &session_id, after_seq)
}

/// End-to-end test hooks. They do nothing unless `LIGHTNING_GUI_SELFTEST` names a plan file.
fn selftest_enabled() -> bool {
    std::env::var_os("LIGHTNING_GUI_SELFTEST").is_some()
}

#[tauri::command]
fn selftest_plan() -> Option<serde_json::Value> {
    let path = std::env::var("LIGHTNING_GUI_SELFTEST").ok()?;
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[tauri::command]
fn selftest_report(line: String) {
    if selftest_enabled() {
        println!("SELFTEST {line}");
    }
}

#[tauri::command]
fn selftest_done(app: AppHandle, code: i32) {
    if selftest_enabled() {
        app.exit(code);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // LIGHTNING_GUI_DATA_DIR lets dev and test runs keep their settings apart from the real ones.
            let data_dir = match std::env::var_os("LIGHTNING_GUI_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            let settings_path = data_dir.join("settings.json");
            let keys: Arc<dyn KeyStore> = Arc::new(CredentialStore);
            let stored = StoredSettings::load(&settings_path);

            let key_source = keys.clone();
            let make_llm = move |req: &StartRequest| {
                let key = logic::resolve_api_key(key_source.as_ref(), std::env::var("GEMINI_API_KEY").ok())
                    .ok_or_else(|| HostError::Setup("No API key yet. Open Settings and add your Gemini API key.".into()))?;
                let model = req.model.clone().unwrap_or_else(|| logic::DEFAULT_MODEL.to_string());
                Ok(GeminiClient::new(key, model))
            };

            let rt = tauri::async_runtime::block_on(async { tokio::runtime::Handle::current() });
            let config = HostConfig::new("gemini", stored.effective_model());
            let host = SessionHost::new(Arc::new(TauriEmitter(app.handle().clone())), config, make_llm, rt);

            app.manage(AppState { host: Arc::new(host), settings: Mutex::new(stored), settings_path, keys });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            set_api_key,
            pick_workspace,
            start_session,
            send_message,
            cancel_turn,
            end_session,
            respond_approval,
            get_active,
            list_sessions,
            load_session,
            session_events,
            selftest_plan,
            selftest_report,
            selftest_done,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        // Closing the window ends the session cleanly so summary.json is written.
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            api.prevent_exit();
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let host = handle.state::<AppState>().host.clone();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(6), host.shutdown()).await;
                handle.exit(0);
            });
        }
    });
}
