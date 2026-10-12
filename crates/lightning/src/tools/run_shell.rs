//! `run_shell`: runs one command in the workspace folder. This tool is outside the file
//! sandbox (a command can touch anything the user can), so it always passes the permission
//! gate and runs with secret-looking environment variables removed. Unlike the other tools it
//! is async, because a running process must be killed on timeout or cancel.
use std::process::Stdio;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::cancel::CancelToken;
use crate::tools::ToolOutput;
use crate::tools::error::ToolError;
use crate::tools::workspace::Workspace;

/// Output kept per stream; the rest is read and discarded so the process never blocks.
const MAX_STREAM_BYTES: usize = 256 * 1024;
const MAX_TIMEOUT_SECS: u32 = 300;
/// How long to wait for output readers after the process is gone (grandchildren can hold pipes).
const READER_GRACE: Duration = Duration::from_secs(2);

/// Runs a command in the workspace folder and returns its output. On Windows the command is PowerShell, elsewhere `sh`. A nonzero exit code is reported as an error together with the output. Use it for builds, tests and git; prefer read_file, list_dir and grep for looking at files. This changes the machine state and always needs the user's approval.
#[derive(Deserialize, Debug, JsonSchema)]
pub struct RunShellArgs {
    /// The command line to run.
    pub command: String,
    /// Seconds to wait before the command is killed. 0 (the default) uses the standard limit; at most 300.
    #[serde(default)]
    pub timeout_secs: u32,
}

/// Names that look like secrets are not passed to the child process.
pub fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"].iter().any(|word| upper.contains(word))
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("powershell.exe");
        // UTF-8 output regardless of the console code page.
        let script = format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };
    for (name, _) in std::env::vars_os() {
        if is_secret_env_name(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd
}

/// Reads a stream fully, keeping only the first `MAX_STREAM_BYTES`.
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STREAM_BYTES.saturating_sub(kept.len());
                if room > 0 {
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                if n > room {
                    truncated = true;
                }
            }
        }
    }
    (kept, truncated)
}

pub async fn run_shell(
    ws: &Workspace,
    args: RunShellArgs,
    default_timeout: Duration,
    cancel: &CancelToken,
) -> Result<ToolOutput, ToolError> {
    if args.command.trim().is_empty() {
        return Err(ToolError::InvalidArguments { message: "command is empty".into() });
    }
    let timeout = if args.timeout_secs == 0 {
        default_timeout
    } else {
        Duration::from_secs(args.timeout_secs.min(MAX_TIMEOUT_SECS) as u64)
    };

    let mut cmd = shell_command(&args.command);
    cmd.current_dir(ws.display_root()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| ToolError::Io { message: format!("could not start the shell: {e}") })?;

    let stdout = child.stdout.take().ok_or_else(|| ToolError::Io { message: "no stdout pipe".into() })?;
    let stderr = child.stderr.take().ok_or_else(|| ToolError::Io { message: "no stderr pipe".into() })?;
    let out_task = tokio::spawn(read_capped(stdout));
    let err_task = tokio::spawn(read_capped(stderr));

    let started = Instant::now();
    enum End {
        Exited(std::io::Result<std::process::ExitStatus>),
        TimedOut,
        Cancelled,
    }
    let end = tokio::select! {
        biased;
        _ = cancel.cancelled() => End::Cancelled,
        r = tokio::time::timeout(timeout, child.wait()) => match r {
            Ok(status) => End::Exited(status),
            Err(_) => End::TimedOut,
        },
    };

    if matches!(end, End::TimedOut | End::Cancelled) {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(READER_GRACE, child.wait()).await;
    }
    let (stdout, out_cut) = tokio::time::timeout(READER_GRACE, out_task).await.ok().and_then(Result::ok).unwrap_or_default();
    let (stderr, err_cut) = tokio::time::timeout(READER_GRACE, err_task).await.ok().and_then(Result::ok).unwrap_or_default();
    let out = String::from_utf8_lossy(&stdout).into_owned();
    let err = String::from_utf8_lossy(&stderr).into_owned();
    let output = combine(&out, &err, out_cut || err_cut);

    match end {
        End::Cancelled => Err(ToolError::Cancelled),
        End::TimedOut => Err(ToolError::Timeout { seconds: timeout.as_secs(), output }),
        End::Exited(Err(e)) => Err(ToolError::Io { message: format!("waiting for the command failed: {e}") }),
        End::Exited(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            if code == 0 {
                Ok(ToolOutput {
                    content: if output.is_empty() { "(no output)".to_string() } else { output },
                    metadata: serde_json::json!({ "exit_code": 0, "duration_ms": started.elapsed().as_millis() as u64 }),
                })
            } else {
                Err(ToolError::ExecutionFailed { exit_code: code, output })
            }
        }
    }
}

