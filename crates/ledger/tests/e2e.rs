use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use ledger::cli::{Cli, run};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_root")
}

fn cli(args: &[&std::ffi::OsStr]) -> String {
    let mut full: Vec<&std::ffi::OsStr> = vec!["ledger".as_ref()];
    full.extend_from_slice(args);
    let parsed = Cli::try_parse_from(full).unwrap();
    let mut out = Vec::new();
    run(parsed, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn ingest_report_doctor_through_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("data/l.db");
    let pricing = tmp.path().join("pricing.toml");
    std::fs::write(&pricing, "version = \"2026-10-01\"\n[[model]]\nid = \"claude-opus-5\"\ninput_per_mtok = 1000000.0\noutput_per_mtok = 2000000.0\ncache_read_per_mtok = 100000.0\ncache_write_5m_per_mtok = 1500000.0\ncache_write_1h_per_mtok = 3000000.0\n").unwrap();
    let (db, root, pricing) = (db.as_os_str(), fixtures(), pricing);

    let first = cli(&["--db".as_ref(), db, "ingest".as_ref(), "--root".as_ref(), root.as_os_str()]);
    assert!(first.contains("3 seen, 3 parsed, 0 unchanged"), "{first}");
    let second = cli(&["--db".as_ref(), db, "ingest".as_ref(), "--root".as_ref(), root.as_os_str()]);
    assert!(second.contains("3 seen, 0 parsed, 3 unchanged"), "{second}");

    let json = cli(&[
        "--db".as_ref(),
        db,
        "report".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
        "--pricing".as_ref(),
        pricing.as_os_str(),
    ]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rows"][0]["name"], "claude-opus-5");
    assert_eq!(v["rows"][0]["est_cost"], 896.0);

    let doc = cli(&["--db".as_ref(), db, "doctor".as_ref(), "--pricing".as_ref(), pricing.as_os_str()]);
    assert!(doc.contains("3 files, 2 sessions, 10 turns, 8 tool calls"), "{doc}");
}

#[test]
fn ingest_refuses_a_database_inside_the_root() {
    let root = fixtures();
    let db = root.join("oops.db");
    let parsed = Cli::try_parse_from(["ledger".as_ref(), "--db".as_ref(), db.as_os_str(), "ingest".as_ref(), "--root".as_ref(), root.as_os_str()]).unwrap();
    assert!(run(parsed, &mut Vec::new()).is_err());
    assert!(!db.exists());
}

fn generate(root: &Path, sessions: usize, messages: usize) {
    for s in 0..sessions {
        let dir = root.join(format!("-proj-{}", s % 7));
        std::fs::create_dir_all(&dir).unwrap();
        let sid = format!("{s:08}-0000-4000-8000-000000000000");
        let mut text = String::new();
        for m in 0..messages {
            let model = ["claude-opus-5", "claude-sonnet-5-5"][(s + m) % 2];
            let ts = format!("2026-10-{:02}T{:02}:{:02}:00Z", 1 + s % 28, m / 60 % 24, m % 60);
            // Three lines per API message with growing output, then the tool result.
            for part in 1..=3 {
                let mut block = String::from("{\"type\":\"text\",\"text\":\"x\"}");
                if part == 3 {
                    block = format!("{{\"type\":\"tool_use\",\"id\":\"tu_{s}_{m}\",\"name\":\"Read\",\"input\":{{\"file_path\":\"/p/f{}.rs\"}}}}", m % 5);
                }
                writeln!(text, "{{\"type\":\"assistant\",\"uuid\":\"u{s}_{m}_{part}\",\"timestamp\":\"{ts}\",\"cwd\":\"/p\",\"sessionId\":\"{sid}\",\"version\":\"3.0.1\",\"message\":{{\"id\":\"m{s}_{m}\",\"model\":\"{model}\",\"content\":[{block}],\"usage\":{{\"input_tokens\":2,\"output_tokens\":{},\"cache_read_input_tokens\":10}}}}}}", part * 10).unwrap();
            }
            writeln!(text, "{{\"type\":\"user\",\"uuid\":\"r{s}_{m}\",\"timestamp\":\"{ts}\",\"sessionId\":\"{sid}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"tu_{s}_{m}\",\"content\":\"ok\"}}]}}}}").unwrap();
        }
        std::fs::write(dir.join(format!("{sid}.jsonl")), text).unwrap();
    }
}

#[test]
fn synthetic_scale_is_correct_and_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    generate(&root, 200, 50);
    let db = tmp.path().join("db/l.db");
    let t = Instant::now();
    let out = cli(&["--db".as_ref(), db.as_os_str(), "ingest".as_ref(), "--root".as_ref(), root.as_os_str(), "--json".as_ref()]);
    let elapsed = t.elapsed();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["turns"], 200 * 50, "three lines per message must collapse to one turn");
    assert_eq!(v["tool_calls"], 200 * 50);
    assert_eq!(v["sessions"], 200);
    assert!(elapsed.as_secs() < 60, "ingest took {elapsed:?}");

    let rep = cli(&["--db".as_ref(), db.as_os_str(), "report".as_ref(), "--format".as_ref(), "json".as_ref(), "--pricing".as_ref(), tmp.path().join("none.toml").as_os_str()]);
    let v: serde_json::Value = serde_json::from_str(&rep).unwrap();
    let out_tokens: i64 = v["rows"].as_array().unwrap().iter().map(|r| r["output_tokens"].as_i64().unwrap()).sum();
    assert_eq!(out_tokens, 200 * 50 * 30, "final (largest) usage per message, not the sum of its lines");
}
