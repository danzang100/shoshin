use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use ledger::adapter::claude_code::ClaudeCode;
use ledger::ingest::ingest;
use ledger::pricing::PriceTable;
use ledger::report::{Format, GroupBy, Report, ReportOptions, Row, build, parse_since};
use ledger::store::Store;

// $1/token input, $2 output, $0.1 cache read, $1.5 cache write 5m, $3 cache write 1h.
const PRICES: &str = r#"
version = "2026-10-01"
[[model]]
id = "claude-opus-5"
input_per_mtok = 1000000.0
output_per_mtok = 2000000.0
cache_read_per_mtok = 100000.0
cache_write_5m_per_mtok = 1500000.0
cache_write_1h_per_mtok = 3000000.0
[[model]]
id = "claude-sonnet-5-5"
input_per_mtok = 1000000.0
output_per_mtok = 2000000.0
cache_read_per_mtok = 100000.0
cache_write_5m_per_mtok = 1500000.0
cache_write_1h_per_mtok = 3000000.0
[[model]]
id = "claude-sonnet-5"
input_per_mtok = 1000000.0
output_per_mtok = 2000000.0
cache_read_per_mtok = 100000.0
cache_write_5m_per_mtok = 1500000.0
cache_write_1h_per_mtok = 3000000.0
"#;

fn setup() -> (Store, PriceTable) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_root");
    let mut store = Store::open_in_memory().unwrap();
    ingest(&mut store, &ClaudeCode, &root).unwrap();
    (store, PriceTable::parse(PRICES).unwrap())
}

fn opts(by: GroupBy) -> ReportOptions {
    ReportOptions { since: None, by, include_subagents: true, include_synthetic: false }
}

fn row<'a>(r: &'a Report, name: &str) -> &'a Row {
    r.rows
        .iter()
        .find(|x| x.name == name)
        .unwrap_or_else(|| panic!("no row {name}: {:?}", r.rows.iter().map(|x| &x.name).collect::<Vec<_>>()))
}

#[test]
fn by_model_totals_costs_and_rates() {
    let (store, prices) = setup();
    let r = build(&store, &prices, &opts(GroupBy::Model)).unwrap();
    let names: Vec<_> = r.rows.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["claude-opus-5", "claude-sonnet-5-5", "claude-sonnet-5 [subagent]", "claude-haiku-9"]);

    let o = row(&r, "claude-opus-5");
    assert_eq!((o.turns, o.input_tokens, o.output_tokens, o.cache_read_tokens, o.cache_write_tokens), (5, 22, 77, 1350, 240));
    assert_eq!(o.est_cost, Some(896.0));
    assert_eq!(o.tool_calls, 3);
    assert_eq!((o.tool_error_rate.value, o.tool_error_rate.n), (Some(1.0 / 3.0), 3));
    assert_eq!(o.denied, 1);
    assert_eq!((o.reread_rate.value, o.reread_rate.n), (Some(0.0), 1));

    let s = row(&r, "claude-sonnet-5-5");
    assert_eq!((s.turns, s.input_tokens, s.output_tokens, s.cache_read_tokens, s.cache_write_tokens), (2, 4, 38, 1120, 20));
    assert_eq!(s.est_cost, Some(252.0));
    assert_eq!((s.reread_rate.value, s.reread_rate.n), (Some(0.5), 2), "t2 re-reads, t5 follows an edit");
    assert_eq!((s.tool_error_rate.value, s.tool_error_rate.n), (Some(1.0 / 3.0), 3));

    let sub = row(&r, "claude-sonnet-5 [subagent]");
    assert_eq!(sub.est_cost, Some(192.0));
    assert_eq!((sub.reread_rate.value, sub.reread_rate.n), (Some(0.0), 1), "subagent state is separate");
    assert_eq!((sub.tool_error_rate.value, sub.tool_error_rate.n), (Some(0.5), 2));
    assert_eq!(sub.cache_hit_ratio, Some(0.0));

    let h = row(&r, "claude-haiku-9");
    assert_eq!(h.est_cost, None, "unpriced, never $0");
    assert_eq!(h.unpriced_turns, 1);
    assert_eq!((h.tool_error_rate.value, h.tool_error_rate.n), (None, 0));
}

