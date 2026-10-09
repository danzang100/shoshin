//! Command line front end (spec 001, section 10): `run`, `inspect`, `sessions`.
//! All logic lives in the library; `main.rs` only calls [`main_entry`].
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::{env, io::IsTerminal};

use clap::{Args, Parser, Subcommand};

use crate::config::{self, Config, ConfigError, DEFAULT_HOME, LogLevel, Overrides};
use crate::llm::GeminiClient;
use crate::obs::console::ConsoleSink;
use crate::obs::inspect;
use crate::obs::{EventSink, JsonlSink, PayloadLog, Recorder, Redactor, SessionDir, SessionId, SessionStatus};
use crate::session::{RetryPolicy, Session, SessionConfig};
use crate::tools::dispatch::{Dispatcher, ToolLimits};
use crate::tools::gate::{ApprovalRequest, BoxFuture, FixedPrompter, Gate, PermissionMode, Prompter};
use crate::tools::workspace::Workspace;

#[derive(Debug, Parser)]
#[command(name = "lightning", version, about = "A small agentic coding harness")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the agent on a task.
    Run(RunArgs),
    /// Show what happened in a recorded session.
    Inspect(InspectArgs),
    /// List recorded sessions, newest first.
    Sessions(HomeArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// What you want done. Optional with --interactive.
    pub task: Option<String>,
    /// Keep prompting for follow-up messages in the same session.
    #[arg(short, long)]
    pub interactive: bool,
    #[command(flatten)]
    pub overrides: Overrides,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Session id, a unique id prefix, or `latest`.
    #[arg(default_value = "latest")]
    pub session: String,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
    #[command(flatten)]
    pub home: HomeArgs,
}

#[derive(Debug, Args)]
pub struct HomeArgs {
    /// Where logs live. Default: ./.lightning
    #[arg(long = "home", env = "LIGHTNING_HOME")]
    pub home: Option<PathBuf>,
}

impl HomeArgs {
    fn path(&self) -> PathBuf {
        self.home.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_HOME))
    }
}

impl RunArgs {
    /// A task is required unless the session is interactive.
    pub fn validate(&self) -> Result<(), String> {
        if self.task.as_deref().is_none_or(|t| t.trim().is_empty()) && !self.interactive {
            return Err("give a task, or use --interactive".to_string());
        }
        Ok(())
    }
}

/// Exit codes: 0 completed, 1 any other end state, 2 usage or configuration error.
pub fn exit_code(status: SessionStatus) -> u8 {
    if status == SessionStatus::Completed { 0 } else { 1 }
}

pub async fn main_entry() -> ExitCode {
    dotenv::dotenv().ok();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Run(args) => run_command(args).await,
        Command::Inspect(args) => inspect_command(args),
        Command::Sessions(args) => sessions_command(args),
    };
    ExitCode::from(code)
}

fn inspect_command(args: InspectArgs) -> u8 {
    let home = args.home.path();
    let result = inspect::find_session(&home, &args.session).and_then(|id| inspect::load_session(&home, &id));
    match result {
        Ok(session) => {
            if args.json {
                match serde_json::to_string_pretty(&inspect::to_json(&session)) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("error: {err}");
                        return 2;
                    }
                }
            } else {
                print!("{}", inspect::render_timeline(&session));
            }
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    }
}

fn sessions_command(args: HomeArgs) -> u8 {
    match inspect::list_sessions(&args.path()) {
        Ok(list) => {
            print!("{}", inspect::render_list(&list));
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            2
        }
    }
}

/// Asks on the terminal before a file change.
struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn confirm<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, bool> {
        let question = format!("Allow the assistant to {}?", request.summary);
        Box::pin(async move {
            // inquire blocks on the terminal, so keep it off the async worker threads.
            tokio::task::spawn_blocking(move || inquire::Confirm::new(&question).with_default(false).prompt().unwrap_or(false))
                .await
                .unwrap_or(false)
        })
    }
}

fn load_config(overrides: &Overrides) -> Result<Config, ConfigError> {
    let home = overrides.home.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_HOME));
    let file = config::load_file(&home)?;
    let cwd = env::current_dir().map_err(|e| ConfigError::Invalid(format!("cannot read current directory: {e}")))?;
    config::resolve(overrides, &file, env::var("GEMINI_API_KEY").ok(), &cwd)
}

async fn run_command(args: RunArgs) -> u8 {
    if let Err(msg) = args.validate() {
        eprintln!("error: {msg}");
        return 2;
    }
    let cfg = match load_config(&args.overrides) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("error: {err}");
            return 2;
        }
    };
    match run_session(&cfg, args.task, args.interactive).await {
        Ok(status) => exit_code(status),
        Err(msg) => {
            eprintln!("error: {msg}");
            2
        }
    }
}

