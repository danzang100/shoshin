use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use ledger::adapter::claude_code::ClaudeCode;
use ledger::doctor::build;
use ledger::ingest::ingest;
use ledger::pricing::PriceTable;
use ledger::store::Store;

fn setup() -> Store {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_root");
    let mut store = Store::open_in_memory().unwrap();
    ingest(&mut store, &ClaudeCode, &root).unwrap();
    store
}

#[test]
fn doctor_golden() {
    let store = setup();
    let now = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
    let r = build(&store, &PriceTable::builtin(), now).unwrap();
    let expected = "\
Model Ledger doctor

data: 3 files, 2 sessions, 10 turns, 8 tool calls
parse: 1 malformed lines, 1 file(s) ending in a partial line
range: 2026-10-01T10:00:00.000Z .. 2026-10-02T09:00:04.000Z
other line types (ignored in M0): system=1, weird-new-type=1

field presence by source version (non-synthetic turns)
version        turns  usage  cache_read  cache_write  stop_reason
2.9.0              3   100%         33%          33%          67%
3.0.1              6   100%        100%         100%         100%

pricing: version 2026-10-12, unpriced: claude-haiku-9, claude-opus-5, claude-sonnet-5, claude-sonnet-5-5

warnings
- 1 malformed line(s) skipped; if this grows after a harness update the format may have changed
- 4 model(s) have no complete price and are reported as unpriced: claude-haiku-9, claude-opus-5, claude-sonnet-5, claude-sonnet-5-5
";
    assert_eq!(r.render(), expected);
}

#[test]
fn retention_and_stale_pricing_warnings() {
    let store = setup();
    let now = Utc.with_ymd_and_hms(2027, 1, 15, 0, 0, 0).unwrap();
    let r = build(&store, &PriceTable::builtin(), now).unwrap();
    let text = r.render();
    assert!(text.contains("pricing table is 95 days old"), "{text}");
    assert!(text.contains("oldest ingested session started 105 days ago"), "{text}");
    assert!(text.contains("cleanupPeriodDays"));
}

#[test]
fn empty_database_does_not_fail() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
    let r = build(&store, &PriceTable::builtin(), now).unwrap();
    assert_eq!(r.turns, 0);
    assert!(r.render().contains("range: n/a .. n/a"));
}
