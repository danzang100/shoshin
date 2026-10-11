//! Small, pure formatting helpers for the UI. Tested natively.
use lightning_protocol::SessionStatus;
use serde_json::Value;

pub fn ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{}m {:02}s", ms / 60_000, (ms / 1000) % 60)
    }
}

/// 1234 -> "1.2k", 999 -> "999".
pub fn tokens(n: Option<u64>) -> String {
    match n {
        None => "-".to_string(),
        Some(n) if n < 1000 => n.to_string(),
        Some(n) if n < 100_000 => format!("{:.1}k", n as f64 / 1000.0),
        Some(n) => format!("{}k", n / 1000),
    }
}

pub fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Completed => "completed",
        SessionStatus::MaxStepsReached => "too many steps",
        SessionStatus::LlmError => "model error",
        SessionStatus::Cancelled => "stopped",
        SessionStatus::Stuck => "stuck",
        SessionStatus::ContextLimit => "conversation too long",
        SessionStatus::TruncatedOutput => "reply cut off",
        SessionStatus::InternalError => "internal error",
    }
}

/// CSS class for a status chip: ok, warn or bad.
pub fn status_class(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Completed => "ok",
        SessionStatus::Cancelled | SessionStatus::TruncatedOutput | SessionStatus::MaxStepsReached | SessionStatus::ContextLimit => "warn",
        SessionStatus::LlmError | SessionStatus::Stuck | SessionStatus::InternalError => "bad",
    }
}

/// `20261010T104200Z-ab12` -> `Oct 10, 10:42`. Falls back to the id.
pub fn session_time(id: &str) -> String {
    let b = id.as_bytes();
    if b.len() < 15 || b[8] != b'T' || !b[..8].iter().all(u8::is_ascii_digit) {
        return id.to_string();
    }
    let month = match &id[4..6] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return id.to_string(),
    };
    format!("{month} {}, {}:{}", id[6..8].trim_start_matches('0'), &id[9..11], &id[11..13])
}

/// Last path component of a Windows or Unix path.
pub fn folder_name(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(path).to_string()
}

/// `plural(1, "tool")` -> "1 tool", `plural(2, "tool")` -> "2 tools".
pub fn plural(n: u64, noun: &str) -> String {
    if n == 1 { format!("{n} {noun}") } else { format!("{n} {noun}s") }
}

pub fn shorten(text: &str, max_chars: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let cut: String = flat.chars().take(max_chars).collect();
        format!("{cut}...")
    }
}

/// One-line view of tool arguments: `file_path=Cargo.toml`, or nothing for `{}`.
pub fn args_summary(args: &Value) -> String {
    match args {
        Value::Object(map) if map.is_empty() => String::new(),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}={}", shorten(s, 40)),
                    other => format!("{k}={}", shorten(&other.to_string(), 40)),
                })
                .collect();
            parts.join(", ")
        }
        Value::Null => String::new(),
        other => shorten(&other.to_string(), 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn durations_and_tokens() {
        assert_eq!(ms(12), "12 ms");
        assert_eq!(ms(1500), "1.5 s");
        assert_eq!(ms(125_000), "2m 05s");
        assert_eq!(tokens(None), "-");
        assert_eq!(tokens(Some(999)), "999");
        assert_eq!(tokens(Some(1234)), "1.2k");
        assert_eq!(tokens(Some(250_000)), "250k");
    }

    #[test]
    fn session_ids_become_readable_times() {
        assert_eq!(session_time("20261010T104200Z-ab12"), "Oct 10, 10:42");
        assert_eq!(session_time("20260305T090000Z-ab12"), "Mar 5, 09:00");
        assert_eq!(session_time("garbage"), "garbage");
        assert_eq!(session_time("20269910T104200Z-ab12"), "20269910T104200Z-ab12");
    }

    #[test]
    fn pluralisation() {
        assert_eq!(plural(0, "tool"), "0 tools");
        assert_eq!(plural(1, "tool"), "1 tool");
        assert_eq!(plural(3, "tool"), "3 tools");
    }

    #[test]
    fn folder_names_from_both_path_styles() {
        assert_eq!(folder_name("C:\\Users\\me\\proj"), "proj");
        assert_eq!(folder_name("C:\\Users\\me\\proj\\"), "proj");
        assert_eq!(folder_name("/home/me/proj"), "proj");
        assert_eq!(folder_name("proj"), "proj");
    }

    #[test]
    fn args_are_summarised_on_one_line() {
        assert_eq!(args_summary(&json!({})), "");
        assert_eq!(args_summary(&json!({ "file_path": "Cargo.toml" })), "file_path=Cargo.toml");
        let long = args_summary(&json!({ "content": "a\n".repeat(100) }));
        assert!(long.ends_with("...") && !long.contains('\n'));
        assert_eq!(args_summary(&Value::Null), "");
        assert_eq!(shorten("  a   b \n c ", 20), "a b c");
    }

    #[test]
    fn every_status_has_a_label_and_class() {
        use SessionStatus::*;
        for s in [Completed, MaxStepsReached, LlmError, Cancelled, Stuck, ContextLimit, TruncatedOutput, InternalError] {
            assert!(!status_label(s).is_empty());
            assert!(["ok", "warn", "bad"].contains(&status_class(s)));
        }
    }
}
