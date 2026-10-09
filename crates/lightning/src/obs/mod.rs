//! Observability without LGTM: typed events, JSONL canonical store, summary
//! (spec 001, section 9). Everything observable goes through [`Recorder::emit`].
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};

pub mod console;
pub mod event;
pub mod inspect;
pub mod jsonl;
pub mod redact;
pub mod summary;

pub use event::{Event, EventKind, Outcome, SCHEMA_VERSION, SessionStatus, ToolUseRef, preview};
pub use jsonl::{JsonlSink, PayloadLog, ReadReport, read_events};
pub use redact::Redactor;
pub use summary::Summary;

/// Sortable session identifier: UTC timestamp plus 4 random hex chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        let rand = uuid::Uuid::new_v4().simple().to_string();
        SessionId(format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), &rand[..4]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        SessionId::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        SessionId(s.to_string())
    }
}

/// On-disk layout of one session under `<home>/sessions/<id>/`.
#[derive(Debug, Clone)]
pub struct SessionDir {
    dir: PathBuf,
}

impl SessionDir {
    pub fn sessions_root(home: &Path) -> PathBuf {
        home.join("sessions")
    }

    /// Creates the directory (and parents).
    pub fn create(home: &Path, id: &SessionId) -> io::Result<Self> {
        let dir = Self::sessions_root(home).join(id.as_str());
        fs::create_dir_all(&dir)?;
        Ok(SessionDir { dir })
    }

    /// Opens an existing session directory without creating it.
    pub fn open(home: &Path, id: &str) -> io::Result<Self> {
        let dir = Self::sessions_root(home).join(id);
        if dir.is_dir() {
            Ok(SessionDir { dir })
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, format!("no such session: {id}")))
        }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn events_path(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    pub fn payloads_path(&self) -> PathBuf {
        self.dir.join("llm_payloads.jsonl")
    }

    pub fn summary_path(&self) -> PathBuf {
        self.dir.join("summary.json")
    }
}

/// Destination for events. Implementations must not panic or fail the session.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &Event);
    /// Called at step boundaries and at session end.
    fn flush(&self) {}
    /// Final flush; durable sinks also sync to disk.
    fn close(&self) {
        self.flush();
    }
}

/// Keeps events in memory. Used by tests and by anything that wants to look at a
/// live session (the GUI later). Clones share the same buffer.
#[derive(Clone, Default)]
pub struct MemorySink(Arc<Mutex<Vec<Event>>>);

impl MemorySink {
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EventSink for MemorySink {
    fn emit(&self, event: &Event) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event.clone());
    }
}

struct Counters {
    seq: u64,
    span: u64,
}

/// Stamps, redacts and fans out events. Cheap to share by reference.
pub struct Recorder {
    session_id: SessionId,
    redactor: Redactor,
    sinks: Vec<Box<dyn EventSink>>,
    counters: Mutex<Counters>,
}

impl Recorder {
    pub fn new(session_id: SessionId, redactor: Redactor, sinks: Vec<Box<dyn EventSink>>) -> Self {
        Recorder { session_id, redactor, sinks, counters: Mutex::new(Counters { seq: 0, span: 0 }) }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Allocates a span id (`s_1`, `s_2`, ...), unique within the session.
    pub fn new_span(&self) -> String {
        let mut c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        c.span += 1;
        format!("s_{}", c.span)
    }

    /// Stamps, redacts and delivers one event; returns its `seq`.
    /// Sequence assignment and sink delivery happen under one lock so every sink
    /// sees events in `seq` order.
    pub fn emit(&self, kind: EventKind, span_id: Option<&str>, parent_span_id: Option<&str>) -> u64 {
        let mut c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        c.seq += 1;
        let event = Event {
            v: SCHEMA_VERSION,
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            session_id: self.session_id.to_string(),
            seq: c.seq,
            kind,
            span_id: span_id.map(str::to_string),
            parent_span_id: parent_span_id.map(str::to_string),
        };
        let event = self.redactor.redact_event(&event);
        for sink in &self.sinks {
            sink.emit(&event);
            if event.kind.is_flush_point() {
                sink.flush();
            }
        }
        c.seq
    }

    /// Flushes and syncs every sink. Call once when the session is over.
    pub fn close(&self) {
        for sink in &self.sinks {
            sink.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recorder(secret: &str) -> (Recorder, MemorySink) {
        let sink = MemorySink::default();
        let sinks: Vec<Box<dyn EventSink>> = vec![Box::new(sink.clone())];
        let rec = Recorder::new(SessionId::from("sess"), Redactor::new([secret.to_string()]), sinks);
        (rec, sink)
    }

    #[test]
    fn seq_increases_and_spans_are_unique() {
        let (rec, store) = recorder("sk-secret-123456");
        let a = rec.new_span();
        let b = rec.new_span();
        assert_ne!(a, b);
        rec.emit(EventKind::StepStarted { step: 1 }, Some(&a), None);
        rec.emit(EventKind::StepFinished { step: 1, duration_ms: 1 }, Some(&a), None);
        rec.emit(EventKind::StepStarted { step: 2 }, Some(&b), None);
        let seqs: Vec<u64> = store.events().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert!(store.events().iter().all(|e| e.session_id == "sess" && e.v == SCHEMA_VERSION));
    }

    #[test]
    fn secrets_never_reach_sinks() {
        let (rec, store) = recorder("sk-secret-123456");
        rec.emit(EventKind::UserMessage { text: "use sk-secret-123456 please".into() }, None, None);
        let dump = serde_json::to_string(&store.events()).unwrap();
        assert!(!dump.contains("sk-secret-123456"));
        assert!(dump.contains("[REDACTED]"));
    }

    #[test]
    fn session_ids_sort_by_time_and_have_a_suffix() {
        let id = SessionId::new();
        let (stamp, rand) = id.as_str().split_once('-').unwrap();
        assert_eq!(stamp.len(), 16);
        assert_eq!(rand.len(), 4);
    }

    #[test]
    fn session_dir_layout() {
        let home = std::env::temp_dir().join(format!("lightning-test-layout-{}", uuid::Uuid::new_v4()));
        let id = SessionId::from("20261010T000000Z-abcd");
        let dir = SessionDir::create(&home, &id).unwrap();
        assert!(dir.path().ends_with("sessions/20261010T000000Z-abcd") || dir.path().ends_with("sessions\\20261010T000000Z-abcd"));
        assert!(dir.events_path().ends_with("events.jsonl"));
        assert!(SessionDir::open(&home, "20261010T000000Z-abcd").is_ok());
        assert!(SessionDir::open(&home, "missing").is_err());
        fs::remove_dir_all(home).ok();
    }
}
