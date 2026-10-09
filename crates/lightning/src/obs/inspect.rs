//! Reads session logs back and renders them (spec 001, section 9.7). Everything here
//! works from the files on disk only: no network, no LGTM.
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use chrono::DateTime;
use serde_json::{Value, json};

use super::event::{Event, EventKind, Outcome, preview};
use super::jsonl::{ReadReport, read_events};
use super::summary::Summary;
use super::{SessionDir, SessionStatus};

/// One session as loaded from disk.
pub struct LoadedSession {
    pub id: String,
    pub summary: Summary,
    pub events: Vec<Event>,
    pub report: ReadReport,
}

/// Session ids under `<home>/sessions`, newest first (ids sort by start time).
pub fn session_ids(home: &Path) -> io::Result<Vec<String>> {
    let root = SessionDir::sessions_root(home);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.reverse();
    Ok(ids)
}

/// Resolves `latest`, a full id, or a unique id prefix.
pub fn find_session(home: &Path, query: &str) -> io::Result<String> {
    let ids = session_ids(home)?;
    let not_found = || io::Error::new(io::ErrorKind::NotFound, format!("no session matches '{query}'"));
    if query == "latest" {
        return ids.into_iter().next().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no sessions recorded yet"));
    }
    if ids.iter().any(|id| id == query) {
        return Ok(query.to_string());
    }
    let mut matches = ids.into_iter().filter(|id| id.starts_with(query));
    match (matches.next(), matches.next()) {
        (Some(id), None) => Ok(id),
        (Some(_), Some(_)) => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("'{query}' matches several sessions"))),
        _ => Err(not_found()),
    }
}

/// Loads a session. `summary.json` is used when present and readable; otherwise (a
/// crash before the end) it is rebuilt from the events.
pub fn load_session(home: &Path, id: &str) -> io::Result<LoadedSession> {
    let dir = SessionDir::open(home, id)?;
    let (events, report) = read_events(&dir.events_path())?;
    let summary = Summary::read(&dir.summary_path()).unwrap_or_else(|_| Summary::from_events(id, &events));
    Ok(LoadedSession { id: id.to_string(), summary, events, report })
}

/// Summaries of every readable session, newest first. Unreadable ones are skipped.
pub fn list_sessions(home: &Path) -> io::Result<Vec<Summary>> {
    let mut out = Vec::new();
    for id in session_ids(home)? {
        if let Ok(session) = load_session(home, &id) {
            out.push(session.summary);
        }
    }
    Ok(out)
}

pub fn render_list(summaries: &[Summary]) -> String {
    if summaries.is_empty() {
        return "No sessions recorded yet.\n".to_string();
    }
    let mut out = String::new();
    for s in summaries {
        let status = s.status.map(status_name).unwrap_or_else(|| "unfinished".to_string());
        let duration = s.duration_ms.map(format_ms).unwrap_or_else(|| "-".to_string());
        let task = preview(s.task.as_deref().unwrap_or("-").lines().next().unwrap_or("-"), 50);
        out.push_str(&format!(
            "{}  {:<17} {:>8}  {:>2} steps  {:>2} tools ({} err)  {}\n",
            s.session_id, status, duration, s.steps, s.tool_calls, s.tool_errors, task
        ));
    }
    out
}

