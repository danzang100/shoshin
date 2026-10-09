//! G0 spike: proves the Tauri 2 APIs spec 002 depends on (commands, managed state,
//! camelCase argument mapping, error results, events, dialog plugin, linking the
//! `lightning` crate). Replaced by the real shell in task G5.
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Counter(AtomicU32);

#[derive(Serialize, Clone)]
struct Tick {
    n: u32,
}

#[derive(Serialize)]
struct Echo {
    session_id: String,
    after_seq: u64,
}

#[tauri::command]
fn ping(name: &str) -> String {
    format!("pong:{name}")
}

/// Managed state shared between commands.
#[tauri::command]
fn bump(counter: State<'_, Counter>) -> u32 {
    counter.0.fetch_add(1, Ordering::SeqCst) + 1
}

/// JS sends `{ sessionId, afterSeq }`; Rust receives snake_case parameters.
#[tauri::command]
fn echo_args(session_id: String, after_seq: u64) -> Echo {
    Echo { session_id, after_seq }
}

#[tauri::command]
fn always_fails() -> Result<(), String> {
    Err("expected failure from always_fails".to_string())
}

/// Emits `count` events from a background task, like the real `TauriSink` will.
#[tauri::command]
async fn emit_ticks(app: AppHandle, count: u32) -> Result<(), String> {
    tauri::async_runtime::spawn(async move {
        for n in 1..=count {
            tokio_sleep(Duration::from_millis(50)).await;
            let _ = app.emit("spike://tick", Tick { n });
        }
    });
    Ok(())
}

async fn tokio_sleep(d: Duration) {
    tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await.ok();
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked.map(|p| p.to_string()))
}

/// Proves the `lightning` library links into the shell.
#[tauri::command]
fn core_session_id() -> String {
    lightning::obs::SessionId::new().to_string()
}

/// Lets the self-test running in the webview write its results to the dev console.
#[tauri::command]
fn report(line: String) {
    println!("SPIKE-REPORT: {line}");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Counter::default())
        .invoke_handler(tauri::generate_handler![
            ping,
            bump,
            echo_args,
            always_fails,
            emit_ticks,
            pick_folder,
            core_session_id,
            report
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