#[test]
fn synthetic_and_subagent_switches() {
    let (store, prices) = setup();
    let mut o = opts(GroupBy::Model);
    o.include_synthetic = true;
    let r = build(&store, &prices, &o).unwrap();
    assert_eq!(row(&r, "<synthetic>").turns, 1);

    let mut o = opts(GroupBy::Model);
    o.include_subagents = false;
    let r = build(&store, &prices, &o).unwrap();
    assert!(r.rows.iter().all(|x| !x.name.contains("[subagent]")));
}

#[test]
fn totals_are_not_double_counted() {
    let (store, prices) = setup();
    let mut o = opts(GroupBy::Model);
    o.include_synthetic = true;
    let r = build(&store, &prices, &o).unwrap();
    let out: i64 = r.rows.iter().map(|x| x.output_tokens).sum();
    // alpha main 42+30+10+8+5, subagent 70, beta 11+9+1
    assert_eq!(out, 95 + 70 + 21);
}

#[test]
fn by_repo_and_by_session() {
    let (store, prices) = setup();
    let r = build(&store, &prices, &opts(GroupBy::Repo)).unwrap();
    assert_eq!(row(&r, "alpha").turns, 6);
    assert_eq!(row(&r, "beta").turns, 3);
    let r = build(&store, &prices, &opts(GroupBy::Session)).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.total_groups, 2);
}

#[test]
fn since_filters_by_turn_time() {
    let (store, prices) = setup();
    let now = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
    let mut o = opts(GroupBy::Repo);
    o.since = Some(parse_since("10d", now).unwrap());
    let r = build(&store, &prices, &o).unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].name, "beta");
    o.since = Some(parse_since("2026-10-03", now).unwrap());
    assert!(build(&store, &prices, &o).unwrap().rows.is_empty());
    assert_eq!(parse_since("1w", now).unwrap(), Utc.with_ymd_and_hms(2026, 10, 5, 0, 0, 0).unwrap());
    assert!(parse_since("soon", now).is_err());
}

#[test]
fn output_is_deterministic_and_renders_all_formats() {
    let (store, prices) = setup();
    let a = build(&store, &prices, &opts(GroupBy::Model)).unwrap();
    let b = build(&store, &prices, &opts(GroupBy::Model)).unwrap();
    for f in [Format::Table, Format::Json, Format::Md] {
        assert_eq!(a.render(f).unwrap(), b.render(f).unwrap());
    }
    let json: serde_json::Value = serde_json::from_str(&a.render(Format::Json).unwrap()).unwrap();
    assert_eq!(json["rows"][0]["name"], "claude-opus-5");
    assert_eq!(json["rows"][0]["tool_error_rate"]["n"], 3);
}

#[test]
fn table_golden() {
    let (store, prices) = setup();
    let t = build(&store, &prices, &opts(GroupBy::Model)).unwrap().render(Format::Table).unwrap();
    let expected = "\
Model Ledger report: by model | pricing 2026-10-01 (USD) | costs are estimates

group                       turns  in  out  cache_r  cache_w  est_cost  tool_calls     tool_err  denied       reread  cache_hit
claude-opus-5                   5  22   77    1,350      240   $896.00           3  33.3% (n=3)       1   0.0% (n=1)      83.7%
claude-sonnet-5-5               2   4   38    1,120       20   $252.00           3  33.3% (n=3)       0  50.0% (n=2)      97.9%
claude-sonnet-5 [subagent]      1   7   70        0       30   $192.00           2  50.0% (n=2)       0   0.0% (n=1)       0.0%
claude-haiku-9                  1   1    1        0        0  unpriced           0          n/a       0          n/a       0.0%
";
    assert_eq!(t, expected);
}
