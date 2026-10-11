//! Shell logic with no Tauri types, so it is unit-tested natively: settings file, API key
//! resolution, input validation and history loading (spec 002, sections 8.1-8.4).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lightning::obs::inspect;
use lightning_protocol::Summary;
use lightning_protocol::dto::{LoadedSessionDto, PermissionMode, SaveSettingsDto, SettingsDto};
use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL: &str = lightning::llm::gemini::DEFAULT_MODEL;
pub const KNOWN_MODELS: &[&str] = &["gemini-3.5-flash-lite"];
const MAX_RECENT: usize = 8;

/// What is saved in `settings.json` in the app data folder. No secrets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredSettings {
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub recent_workspaces: Vec<String>,
    pub last_workspace: Option<String>,
}

impl StoredSettings {
    /// Missing or unreadable files give defaults; a corrupt file must not stop the app.
    pub fn load(path: &Path) -> StoredSettings {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Writes via a temp file and rename so a crash cannot leave half a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self).map_err(io::Error::from)?)?;
        fs::rename(&tmp, path)
    }

    pub fn remember_workspace(&mut self, workspace: &str) {
        self.recent_workspaces.retain(|w| w != workspace);
        self.recent_workspaces.insert(0, workspace.to_string());
        self.recent_workspaces.truncate(MAX_RECENT);
        self.last_workspace = Some(workspace.to_string());
    }

    pub fn effective_model(&self) -> String {
        self.model.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }

    pub fn apply(&mut self, save: &SaveSettingsDto) {
        let model = save.model.trim();
        self.model = if model.is_empty() { None } else { Some(model.to_string()) };
        self.permission_mode = save.permission_mode;
    }

    pub fn to_dto(&self, key_present: bool) -> SettingsDto {
        SettingsDto {
            model: self.effective_model(),
            default_model: DEFAULT_MODEL.to_string(),
            known_models: KNOWN_MODELS.iter().map(|m| m.to_string()).collect(),
            permission_mode: self.permission_mode,
            recent_workspaces: self.recent_workspaces.clone(),
            last_workspace: self.last_workspace.clone(),
            key_present,
        }
    }
}

/// Where the API key lives. The Windows credential store in the app; a map in tests.
pub trait KeyStore: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, key: &str) -> Result<(), String>;
}

pub struct CredentialStore;

impl CredentialStore {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new("lightning", "gemini-api-key").map_err(|e| e.to_string())
    }
}

impl KeyStore for CredentialStore {
    fn get(&self) -> Option<String> {
        Self::entry().ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
    }

    fn set(&self, key: &str) -> Result<(), String> {
        Self::entry()?.set_password(key).map_err(|e| format!("could not save the key: {e}"))
    }
}

/// In-memory key store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeyStore(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, key: &str) -> Result<(), String> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.to_string());
        Ok(())
    }
}

/// Credential store first, then the environment (`.env` already loaded into it).
pub fn resolve_api_key(store: &dyn KeyStore, env: Option<String>) -> Option<String> {
    store
        .get()
        .or(env)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

pub fn clean_api_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("The API key is empty.".into());
    }
    if key.chars().any(char::is_whitespace) || key.len() > 512 {
        return Err("That does not look like an API key.".into());
    }
    Ok(key.to_string())
}

/// The workspace must be an existing folder.
pub fn validate_workspace(workspace: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(workspace.trim());
    if workspace.trim().is_empty() || !path.is_dir() {
        return Err(format!("{workspace} is not a folder."));
    }
    Ok(path)
}

fn home_of(workspace: &Path) -> PathBuf {
    workspace.join(".lightning")
}

pub fn list_sessions(workspace: &str) -> Result<Vec<Summary>, String> {
    let ws = validate_workspace(workspace)?;
    inspect::list_sessions(&home_of(&ws)).map_err(|e| e.to_string())
}

pub fn load_session(workspace: &str, session_id: &str) -> Result<LoadedSessionDto, String> {
    let ws = validate_workspace(workspace)?;
    inspect::load_session_dto(&home_of(&ws), session_id).map_err(|e| e.to_string())
}