/// stdout, then stderr under a marker when there is any.
fn combine(stdout: &str, stderr: &str, truncated: bool) -> String {
    let mut text = stdout.trim_end().to_string();
    if !stderr.trim().is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[stderr]\n");
        text.push_str(stderr.trim_end());
    }
    if truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;
    use std::fs;

    fn args(command: &str) -> RunShellArgs {
        RunShellArgs { command: command.into(), timeout_secs: 0 }
    }

    async fn run(ws: &Workspace, command: &str) -> Result<ToolOutput, ToolError> {
        run_shell(ws, args(command), Duration::from_secs(30), &CancelToken::default()).await
    }

    #[test]
    fn secret_looking_names_are_recognised() {
        for name in ["GEMINI_API_KEY", "github_token", "AWS_SECRET_ACCESS_KEY", "DB_PASSWORD", "MY_CREDENTIALS"] {
            assert!(is_secret_env_name(name), "{name}");
        }
        for name in ["PATH", "HOME", "USERPROFILE", "TEMP", "SystemRoot", "LANG"] {
            assert!(!is_secret_env_name(name), "{name}");
        }
    }

    #[test]
    fn output_is_combined_with_a_stderr_marker() {
        assert_eq!(combine("out\n", "", false), "out");
        assert_eq!(combine("out", "warn\n", false), "out\n\n[stderr]\nwarn");
        assert_eq!(combine("", "only err", true), "[stderr]\nonly err\n[output truncated]");
        assert_eq!(combine("", "", false), "");
    }

    #[tokio::test]
    async fn runs_in_the_workspace_and_captures_output() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join("marker.txt"), "x").unwrap();
        let listing = if cfg!(windows) { "Get-ChildItem -Name" } else { "ls" };
        let out = run(&ws, listing).await.unwrap();
        assert!(out.content.contains("marker.txt"), "{}", out.content);
        assert_eq!(out.metadata["exit_code"], 0);

        let hello = run(&ws, "echo hello").await.unwrap();
        assert_eq!(hello.content, "hello");
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error_with_the_output_attached() {
        let (ws, dir) = temp_workspace();
        let script = if cfg!(windows) { "Write-Output before; exit 3" } else { "echo before; exit 3" };
        match run(&ws, script).await {
            Err(ToolError::ExecutionFailed { exit_code, output }) => {
                assert_eq!(exit_code, 3);
                assert!(output.contains("before"));
            }
            other => panic!("{other:?}"),
        }
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn stderr_is_reported_but_exit_zero_stays_ok() {
        let (ws, dir) = temp_workspace();
        let script = if cfg!(windows) { "[Console]::Error.WriteLine('oops'); echo fine" } else { "echo oops 1>&2; echo fine" };
        let out = run(&ws, script).await.unwrap();
        assert!(out.content.contains("fine") && out.content.contains("[stderr]") && out.content.contains("oops"), "{}", out.content);
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_slow_command_is_killed_at_the_timeout() {
        let (ws, dir) = temp_workspace();
        let started = Instant::now();
        let result = run_shell(&ws, RunShellArgs { command: "sleep 20".into(), timeout_secs: 1 }, Duration::from_secs(30), &CancelToken::default()).await;
        assert!(matches!(result, Err(ToolError::Timeout { seconds: 1, .. })), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn cancelling_kills_the_command_promptly() {
        let (ws, dir) = temp_workspace();
        let cancel = CancelToken::default();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            trigger.cancel();
        });
        let started = Instant::now();
        let result = run_shell(&ws, args("sleep 20"), Duration::from_secs(30), &cancel).await;
        assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn huge_output_is_capped_and_empty_commands_are_rejected() {
        let (ws, dir) = temp_workspace();
        let big = if cfg!(windows) { "1..60000 | ForEach-Object { 'line of padding text to make this large ' + $_ }" } else { "yes 'line of padding text to make this large' | head -n 60000" };
        let out = run(&ws, big).await.unwrap();
        assert!(out.content.ends_with("[output truncated]"));
        assert!(out.content.len() <= MAX_STREAM_BYTES + 64);
        assert!(matches!(run(&ws, "   ").await, Err(ToolError::InvalidArguments { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn the_child_does_not_see_secret_looking_variables() {
        // PATH is passed through; a secret-looking name that exists in this process is not.
        let (ws, dir) = temp_workspace();
        let name = std::env::vars().map(|(k, _)| k).find(|k| is_secret_env_name(k));
        let Some(name) = name else {
            // Nothing secret-looking in this environment; the name filter is covered above.
            fs::remove_dir_all(dir).ok();
            return;
        };
        let script = if cfg!(windows) { format!("if (Test-Path Env:{name}) {{ 'visible' }} else {{ 'hidden' }}") } else { format!("if [ -n \"${{{name}+x}}\" ]; then echo visible; else echo hidden; fi") };
        let out = run(&ws, &script).await.unwrap();
        assert_eq!(out.content, "hidden");
        fs::remove_dir_all(dir).ok();
    }
}
