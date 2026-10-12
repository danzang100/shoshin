//! Claude Code transcript adapter (format verified against local data, spec 003 section 5).
//!
//! Files are opened read-only and never written.
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

use super::{Adapter, DiscoveredFile};
use crate::redact::{Hasher, last_component};
use crate::schema::*;

pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn detect(&self, root: &Path) -> bool {
        root.is_dir()
    }

    fn discover(&self, root: &Path) -> Result<Vec<DiscoveredFile>> {
        let mut out = Vec::new();
        for project in sorted_entries(root)? {
            if !project.is_dir() {
                continue;
            }
            let slug = file_name(&project);
            for entry in sorted_entries(&project)? {
                if entry.is_file() && has_ext(&entry, "jsonl") {
                    out.push(DiscoveredFile {
                        session_id: stem(&entry),
                        path: entry,
                        project: slug.clone(),
                        is_subagent: false,
                        agent_id: None,
                    });
                } else if entry.is_dir() {
                    let session_id = file_name(&entry);
                    let sub = entry.join("subagents");
                    if !sub.is_dir() {
                        continue;
                    }
                    for f in sorted_entries(&sub)? {
                        if f.is_file() && has_ext(&f, "jsonl") {
                            let agent = stem(&f);
                            let agent = agent.strip_prefix("agent-").unwrap_or(&agent).to_string();
                            out.push(DiscoveredFile {
                                path: f,
                                project: slug.clone(),
                                session_id: session_id.clone(),
                                is_subagent: true,
                                agent_id: Some(agent),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn parse(&self, file: &DiscoveredFile, hasher: &Hasher) -> Result<ParsedFile> {
        let f = File::open(&file.path).with_context(|| format!("open {}", file.path.display()))?;
        parse_reader(BufReader::new(f), file, hasher)
    }
}

fn sorted_entries(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    v.sort();
    Ok(v)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

struct UsageSnap {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    w5m: Option<i64>,
    w1h: Option<i64>,
}

struct TurnAcc {
    turn: Turn,
    best_output: i64,
    has_usage: bool,
}

struct Pending {
    is_error: bool,
    permission: Permission,
    denial_kind: Option<String>,
}

fn norm_ts(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let dt = DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc);
    Some(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str().filter(|s| !s.is_empty())
}

fn i64_of(v: &Value, key: &str) -> Option<i64> {
    v.get(key)?.as_i64()
}

fn read_usage(u: &Value) -> UsageSnap {
    let cc = u.get("cache_creation");
    let w5 = cc.and_then(|c| i64_of(c, "ephemeral_5m_input_tokens"));
    let w1 = cc.and_then(|c| i64_of(c, "ephemeral_1h_input_tokens"));
    let total = i64_of(u, "cache_creation_input_tokens");
    // Without the TTL breakdown, attribute the total to the default 5 minute tier.
    let (w5m, w1h) = if w5.is_none() && w1.is_none() { (total, None) } else { (w5, w1) };
    UsageSnap {
        input: i64_of(u, "input_tokens"),
        output: i64_of(u, "output_tokens"),
        cache_read: i64_of(u, "cache_read_input_tokens"),
        w5m,
        w1h,
    }
}

const TARGET_TOOLS: [&str; 4] = ["Read", "Write", "Edit", "NotebookEdit"];

fn permission_of(line: &Value) -> (Permission, Option<String>) {
    if let Some(kind) = str_of(line, "toolDenialKind") {
        return (Permission::Denied, Some(kind.to_string()));
    }
    let decision = match line.get("permissionDecision") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(o @ Value::Object(_)) => o.get("decision").and_then(Value::as_str),
        _ => None,
    };
    match decision {
        Some("accept" | "allow" | "allowed") => (Permission::Allowed, None),
        Some("reject" | "deny" | "denied") => (Permission::Denied, None),
        _ => (Permission::None, None),
    }
}

pub fn parse_reader<R: BufRead>(mut reader: R, file: &DiscoveredFile, hasher: &Hasher) -> Result<ParsedFile> {
    let mut out = ParsedFile::default();
    let mut accs: Vec<TurnAcc> = Vec::new();
    let mut acc_index: HashMap<(String, String), usize> = HashMap::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut call_index: HashMap<String, usize> = HashMap::new();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let (mut first_ts, mut last_ts): (Option<String>, Option<String>) = (None, None);
    let (mut version, mut cwd, mut branch): (Option<String>, Option<String>, Option<String>) = (None, None, None);

    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let complete = buf.ends_with(b"\n");
        let text = String::from_utf8_lossy(&buf);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.stats.lines += 1;
        let line: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) if !complete => {
                out.stats.partial_tail = true;
                continue;
            }
            Err(_) => {
                out.stats.bad_lines += 1;
                continue;
            }
        };
        let ty = line.get("type").and_then(Value::as_str).unwrap_or("(none)");
        if ty != "assistant" && ty != "user" {
            *out.stats.unknown_types.entry(ty.to_string()).or_default() += 1;
            continue;
        }

        let ts = norm_ts(line.get("timestamp"));
        if let Some(t) = &ts {
            if first_ts.as_ref().is_none_or(|f| t < f) {
                first_ts = Some(t.clone());
            }
            if last_ts.as_ref().is_none_or(|l| t > l) {
                last_ts = Some(t.clone());
            }
        }
        if version.is_none() {
            version = str_of(&line, "version").map(String::from);
        }
        if cwd.is_none() {
            cwd = str_of(&line, "cwd").map(String::from);
        }
        if branch.is_none() {
            branch = str_of(&line, "gitBranch").map(String::from);
        }

        let line_agent = str_of(&line, "agentId").map(String::from);
        let is_subagent = file.is_subagent || line.get("isSidechain").and_then(Value::as_bool) == Some(true) || line_agent.is_some();
        let agent_id = line_agent.or_else(|| file.agent_id.clone()).unwrap_or_default();
        let session_id = if file.is_subagent {
            file.session_id.clone()
        } else {
            str_of(&line, "sessionId").map(String::from).unwrap_or_else(|| file.session_id.clone())
        };
        let cwd_ref = cwd.as_deref();

        if ty == "user" {
            let content = line.get("message").and_then(|m| m.get("content"));
            let is_meta = line.get("isMeta").and_then(Value::as_bool) == Some(true);
            let mut saw_result = false;
            if let Some(Value::Array(blocks)) = content {
                let (perm, kind) = permission_of(&line);
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        saw_result = true;
                        if let Some(id) = str_of(b, "tool_use_id") {
                            pending.insert(
                                id.to_string(),
                                Pending {
                                    is_error: b.get("is_error").and_then(Value::as_bool) == Some(true),
                                    permission: perm,
                                    denial_kind: kind.clone(),
                                },
                            );
                        }
                    }
                }
            }
            let kind = if is_meta {
                SignalKind::Meta
            } else if saw_result {
                SignalKind::ToolResult
            } else {
                SignalKind::Prompt
            };
            out.signals.push(UserSignal { session_id, agent_id, ts, kind });
            continue;
        }

        // assistant
        let Some(msg) = line.get("message") else { continue };
        let key = str_of(msg, "id").or_else(|| str_of(&line, "uuid")).unwrap_or("").to_string();
        if key.is_empty() {
            out.stats.bad_lines += 1;
            continue;
        }
        let idx = *acc_index.entry((agent_id.clone(), key.clone() + "|" + &session_id)).or_insert_with(|| {
            let turn_id = hasher.hash(&format!("{session_id}|{agent_id}|{key}"));
            accs.push(TurnAcc {
                turn: Turn {
                    turn_id,
                    session_id: session_id.clone(),
                    agent_id: agent_id.clone(),
                    is_subagent,
                    parent_session_id: is_subagent.then(|| session_id.clone()),
                    project: file.project.clone(),
                    project_name: String::new(),
                    source_version: str_of(&line, "version").map(String::from),
                    model: String::new(),
                    ts: ts.clone(),
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_5m_tokens: None,
                    cache_write_1h_tokens: None,
                    stop_reason: None,
                    is_api_error: false,
                    is_aborted: false,
                },
                best_output: -1,
                has_usage: false,
            });
            accs.len() - 1
        });
        let acc = &mut accs[idx];
        if acc.turn.model.is_empty() {
            if let Some(m) = str_of(msg, "model") {
                acc.turn.model = m.to_string();
            }
        }
        acc.turn.is_api_error |= line.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
        acc.turn.is_aborted |= line.get("isAbortedMidStream").and_then(Value::as_bool) == Some(true);
        if let Some(u) = msg.get("usage").filter(|u| u.is_object()) {
            let snap = read_usage(u);
            let out_tokens = snap.output.unwrap_or(0);
            if !acc.has_usage || out_tokens >= acc.best_output {
                acc.has_usage = true;
                acc.best_output = out_tokens;
                acc.turn.input_tokens = snap.input;
                acc.turn.output_tokens = snap.output;
                acc.turn.cache_read_tokens = snap.cache_read;
                acc.turn.cache_write_5m_tokens = snap.w5m;
                acc.turn.cache_write_1h_tokens = snap.w1h;
                if let Some(s) = str_of(msg, "stop_reason") {
                    acc.turn.stop_reason = Some(s.to_string());
                }
            }
        }
        if acc.turn.stop_reason.is_none() {
            acc.turn.stop_reason = str_of(msg, "stop_reason").map(String::from);
        }
        let turn_id = acc.turn.turn_id.clone();

        if let Some(Value::Array(blocks)) = msg.get("content") {
            for b in blocks {
                if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let Some(id) = str_of(b, "id") else { continue };
                if call_index.contains_key(id) {
                    continue;
                }
                let name = str_of(b, "name").unwrap_or("unknown").to_string();
                let input = b.get("input").cloned().unwrap_or(Value::Null);
                let target = TARGET_TOOLS
                    .contains(&name.as_str())
                    .then(|| str_of(&input, "file_path").or_else(|| str_of(&input, "notebook_path")))
                    .flatten()
                    .map(|p| hasher.hash_path(p, cwd_ref));
                call_index.insert(id.to_string(), calls.len());
                calls.push(ToolCall {
                    tool_use_id: id.to_string(),
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    agent_id: agent_id.clone(),
                    seq: calls.len() as i64,
                    ts: ts.clone(),
                    tool_name: name,
                    args_hash: hasher.hash_json(&input),
                    target_hash: target,
                    result_status: ResultStatus::Unknown,
                    permission: Permission::None,
                    denial_kind: None,
                });
            }
        }
    }

    for c in &mut calls {
        if let Some(p) = pending.get(&c.tool_use_id) {
            c.result_status = if p.is_error { ResultStatus::Error } else { ResultStatus::Ok };
            c.permission = p.permission;
            c.denial_kind = p.denial_kind.clone();
        }
    }

    let project_name = cwd.as_deref().map(last_component).filter(|s| !s.is_empty()).unwrap_or_else(|| file.project.clone());
    for a in &mut accs {
        a.turn.project_name = project_name.clone();
    }

    if out.stats.lines > 0 {
        out.session = Some(Session {
            session_id: file.session_id.clone(),
            agent_id: file.agent_id.clone().unwrap_or_default(),
            source: SOURCE_CLAUDE_CODE.to_string(),
            source_version: version,
            project: file.project.clone(),
            project_name,
            cwd_hash: cwd.as_deref().map(|c| hasher.hash(&crate::redact::normalize_path(c, None))),
            git_branch: branch,
            started_at: first_ts,
            ended_at: last_ts,
        });
    }
    out.turns = accs.into_iter().map(|a| a.turn).collect();
    out.tool_calls = calls;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_root")
    }

    fn parse_fixture(rel: &str) -> ParsedFile {
        let root = fixtures();
        let files = ClaudeCode.discover(&root).unwrap();
        let f = files.iter().find(|f| f.path.to_string_lossy().replace('\\', "/").ends_with(rel)).unwrap();
        ClaudeCode.parse(f, &Hasher::new("test-salt")).unwrap()
    }

    #[test]
    fn discovers_main_and_subagent_files() {
        let files = ClaudeCode.discover(&fixtures()).unwrap();
        assert_eq!(files.len(), 3);
        let sub = files.iter().find(|f| f.is_subagent).unwrap();
        assert_eq!(sub.session_id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(sub.agent_id.as_deref(), Some("abc123"));
        assert_eq!(sub.project, "-proj-alpha");
    }

    #[test]
    fn duplicate_message_lines_collapse_to_one_turn_with_final_usage() {
        let p = parse_fixture("11111111-1111-4111-8111-111111111111.jsonl");
        let m1: Vec<_> = p.turns.iter().filter(|t| t.input_tokens == Some(10)).collect();
        assert_eq!(m1.len(), 1, "three lines sharing msg_m1 must be one turn");
        let t = m1[0];
        assert_eq!(t.output_tokens, Some(42));
        assert_eq!(t.cache_read_tokens, Some(100));
        assert_eq!(t.cache_write_5m_tokens, Some(50));
        assert_eq!(t.cache_write_1h_tokens, Some(150));
        assert_eq!(t.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(p.turns.len(), 6);
        let total_out: i64 = p.turns.iter().filter_map(|t| t.output_tokens).sum();
        assert_eq!(total_out, 42 + 30 + 10 + 8 + 5);
    }

    #[test]
    fn tool_results_permissions_and_targets() {
        let p = parse_fixture("11111111-1111-4111-8111-111111111111.jsonl");
        let by = |id: &str| p.tool_calls.iter().find(|c| c.tool_use_id == id).unwrap();
        assert_eq!(by("toolu_t1").result_status, ResultStatus::Ok);
        assert_eq!(by("toolu_t3").result_status, ResultStatus::Error);
        assert_eq!(by("toolu_t4").permission, Permission::Allowed);
        assert_eq!(by("toolu_t6").permission, Permission::Denied);
        assert_eq!(by("toolu_t6").denial_kind.as_deref(), Some("user-rejected"));
        // Same file spelled differently hashes to the same target.
        assert_eq!(by("toolu_t1").target_hash, by("toolu_t2").target_hash);
        assert!(by("toolu_t1").target_hash.is_some());
        assert!(by("toolu_t3").target_hash.is_none());
        assert_eq!(p.tool_calls.len(), 6);
        let seqs: Vec<_> = p.tool_calls.iter().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn tolerant_of_garbage_and_unknown_types() {
        let p = parse_fixture("11111111-1111-4111-8111-111111111111.jsonl");
        assert_eq!(p.stats.bad_lines, 1);
        assert!(p.stats.partial_tail);
        assert_eq!(p.stats.unknown_types.get("system"), Some(&1));
        assert_eq!(p.stats.unknown_types.get("weird-new-type"), Some(&1));
    }

    #[test]
    fn synthetic_turn_is_kept_with_null_usage() {
        let p = parse_fixture("11111111-1111-4111-8111-111111111111.jsonl");
        let s = p.turns.iter().find(|t| t.model == SYNTHETIC_MODEL).unwrap();
        assert_eq!(s.input_tokens, None);
        assert_eq!(s.cache_read_tokens, None);
    }

    #[test]
    fn subagent_turns_link_to_parent() {
        let p = parse_fixture("subagents/agent-abc123.jsonl");
        assert_eq!(p.turns.len(), 1);
        let t = &p.turns[0];
        assert!(t.is_subagent);
        assert_eq!(t.agent_id, "abc123");
        assert_eq!(t.parent_session_id.as_deref(), Some("11111111-1111-4111-8111-111111111111"));
        assert_eq!(t.cache_write_5m_tokens, Some(30));
        assert_eq!(p.tool_calls.len(), 2);
    }

    #[test]
    fn missing_fields_stay_null_and_total_cache_write_goes_to_5m() {
        let p = parse_fixture("22222222-2222-4222-8222-222222222222.jsonl");
        assert_eq!(p.turns.len(), 3);
        let n1 = p.turns.iter().find(|t| t.output_tokens == Some(11)).unwrap();
        assert_eq!(n1.cache_write_5m_tokens, Some(40));
        assert_eq!(n1.cache_write_1h_tokens, None);
        let n2 = p.turns.iter().find(|t| t.output_tokens == Some(9)).unwrap();
        assert_eq!(n2.cache_read_tokens, None);
        assert_eq!(n2.cache_write_5m_tokens, None);
        let n3 = p.turns.iter().find(|t| t.model == "claude-haiku-9").unwrap();
        assert!(n3.is_api_error);
        assert_eq!(p.signals.iter().filter(|s| s.kind == SignalKind::Meta).count(), 1);
        assert_eq!(p.signals.iter().filter(|s| s.kind == SignalKind::Prompt).count(), 1);
        assert_eq!(p.session.as_ref().unwrap().project_name, "beta");
    }

    #[test]
    fn crlf_lines_parse() {
        let data = "{\"type\":\"assistant\",\"uuid\":\"x\",\"message\":{\"id\":\"m\",\"model\":\"a\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\r\n";
        let f = DiscoveredFile {
            path: "x.jsonl".into(),
            project: "p".into(),
            session_id: "s".into(),
            is_subagent: false,
            agent_id: None,
        };
        let p = parse_reader(data.as_bytes(), &f, &Hasher::new("s")).unwrap();
        assert_eq!(p.turns.len(), 1);
        assert_eq!(p.stats.bad_lines, 0);
    }

    #[test]
    fn parser_never_panics_on_truncations_of_a_real_shaped_file() {
        let root = fixtures();
        let text = std::fs::read_to_string(root.join("-proj-alpha/11111111-1111-4111-8111-111111111111.jsonl")).unwrap();
        let f = DiscoveredFile {
            path: "x".into(),
            project: "p".into(),
            session_id: "s".into(),
            is_subagent: false,
            agent_id: None,
        };
        for cut in (0..text.len()).filter(|i| text.is_char_boundary(*i)) {
            parse_reader(text[..cut].as_bytes(), &f, &Hasher::new("s")).unwrap();
        }
    }
}
