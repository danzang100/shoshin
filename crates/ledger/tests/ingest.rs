use std::path::PathBuf;

use ledger::adapter::claude_code::ClaudeCode;
use ledger::ingest::{ensure_db_outside_root, ingest};
use ledger::store::Store;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_root")
}

const CANARIES: [&str; 11] = [
    "CANARY-PROMPT-7f3a",
    "CANARY-ARGS-91bc",
    "CANARY-OUTPUT-55de",
    "CANARY-THINKING-1",
    "CANARY-ASSISTANT-TEXT",
    "CANARY-WRITE-BODY",
    "CANARY-EDIT-OLD",
    "CANARY-SUBAGENT-PROMPT",
    "CANARY-META",
    "CANARY-SYSTEM",
    "CANARY-PROMPT-LIST",
];

#[test]
fn ingest_counts_and_is_idempotent() {
    let mut store = Store::open_in_memory().unwrap();
    let a = ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
    assert_eq!((a.files_seen, a.files_parsed, a.files_skipped), (3, 3, 0));
    assert_eq!((a.turns, a.tool_calls, a.sessions), (10, 8, 2));
    assert_eq!(a.bad_lines, 1);
    assert_eq!(a.partial_tails, 1);
    assert_eq!(a.unknown_types.get("weird-new-type"), Some(&1));

    let before: Vec<i64> = ["turns", "tool_calls", "signals", "files", "sessions"]
        .iter()
        .map(|t| store.count(t).unwrap())
        .collect();
    let b = ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
    assert_eq!((b.files_parsed, b.files_skipped), (0, 3));
    let after: Vec<i64> = ["turns", "tool_calls", "signals", "files", "sessions"]
        .iter()
        .map(|t| store.count(t).unwrap())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn reingesting_a_changed_file_replaces_its_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("root/-p");
    std::fs::create_dir_all(&proj).unwrap();
    let f = proj.join("33333333-3333-4333-8333-333333333333.jsonl");
    let line = |id: &str, out: u32| {
        format!(
            "{{\"type\":\"assistant\",\"uuid\":\"{id}\",\"timestamp\":\"2026-10-03T00:00:00Z\",\"message\":{{\"id\":\"{id}\",\"model\":\"m\",\"content\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":{out}}}}}}}\n"
        )
    };
    std::fs::write(&f, line("a", 5)).unwrap();
    let mut store = Store::open(&tmp.path().join("db/l.db")).unwrap();
    let root = tmp.path().join("root");
    assert_eq!(ingest(&mut store, &ClaudeCode, &root).unwrap().turns, 1);
    std::fs::write(&f, format!("{}{}", line("a", 7), line("b", 9))).unwrap();
    let s = ingest(&mut store, &ClaudeCode, &root).unwrap();
    assert_eq!((s.files_parsed, s.turns), (1, 2));
    let sum: i64 = store.conn().query_row("SELECT SUM(output_tokens) FROM turns", [], |r| r.get(0)).unwrap();
    assert_eq!(sum, 16);
}

#[test]
fn resumed_session_copies_count_once_and_stay_with_the_original() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("root/-p");
    std::fs::create_dir_all(&proj).unwrap();
    let line = |session: &str, id: &str, out: u32| {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"uuid\":\"u-{id}\",\"requestId\":\"r-{id}\",\"timestamp\":\"2026-10-03T00:00:00Z\",\"message\":{{\"id\":\"{id}\",\"model\":\"m\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_{id}\",\"name\":\"Bash\",\"input\":{{}}}}],\"usage\":{{\"input_tokens\":1,\"output_tokens\":{out}}}}}}}\n"
        )
    };
    let orig_id = "44444444-4444-4444-8444-444444444444";
    // Named so it sorts first by path: only the file age should decide ownership.
    let resumed_id = "00000000-0000-4000-8000-000000000000";
    let orig = proj.join(format!("{orig_id}.jsonl"));
    let resumed = proj.join(format!("{resumed_id}.jsonl"));
    std::fs::write(&orig, line(orig_id, "msg_a", 5) + &line(orig_id, "msg_b", 7)).unwrap();
    // The resumed file repeats both calls under its own session id, then adds a new one.
    std::fs::write(
        &resumed,
        line(resumed_id, "msg_a", 5) + &line(resumed_id, "msg_b", 7) + &line(resumed_id, "msg_c", 11),
    )
    .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&orig).unwrap().set_modified(old).unwrap();

    let mut store = Store::open(&tmp.path().join("db/l.db")).unwrap();
    let s = ingest(&mut store, &ClaudeCode, &tmp.path().join("root")).unwrap();
    assert_eq!((s.turns, s.tool_calls), (3, 3));
    let out_by = |session: &str| -> i64 {
        store
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(output_tokens), 0) FROM turns WHERE session_id = ?1",
                [session],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(out_by(orig_id), 12, "copied turns belong to the session that made them");
    assert_eq!(out_by(resumed_id), 11, "only the new turn belongs to the resumed session");
}

#[test]
fn version_1_database_is_rebuilt() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("l.db");
    {
        let mut store = Store::open(&db).unwrap();
        ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
        store.conn().execute_batch("PRAGMA user_version = 1").unwrap();
    }
    let mut store = Store::open(&db).unwrap();
    for t in ["turns", "tool_calls", "signals", "files", "sessions"] {
        assert_eq!(store.count(t).unwrap(), 0, "{t} should be cleared");
    }
    let s = ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
    assert_eq!((s.files_parsed, s.turns, s.tool_calls), (3, 10, 8));
}

#[test]
fn no_raw_content_reaches_the_database_file() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("l.db");
    {
        let mut store = Store::open(&db).unwrap();
        ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
    }
    let bytes = std::fs::read(&db).unwrap();
    let hay = String::from_utf8_lossy(&bytes);
    for c in CANARIES {
        assert!(!hay.contains(c), "{c} leaked into the database");
    }
    for p in [r"C:\proj", "c:/proj", "src/a.rs", r"src\a.rs", "b.rs"] {
        assert!(!hay.contains(p), "path fragment {p} leaked into the database");
    }
}

#[test]
fn transcripts_are_not_modified() {
    fn snapshot(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut v = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    v.push((p.clone(), std::fs::read(p).unwrap()));
                }
            }
        }
        v.sort();
        v
    }
    let before = snapshot(&fixtures());
    let mut store = Store::open_in_memory().unwrap();
    ingest(&mut store, &ClaudeCode, &fixtures()).unwrap();
    assert_eq!(before, snapshot(&fixtures()));
}

#[test]
fn db_inside_root_is_refused() {
    let root = fixtures();
    assert!(ensure_db_outside_root(&root.join("x.db"), &root).is_err());
    assert!(ensure_db_outside_root(&root.join("new/dir/x.db"), &root).is_err());
    let tmp = tempfile::tempdir().unwrap();
    assert!(ensure_db_outside_root(&tmp.path().join("x.db"), &root).is_ok());
}

#[test]
fn missing_root_is_an_error() {
    let mut store = Store::open_in_memory().unwrap();
    assert!(ingest(&mut store, &ClaudeCode, &fixtures().join("nope")).is_err());
}