pub fn render_timeline(session: &LoadedSession) -> String {
    let s = &session.summary;
    let mut out = String::new();

    let status = s.status.map(status_name).unwrap_or_else(|| "unfinished".to_string());
    out.push_str(&format!("Session {}  [{}]\n", session.id, status));
    out.push_str(&format!(
        "  model: {}/{}   workspace: {}\n",
        s.provider.as_deref().unwrap_or("?"),
        s.model.as_deref().unwrap_or("?"),
        s.workspace.as_deref().unwrap_or("?")
    ));
    out.push_str(&format!(
        "  duration: {}   steps: {}   llm calls: {}   tool calls: {} ({} errors)   warnings: {}\n",
        s.duration_ms.map(format_ms).unwrap_or_else(|| "-".into()),
        s.steps,
        s.llm_calls,
        s.tool_calls,
        s.tool_errors,
        s.warnings
    ));
    out.push_str(&format!(
        "  tokens: in {}  out {}  thinking {}\n",
        count(s.usage.input_tokens),
        count(s.usage.output_tokens),
        count(s.usage.thinking_tokens)
    ));
    if let Some(task) = &s.task {
        out.push_str(&format!("  task: {}\n", preview(task, 200)));
    }
    out.push('\n');

    let step_durations: HashMap<u32, u64> = session
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::StepFinished { step, duration_ms } => Some((*step, *duration_ms)),
            _ => None,
        })
        .collect();
    let mut tool_args: HashMap<&str, &Value> = HashMap::new();
    for e in &session.events {
        if let EventKind::ToolStarted { tool_use_id, arguments, .. } = &e.kind {
            tool_args.insert(tool_use_id.as_str(), arguments);
        }
    }
    let origin = session.events.first().and_then(|e| parse_ts(&e.ts));

    for e in &session.events {
        let at = match (origin, parse_ts(&e.ts)) {
            (Some(o), Some(t)) => format!("+{:>7.3}s", (t - o).num_milliseconds() as f64 / 1000.0),
            _ => "         ".to_string(),
        };
        let line = match &e.kind {
            EventKind::SessionStarted { .. } | EventKind::LlmRequest { .. } | EventKind::ToolStarted { .. } => None,
            EventKind::StepFinished { .. } | EventKind::SessionEnded { .. } => None,
            EventKind::UserMessage { text } => Some(format!("user: {}", preview(text, 160))),
            EventKind::StepStarted { step } => Some(match step_durations.get(step) {
                Some(ms) => format!("step {step} ({})", format_ms(*ms)),
                None => format!("step {step} (did not finish)"),
            }),
            EventKind::LlmAttempt { attempt, outcome: Outcome::Error, error, backoff_ms } => Some(format!(
                "  llm attempt {attempt} failed: {}{}",
                error.as_deref().unwrap_or("unknown error"),
                backoff_ms.map(|b| format!(" (retrying in {b} ms)")).unwrap_or_default()
            )),
            EventKind::LlmAttempt { .. } => None,
            EventKind::LlmResponse { finish, usage, latency_ms, tool_use_count } => Some(format!(
                "  llm: {} in {}  in={} out={}{}",
                finish_name(finish),
                format_ms(*latency_ms),
                count(usage.input_tokens),
                count(usage.output_tokens),
                if *tool_use_count > 0 { format!("  ({tool_use_count} tool calls)") } else { String::new() }
            )),
            EventKind::AssistantMessage { text: Some(text), .. } => Some(format!("  assistant: {}", preview(&text.replace('\n', " "), 160))),
            EventKind::AssistantMessage { text: None, .. } => None,
            EventKind::GateDecision { allowed, reason, .. } => {
                if !allowed {
                    Some(format!("  gate: DENIED ({reason})"))
                } else if reason != "read-only tool" {
                    Some(format!("  gate: allowed ({reason})"))
                } else {
                    None
                }
            }
            EventKind::ToolFinished { tool_use_id, name, outcome, error_class, preview: text, duration_ms, truncated, output_bytes, .. } => {
                let args = tool_args.get(tool_use_id.as_str()).map(|v| preview(&v.to_string(), 80)).unwrap_or_default();
                Some(match outcome {
                    Outcome::Ok => format!(
                        "  tool {name} {args} -> ok, {output_bytes} bytes{} ({})",
                        if *truncated { ", truncated" } else { "" },
                        format_ms(*duration_ms)
                    ),
                    Outcome::Error => format!(
                        "  tool {name} {args} -> ERROR {}: {} ({})",
                        error_class.as_deref().unwrap_or("?"),
                        preview(&text.replace('\n', " "), 100),
                        format_ms(*duration_ms)
                    ),
                })
            }
            EventKind::LoopWarning { kind, detail } => Some(format!("  ! {kind}: {detail}")),
        };
        if let Some(line) = line {
            out.push_str(&format!("{at}  {line}\n"));
        }
    }

    out.push('\n');
    match s.status {
        Some(status) => out.push_str(&format!("Ended: {}\n", status_name(status))),
        None => out.push_str("Session did not end cleanly (no session_ended event).\n"),
    }
    if session.report.truncated_last_line {
        out.push_str("Note: the log ends with a partial line (likely a crash mid-write); it was ignored.\n");
    }
    if !session.report.bad_lines.is_empty() {
        out.push_str(&format!("Note: unreadable log lines skipped: {:?}\n", session.report.bad_lines));
    }
    out
}

