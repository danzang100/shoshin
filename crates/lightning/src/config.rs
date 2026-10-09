//! Configuration (spec 001, section 10). Precedence: flags > env > config file > defaults.
//! clap already merges flags and env (`#[arg(env = ...)]`), so [`resolve`] only has to
//! layer the file and the defaults underneath.
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::llm::gemini::DEFAULT_MODEL;
use crate::tools::gate::PermissionMode;

pub const DEFAULT_HOME: &str = ".lightning";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    /// Also records full LLM request/response bodies in `llm_payloads.jsonl`.
    Debug,
}

/// Values the user may set on the command line or in the environment.
/// Every field is optional so that unset values fall through to the file and defaults.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct Overrides {
    /// Where logs and config live. Default: ./.lightning
    #[arg(long, env = "LIGHTNING_HOME")]
    pub home: Option<PathBuf>,
    /// Folder the tools may touch. Default: current directory.
    #[arg(long, env = "LIGHTNING_WORKSPACE")]
    pub workspace: Option<PathBuf>,
    /// LLM provider. Only `gemini` is supported so far.
    #[arg(long, env = "LIGHTNING_PROVIDER")]
    pub provider: Option<String>,
    #[arg(long, env = "LIGHTNING_MODEL")]
    pub model: Option<String>,
    /// LLM calls allowed per request.
    #[arg(long, env = "LIGHTNING_MAX_STEPS")]
    pub max_steps: Option<u32>,
    #[arg(long, env = "LIGHTNING_TOOL_TIMEOUT_SECS")]
    pub tool_timeout_secs: Option<u64>,
    #[arg(long, env = "LIGHTNING_TOOL_OUTPUT_CAP_BYTES")]
    pub tool_output_cap_bytes: Option<usize>,
    /// ask: confirm each file change; allow: never ask; deny: refuse file changes.
    #[arg(long, value_enum)]
    pub permission_mode: Option<PermissionModeArg>,
    #[arg(long, value_enum, env = "LIGHTNING_LOG_LEVEL")]
    pub log_level: Option<LogLevel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PermissionModeArg {
    Ask,
    Allow,
    Deny,
}

impl From<PermissionModeArg> for PermissionMode {
    fn from(arg: PermissionModeArg) -> Self {
        match arg {
            PermissionModeArg::Ask => PermissionMode::Ask,
            PermissionModeArg::Allow => PermissionMode::Allow,
            PermissionModeArg::Deny => PermissionMode::Deny,
        }
    }
}

/// `<home>/config.toml`. Unknown keys are an error so typos do not pass silently.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub workspace: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_steps: Option<u32>,
    pub tool_timeout_secs: Option<u64>,
    pub tool_output_cap_bytes: Option<usize>,
    pub permission_mode: Option<PermissionModeArg>,
    pub log_level: Option<LogLevel>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub provider: String,
    pub model: String,
    pub max_steps: u32,
    pub tool_timeout: Duration,
    pub tool_output_cap_bytes: usize,
    pub permission_mode: PermissionMode,
    pub log_level: LogLevel,
    pub api_key: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    MissingApiKey,
    UnknownProvider(String),
    WorkspaceNotADirectory(PathBuf),
    Invalid(String),
    File { path: PathBuf, message: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingApiKey => {
                write!(f, "GEMINI_API_KEY is not set. Put it in a .env file (current or parent folder) or the environment.")
            }
            ConfigError::UnknownProvider(p) => write!(f, "unknown provider '{p}'; supported: gemini"),
            ConfigError::WorkspaceNotADirectory(p) => write!(f, "workspace {} is not a directory", p.display()),
            ConfigError::Invalid(msg) => write!(f, "invalid setting: {msg}"),
            ConfigError::File { path, message } => write!(f, "cannot read config file {}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Reads `<home>/config.toml` if it exists.
pub fn load_file(home: &Path) -> Result<FileConfig, ConfigError> {
    let path = home.join("config.toml");
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::File { path: path.clone(), message: e.to_string() })?;
    toml::from_str(&text).map_err(|e| ConfigError::File { path, message: e.to_string() })
}

/// Layers overrides over the file over the defaults and validates the result.
/// `api_key` comes from the environment and is never written anywhere.
pub fn resolve(cli: &Overrides, file: &FileConfig, api_key: Option<String>, cwd: &Path) -> Result<Config, ConfigError> {
    let home = cli.home.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_HOME));
    let provider = cli.provider.clone().or_else(|| file.provider.clone()).unwrap_or_else(|| "gemini".to_string());
    if provider != "gemini" {
        return Err(ConfigError::UnknownProvider(provider));
    }
    let api_key = api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).ok_or(ConfigError::MissingApiKey)?;

    let workspace = cli.workspace.clone().or_else(|| file.workspace.clone()).unwrap_or_else(|| cwd.to_path_buf());
    if !workspace.is_dir() {
        return Err(ConfigError::WorkspaceNotADirectory(workspace));
    }

    let max_steps = cli.max_steps.or(file.max_steps).unwrap_or(25);
    if max_steps == 0 {
        return Err(ConfigError::Invalid("max_steps must be at least 1".into()));
    }
    let tool_timeout_secs = cli.tool_timeout_secs.or(file.tool_timeout_secs).unwrap_or(30);
    if tool_timeout_secs == 0 {
        return Err(ConfigError::Invalid("tool_timeout_secs must be at least 1".into()));
    }
    let cap = cli.tool_output_cap_bytes.or(file.tool_output_cap_bytes).unwrap_or(32 * 1024);
    if cap < 256 {
        return Err(ConfigError::Invalid("tool_output_cap_bytes must be at least 256".into()));
    }

    Ok(Config {
        home,
        workspace,
        provider,
        model: cli.model.clone().or_else(|| file.model.clone()).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        max_steps,
        tool_timeout: Duration::from_secs(tool_timeout_secs),
        tool_output_cap_bytes: cap,
        permission_mode: cli.permission_mode.or(file.permission_mode).map(Into::into).unwrap_or_default(),
        log_level: cli.log_level.or(file.log_level).unwrap_or(LogLevel::Info),
        api_key,
    })
}