async fn run_session(cfg: &Config, task: Option<String>, interactive: bool) -> Result<SessionStatus, String> {
    let id = SessionId::new();
    let dir = SessionDir::create(&cfg.home, &id).map_err(|e| format!("cannot create log folder: {e}"))?;
    let redactor = Redactor::new([cfg.api_key.clone()]);

    let jsonl = JsonlSink::create(dir.events_path()).map_err(|e| format!("cannot open event log: {e}"))?;
    let sinks: Vec<Box<dyn EventSink>> = vec![Box::new(jsonl), Box::new(ConsoleSink::new())];
    let rec = Recorder::new(id.clone(), redactor.clone(), sinks);

    let debug = cfg.log_level == LogLevel::Debug;
    let llm = GeminiClient::new(cfg.api_key.clone(), cfg.model.clone()).capture_bodies(debug);

    let workspace = Workspace::new(&cfg.workspace).map_err(|e| e.to_string())?;
    let prompter: Arc<dyn Prompter> = if std::io::stdin().is_terminal() {
        Arc::new(TerminalPrompter)
    } else {
        if cfg.permission_mode == PermissionMode::Ask {
            eprintln!("note: no terminal available, so file changes are denied (use --permission-mode allow to permit them)");
        }
        Arc::new(FixedPrompter(false))
    };
    let gate = Gate::new(cfg.permission_mode, prompter);
    let limits = ToolLimits { timeout: cfg.tool_timeout, output_cap_bytes: cfg.tool_output_cap_bytes };
    let dispatcher = Dispatcher::new(workspace, limits, gate);

    let mut scfg = SessionConfig::new(cfg.provider.clone(), cfg.model.clone());
    scfg.max_steps = cfg.max_steps;
    scfg.retry = RetryPolicy::default();
    scfg.snapshot = cfg.snapshot();

    let mut session = Session::new(llm, dispatcher, rec, scfg).with_dir(dir.clone());
    if debug {
        let payloads = PayloadLog::create(dir.payloads_path(), redactor).map_err(|e| format!("cannot open payload log: {e}"))?;
        session = session.with_payloads(payloads);
    }

    let token = session.cancel_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
        }
    });

    let mut next = task.filter(|t| !t.trim().is_empty());
    let mut status = SessionStatus::Completed;
    loop {
        let text = match next.take() {
            Some(text) => text,
            None if interactive => match inquire::Text::new("you>").prompt() {
                Ok(t) if !t.trim().is_empty() && !matches!(t.trim(), "exit" | "quit") => t,
                _ => break,
            },
            None => break,
        };
        status = session.run_turn(&text).await;
        if status != SessionStatus::Completed || !interactive {
            break;
        }
    }
    session.end(status);

    if let Some(err) = session.last_error().filter(|_| status == SessionStatus::LlmError) {
        eprintln!("error: the model could not be reached: {err}");
    }
    let name = inspect::status_name(status);
    eprintln!("\nSession {id} ended: {name}. Details: lightning inspect {id}");
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("lightning").chain(args.iter().copied()))
    }

    #[test]
    fn run_requires_a_task_unless_interactive() {
        let Command::Run(a) = parse(&["run"]).unwrap().command else { panic!() };
        assert!(a.validate().is_err());
        let Command::Run(a) = parse(&["run", "  "]).unwrap().command else { panic!() };
        assert!(a.validate().is_err());
        let Command::Run(a) = parse(&["run", "-i"]).unwrap().command else { panic!() };
        assert!(a.validate().is_ok());
        let Command::Run(a) = parse(&["run", "do it"]).unwrap().command else { panic!() };
        assert!(a.validate().is_ok());
        assert_eq!(a.task.as_deref(), Some("do it"));
    }

    #[test]
    fn run_flags_map_to_overrides() {
        let Command::Run(a) = parse(&["run", "t", "--model", "m", "--max-steps", "4", "--permission-mode", "allow", "--log-level", "debug"]).unwrap().command else {
            panic!()
        };
        assert_eq!(a.overrides.model.as_deref(), Some("m"));
        assert_eq!(a.overrides.max_steps, Some(4));
        assert!(a.overrides.permission_mode.is_some());
        assert_eq!(a.overrides.log_level, Some(LogLevel::Debug));
    }

    #[test]
    fn bad_flags_are_rejected() {
        assert!(parse(&["run", "t", "--permission-mode", "yolo"]).is_err());
        assert!(parse(&["run", "t", "--max-steps", "many"]).is_err());
        assert!(parse(&["frobnicate"]).is_err());
    }

    #[test]
    fn inspect_defaults_to_latest_and_sessions_parses() {
        let Command::Inspect(a) = parse(&["inspect"]).unwrap().command else { panic!() };
        assert_eq!(a.session, "latest");
        assert!(!a.json);
        let Command::Inspect(a) = parse(&["inspect", "2026", "--json"]).unwrap().command else { panic!() };
        assert_eq!((a.session.as_str(), a.json), ("2026", true));
        assert!(matches!(parse(&["sessions"]).unwrap().command, Command::Sessions(_)));
    }

    #[test]
    fn exit_codes_follow_the_spec() {
        assert_eq!(exit_code(SessionStatus::Completed), 0);
        assert_eq!(exit_code(SessionStatus::LlmError), 1);
        assert_eq!(exit_code(SessionStatus::MaxStepsReached), 1);
    }
}