/// Machine-readable form of `inspect`.
pub fn to_json(session: &LoadedSession) -> Value {
    json!({
        "summary": session.summary,
        "events": session.events,
        "log_issues": {
            "truncated_last_line": session.report.truncated_last_line,
            "bad_lines": session.report.bad_lines,
        },
    })
}

pub fn status_name(status: SessionStatus) -> String {
    serde_json::to_value(status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| format!("{status:?}"))
}

fn finish_name(finish: &crate::llm::FinishReason) -> String {
    match serde_json::to_value(finish) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_string(),
    }
}

fn parse_ts(ts: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(ts).ok()
}

fn format_ms(ms: u64) -> String {
    if ms < 1000 { format!("{ms} ms") } else { format!("{:.2} s", ms as f64 / 1000.0) }
}

fn count(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "-".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FinishReason, Usage};
    use crate::obs::event::SCHEMA_VERSION;
    use crate::obs::jsonl::JsonlSink;
    use crate::obs::{EventSink, ToolUseRef};

    fn ev(seq: u64, ms: u32, kind: EventKind) -> Event {
        Event {
            v: SCHEMA_VERSION,
            ts: format!("2026-10-10T12:00:{:02}.{:03}Z", ms / 1000, ms % 1000),
            session_id: "20261010T120000Z-aaaa".into(),
            seq,
            kind,
            span_id: None,
            parent_span_id: None,
        }
    }

    fn fixture() -> Vec<Event> {
        let usage = Usage { input_tokens: Some(353), output_tokens: Some(24), thinking_tokens: Some(0) };
        vec![
            ev(1, 0, EventKind::SessionStarted {
                workspace: "/w".into(),
                provider: "gemini".into(),
                model: "gemini-3.5-flash-lite".into(),
                prompt_version: "1".into(),
                harness_version: "0.1.0".into(),
                config: Value::Null,
            }),
            ev(2, 1, EventKind::UserMessage { text: "read missing.txt".into() }),
            ev(3, 2, EventKind::StepStarted { step: 1 }),
            ev(4, 700, EventKind::LlmResponse { finish: FinishReason::ToolUse, usage, latency_ms: 698, tool_use_count: 1 }),
            ev(5, 701, EventKind::AssistantMessage { text: None, tool_uses: vec![ToolUseRef { id: "c1".into(), name: "read_file".into() }] }),
            ev(6, 702, EventKind::ToolStarted { tool_use_id: "c1".into(), name: "read_file".into(), arguments: json!({ "file_path": "missing.txt" }) }),
            ev(7, 703, EventKind::GateDecision { tool_use_id: "c1".into(), allowed: true, reason: "read-only tool".into() }),
            ev(8, 704, EventKind::ToolFinished {
                tool_use_id: "c1".into(),
                name: "read_file".into(),
                outcome: Outcome::Error,
                error_class: Some("not_found".into()),
                message: Some("Not found: missing.txt.".into()),
                preview: "Not found: missing.txt.".into(),
                output_bytes: 23,
                truncated: false,
                duration_ms: 2,
            }),
            ev(9, 705, EventKind::StepFinished { step: 1, duration_ms: 703 }),
            ev(10, 706, EventKind::StepStarted { step: 2 }),
            ev(11, 1500, EventKind::LlmResponse { finish: FinishReason::Stop, usage, latency_ms: 794, tool_use_count: 0 }),
            ev(12, 1501, EventKind::AssistantMessage { text: Some("The file does not exist.".into()), tool_uses: vec![] }),
            ev(13, 1502, EventKind::StepFinished { step: 2, duration_ms: 796 }),
            ev(14, 1503, EventKind::SessionEnded {
                status: SessionStatus::Completed,
                steps: 2,
                tool_calls: 1,
                tool_errors: 1,
                usage,
                duration_ms: 1503,
            }),
        ]
    }

    fn loaded(events: Vec<Event>, report: ReadReport) -> LoadedSession {
        let summary = Summary::from_events("20261010T120000Z-aaaa", &events);
        LoadedSession { id: "20261010T120000Z-aaaa".into(), summary, events, report }
    }

    #[test]
    fn timeline_renders_the_known_fixture() {
        let text = render_timeline(&loaded(fixture(), ReadReport::default()));
        let expected = "\
Session 20261010T120000Z-aaaa  [completed]
  model: gemini/gemini-3.5-flash-lite   workspace: /w
  duration: 1.50 s   steps: 2   llm calls: 2   tool calls: 1 (1 errors)   warnings: 0
  tokens: in 706  out 48  thinking 0
  task: read missing.txt

+  0.001s  user: read missing.txt
+  0.002s  step 1 (703 ms)
+  0.700s    llm: tool_use in 698 ms  in=353 out=24  (1 tool calls)
+  0.704s    tool read_file {\"file_path\":\"missing.txt\"} -> ERROR not_found: Not found: missing.txt. (2 ms)
+  0.706s  step 2 (796 ms)
+  1.500s    llm: stop in 794 ms  in=353 out=24
+  1.501s    assistant: The file does not exist.

Ended: completed
";
        assert_eq!(text, expected);
    }

    #[test]
    fn flags_unfinished_sessions_and_partial_logs() {
        let mut events = fixture();
        events.truncate(8);
        let report = ReadReport { truncated_last_line: true, bad_lines: vec![] };
        let text = render_timeline(&loaded(events, report));
        assert!(text.contains("[unfinished]"));
        assert!(text.contains("step 1 (did not finish)"));
        assert!(text.contains("did not end cleanly"));
        assert!(text.contains("partial line"));
    }

    #[test]
    fn denied_gate_and_warnings_are_shown() {
        let mut events = fixture();
        events.insert(7, ev(99, 703, EventKind::GateDecision { tool_use_id: "c1".into(), allowed: false, reason: "declined by user".into() }));
        events.insert(8, ev(100, 703, EventKind::LoopWarning { kind: "repeated_failure".into(), detail: "d".into() }));
        let text = render_timeline(&loaded(events, ReadReport::default()));
        assert!(text.contains("gate: DENIED (declined by user)"));
        assert!(text.contains("! repeated_failure: d"));
    }

    #[test]
    fn json_output_has_summary_events_and_issues() {
        let v = to_json(&loaded(fixture(), ReadReport::default()));
        assert_eq!(v["summary"]["status"], "completed");
        assert_eq!(v["events"].as_array().unwrap().len(), 14);
        assert_eq!(v["log_issues"]["truncated_last_line"], false);
    }

    fn temp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lightning-test-inspect-{}", uuid::Uuid::new_v4()))
    }

    fn write_session(home: &Path, id: &str, events: &[Event], with_summary: bool) {
        let dir = SessionDir::create(home, &id.into()).unwrap();
        let sink = JsonlSink::create(dir.events_path()).unwrap();
        for e in events {
            sink.emit(e);
        }
        sink.close();
        if with_summary {
            Summary::from_events(id, events).write(&dir.summary_path()).unwrap();
        }
    }

    #[test]
    fn lists_newest_first_finds_by_prefix_and_rebuilds_missing_summaries() {
        let home = temp_home();
        write_session(&home, "20261010T100000Z-aaaa", &fixture(), true);
        write_session(&home, "20261010T110000Z-bbbb", &fixture()[..8], false);

        let ids = session_ids(&home).unwrap();
        assert_eq!(ids, vec!["20261010T110000Z-bbbb", "20261010T100000Z-aaaa"]);
        assert_eq!(find_session(&home, "latest").unwrap(), "20261010T110000Z-bbbb");
        assert_eq!(find_session(&home, "20261010T10").unwrap(), "20261010T100000Z-aaaa");
        assert!(find_session(&home, "20261010").is_err()); // ambiguous
        assert!(find_session(&home, "zzz").is_err());

        let list = list_sessions(&home).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].status, None); // rebuilt from events, never ended
        assert_eq!(list[1].status, Some(SessionStatus::Completed));
        let text = render_list(&list);
        assert!(text.contains("unfinished"));
        assert!(text.contains("completed"));
        assert_eq!(render_list(&[]), "No sessions recorded yet.\n");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn empty_home_has_no_sessions() {
        let home = temp_home();
        assert!(session_ids(&home).unwrap().is_empty());
        assert!(find_session(&home, "latest").is_err());
    }
}
