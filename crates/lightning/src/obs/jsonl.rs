//! Canonical JSONL store: write sink and tolerant reader (spec 001, section 9).
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

use super::EventSink;
use super::event::Event;
use super::redact::Redactor;

/// Appends one JSON object per line. A write failure disables the sink after one
/// message on stderr; it never fails the session.
pub struct JsonlSink {
    path: PathBuf,
    writer: Mutex<Option<BufWriter<File>>>,
}

impl JsonlSink {
    pub fn create(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(JsonlSink { path, writer: Mutex::new(Some(BufWriter::new(file))) })
    }

    fn disable(&self, guard: &mut Option<BufWriter<File>>, err: &dyn std::fmt::Display) {
        eprintln!("warning: log sink {} disabled: {err}", self.path.display());
        *guard = None;
    }

    fn write_line<T: Serialize>(&self, item: &T, flush: bool) {
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let Some(writer) = guard.as_mut() else { return };
        let result = serde_json::to_writer(&mut *writer, item)
            .map_err(io::Error::from)
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| if flush { writer.flush() } else { Ok(()) });
        if let Err(err) = result {
            self.disable(&mut guard, &err);
        }
    }
}

impl EventSink for JsonlSink {
    fn emit(&self, event: &Event) {
        self.write_line(event, event.kind.is_flush_point());
    }

    fn flush(&self) {
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(writer) = guard.as_mut()
            && let Err(err) = writer.flush()
        {
            self.disable(&mut guard, &err);
        }
    }

    fn close(&self) {
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(writer) = guard.as_mut() {
            let result = writer.flush().and_then(|_| writer.get_ref().sync_all());
            if let Err(err) = result {
                self.disable(&mut guard, &err);
            }
        }
    }
}

/// Full LLM request/response bodies, written only at debug log level. Redacted.
pub struct PayloadLog {
    sink: JsonlSink,
    redactor: Redactor,
}

impl PayloadLog {
    pub fn create(path: impl Into<PathBuf>, redactor: Redactor) -> io::Result<Self> {
        Ok(PayloadLog { sink: JsonlSink::create(path)?, redactor })
    }

    /// `direction` is `"request"` or `"response"`; `seq` is the matching event's seq.
    pub fn record(&self, seq: u64, direction: &str, mut body: Value) {
        self.redactor.redact_value(&mut body);
        self.sink.write_line(&serde_json::json!({ "seq": seq, "direction": direction, "body": body }), true);
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// The last line was cut off (crash mid-write) and ignored.
    pub truncated_last_line: bool,
    /// Lines in the middle that could not be parsed, as 1-based line numbers.
    pub bad_lines: Vec<usize>,
}

/// Reads events back. A cut-off final line is reported, not fatal.
pub fn read_events(path: &Path) -> io::Result<(Vec<Event>, ReadReport)> {
    let text = fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut events = Vec::with_capacity(lines.len());
    let mut report = ReadReport::default();
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<Event>(line) {
            Ok(event) => events.push(event),
            Err(_) if i + 1 == lines.len() && !text.ends_with('\n') => report.truncated_last_line = true,
            Err(_) => report.bad_lines.push(i + 1),
        }
    }
    Ok((events, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::event::{EventKind, SCHEMA_VERSION};

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            v: SCHEMA_VERSION,
            ts: "t".into(),
            session_id: "s".into(),
            seq,
            kind,
            span_id: None,
            parent_span_id: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightning-test-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_valid_jsonl_and_reads_it_back() {
        let dir = temp_dir("jsonl");
        let path = dir.join("nested").join("events.jsonl");
        let sink = JsonlSink::create(&path).unwrap();
        sink.emit(&ev(1, EventKind::StepStarted { step: 1 }));
        sink.emit(&ev(2, EventKind::StepFinished { step: 1, duration_ms: 4 }));
        sink.close();

        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert!(raw.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()));

        let (events, report) = read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(report, ReadReport::default());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tolerates_truncated_last_line() {
        let dir = temp_dir("trunc");
        let path = dir.join("events.jsonl");
        let sink = JsonlSink::create(&path).unwrap();
        sink.emit(&ev(1, EventKind::StepStarted { step: 1 }));
        sink.close();
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"v":1,"ts":"t","sess"#).unwrap();

        let (events, report) = read_events(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert!(report.truncated_last_line);
        assert!(report.bad_lines.is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reports_bad_lines_in_the_middle() {
        let dir = temp_dir("bad");
        let path = dir.join("events.jsonl");
        let good = serde_json::to_string(&ev(1, EventKind::StepStarted { step: 1 })).unwrap();
        fs::write(&path, format!("{good}\nnot json\n{good}\n")).unwrap();
        let (events, report) = read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(report.bad_lines, vec![2]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn payload_log_redacts() {
        let dir = temp_dir("payload");
        let path = dir.join("llm_payloads.jsonl");
        let log = PayloadLog::create(&path, Redactor::new(["sk-secret-123456".to_string()])).unwrap();
        log.record(3, "request", serde_json::json!({ "note": "sk-secret-123456" }));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-secret-123456"));
        assert!(raw.contains("[REDACTED]"));
        fs::remove_dir_all(dir).ok();
    }
}