impl Config {
    /// Secret-free snapshot for the `session_started` event.
    pub fn snapshot(&self) -> Value {
        json!({
            "home": self.home,
            "workspace": self.workspace,
            "provider": self.provider,
            "model": self.model,
            "max_steps": self.max_steps,
            "tool_timeout_secs": self.tool_timeout.as_secs(),
            "tool_output_cap_bytes": self.tool_output_cap_bytes,
            "permission_mode": self.permission_mode,
            "log_level": format!("{:?}", self.log_level).to_lowercase(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> PathBuf {
        std::env::temp_dir()
    }

    fn key() -> Option<String> {
        Some("test-key-123456".into())
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let c = resolve(&Overrides::default(), &FileConfig::default(), key(), &cwd()).unwrap();
        assert_eq!(c.provider, "gemini");
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.max_steps, 25);
        assert_eq!(c.tool_timeout, Duration::from_secs(30));
        assert_eq!(c.tool_output_cap_bytes, 32768);
        assert_eq!(c.permission_mode, PermissionMode::Ask);
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.home, PathBuf::from(".lightning"));
        assert_eq!(c.workspace, cwd());
    }

    #[test]
    fn flags_beat_file_beats_defaults() {
        let file = FileConfig { model: Some("from-file".into()), max_steps: Some(7), permission_mode: Some(PermissionModeArg::Allow), ..Default::default() };
        let cli = Overrides { model: Some("from-flag".into()), ..Default::default() };
        let c = resolve(&cli, &file, key(), &cwd()).unwrap();
        assert_eq!(c.model, "from-flag");
        assert_eq!(c.max_steps, 7);
        assert_eq!(c.permission_mode, PermissionMode::Allow);
    }

    #[test]
    fn validation_errors_are_specific() {
        let ok = Overrides::default();
        assert_eq!(resolve(&ok, &FileConfig::default(), None, &cwd()).unwrap_err(), ConfigError::MissingApiKey);
        assert_eq!(resolve(&ok, &FileConfig::default(), Some("  ".into()), &cwd()).unwrap_err(), ConfigError::MissingApiKey);

        let bad_provider = Overrides { provider: Some("openai".into()), ..Default::default() };
        assert_eq!(resolve(&bad_provider, &FileConfig::default(), key(), &cwd()).unwrap_err(), ConfigError::UnknownProvider("openai".into()));

        let bad_ws = Overrides { workspace: Some(cwd().join("definitely-not-here-xyz")), ..Default::default() };
        assert!(matches!(resolve(&bad_ws, &FileConfig::default(), key(), &cwd()), Err(ConfigError::WorkspaceNotADirectory(_))));

        let zero = Overrides { max_steps: Some(0), ..Default::default() };
        assert!(matches!(resolve(&zero, &FileConfig::default(), key(), &cwd()), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn file_parses_and_rejects_unknown_keys() {
        let f: FileConfig = toml::from_str("model = \"m\"\nmax_steps = 3\npermission_mode = \"deny\"\nlog_level = \"debug\"").unwrap();
        assert_eq!(f.model.as_deref(), Some("m"));
        assert_eq!(f.permission_mode, Some(PermissionModeArg::Deny));
        assert_eq!(f.log_level, Some(LogLevel::Debug));
        assert!(toml::from_str::<FileConfig>("modle = \"typo\"").is_err());
    }

    #[test]
    fn load_file_missing_is_default_and_broken_is_an_error() {
        let home = std::env::temp_dir().join(format!("lightning-test-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(load_file(&home).unwrap().model.is_none());
        std::fs::write(home.join("config.toml"), "model = ").unwrap();
        assert!(matches!(load_file(&home), Err(ConfigError::File { .. })));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn snapshot_never_contains_the_key() {
        let c = resolve(&Overrides::default(), &FileConfig::default(), key(), &cwd()).unwrap();
        assert!(!c.snapshot().to_string().contains("test-key-123456"));
    }
}