pub fn session_events(workspace: &str, session_id: &str, after_seq: u64) -> Result<Vec<lightning_protocol::Event>, String> {
    let ws = validate_workspace(workspace)?;
    inspect::events_after(&home_of(&ws), session_id, after_seq).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightning::obs::{JsonlSink, SessionDir, SessionId};
    use lightning_protocol::fixtures;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightning-shell-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_round_trip_and_survive_corruption() {
        let dir = temp();
        let path = dir.join("nested").join("settings.json");
        assert_eq!(StoredSettings::load(&path), StoredSettings::default());

        let mut s = StoredSettings::default();
        s.apply(&SaveSettingsDto { model: " custom-model ".into(), permission_mode: PermissionMode::Deny });
        s.remember_workspace("C:\\a");
        s.save(&path).unwrap();
        assert_eq!(StoredSettings::load(&path), s);
        assert_eq!(s.effective_model(), "custom-model");

        fs::write(&path, "{ not json").unwrap();
        assert_eq!(StoredSettings::load(&path), StoredSettings::default());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn blank_model_falls_back_to_the_default_and_recents_are_capped_and_deduped() {
        let mut s = StoredSettings::default();
        s.apply(&SaveSettingsDto { model: "   ".into(), permission_mode: PermissionMode::Ask });
        assert_eq!(s.effective_model(), DEFAULT_MODEL);
        for i in 0..12 {
            s.remember_workspace(&format!("w{i}"));
        }
        s.remember_workspace("w5");
        assert_eq!(s.recent_workspaces.len(), MAX_RECENT);
        assert_eq!(s.recent_workspaces[0], "w5");
        assert_eq!(s.recent_workspaces.iter().filter(|w| *w == "w5").count(), 1);
        assert_eq!(s.last_workspace.as_deref(), Some("w5"));
    }

    #[test]
    fn the_key_never_appears_in_settings_files_or_dtos() {
        let store = MemoryKeyStore::default();
        store.set("sk-sentinel-secret-9999").unwrap();
        let key = resolve_api_key(&store, None).unwrap();
        let settings = StoredSettings::default();
        let dto = settings.to_dto(true);
        let dump = format!("{}{}", serde_json::to_string(&settings).unwrap(), serde_json::to_string(&dto).unwrap());
        assert!(!dump.contains(&key));
        assert!(dto.key_present);
    }

    #[test]
    fn key_resolution_prefers_the_store_then_the_environment() {
        let store = MemoryKeyStore::default();
        assert_eq!(resolve_api_key(&store, None), None);
        assert_eq!(resolve_api_key(&store, Some("  env-key ".into())).as_deref(), Some("env-key"));
        assert_eq!(resolve_api_key(&store, Some("  ".into())), None);
        store.set("store-key").unwrap();
        assert_eq!(resolve_api_key(&store, Some("env-key".into())).as_deref(), Some("store-key"));
    }

    #[test]
    fn api_key_input_is_cleaned_and_validated() {
        assert_eq!(clean_api_key("  abc123  ").unwrap(), "abc123");
        assert!(clean_api_key("   ").is_err());
        assert!(clean_api_key("two words").is_err());
        assert!(clean_api_key(&"k".repeat(600)).is_err());
    }

    #[test]
    fn workspace_must_be_an_existing_folder() {
        let dir = temp();
        assert!(validate_workspace(&dir.display().to_string()).is_ok());
        assert!(validate_workspace("").is_err());
        assert!(validate_workspace(&dir.join("nope").display().to_string()).is_err());
        fs::write(dir.join("file.txt"), "x").unwrap();
        assert!(validate_workspace(&dir.join("file.txt").display().to_string()).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn history_loads_only_from_the_workspace_log_folder() {
        let ws = temp();
        let home = ws.join(".lightning");
        let id = SessionId::from("20261010T120000Z-demo");
        let dir = SessionDir::create(&home, &id).unwrap();
        let sink = JsonlSink::create(dir.events_path()).unwrap();
        for e in fixtures::tool_failure() {
            lightning::obs::EventSink::emit(&sink, &e);
        }
        lightning::obs::EventSink::close(&sink);

        let w = ws.display().to_string();
        let list = list_sessions(&w).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, "20261010T120000Z-demo");

        let loaded = load_session(&w, "20261010T120000Z-demo").unwrap();
        assert_eq!(loaded.events.len(), fixtures::tool_failure().len());
        assert_eq!(session_events(&w, "20261010T120000Z-demo", 5).unwrap().len(), loaded.events.len() - 5);

        // Path traversal and bad ids are refused before touching the disk.
        for bad in ["../../etc/passwd", "..", "latest", "20261010T120000Z-demo/../x"] {
            assert!(load_session(&w, bad).is_err(), "{bad}");
            assert!(session_events(&w, bad, 0).is_err(), "{bad}");
        }
        assert!(list_sessions("definitely/not/here").is_err());
        fs::remove_dir_all(ws).ok();
    }
}
