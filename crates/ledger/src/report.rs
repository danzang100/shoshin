//! Descriptive per-group report. No comparisons or rankings of models in M0 (spec 003 section 9).
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use serde::Serialize;

use crate::pricing::{PriceTable, Tokens};
use crate::schema::SYNTHETIC_MODEL;
use crate::store::Store;

const SESSION_LIMIT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Model,
    Repo,
    Session,
}

impl GroupBy {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "model" => Ok(Self::Model),
            "repo" => Ok(Self::Repo),
            "session" => Ok(Self::Session),
            other => bail!("unknown --by {other:?} (model, repo, session)"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Repo => "repo",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
    Md,
}

impl Format {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "table" => Ok(Self::Table),
            "json" => Ok(Self::Json),
            "md" => Ok(Self::Md),
            other => bail!("unknown --format {other:?} (table, json, md)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReportOptions {
    pub since: Option<DateTime<Utc>>,
    pub by: GroupBy,
    pub include_subagents: bool,
    pub include_synthetic: bool,
}

/// `Nd` (days ago), `Nw` (weeks ago) or `YYYY-MM-DD` (UTC midnight).
pub fn parse_since(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return Ok(now - chrono::Duration::days(n));
    }
    if let Some(n) = s.strip_suffix('w').and_then(|n| n.parse::<i64>().ok()) {
        return Ok(now - chrono::Duration::weeks(n));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()));
    }
    bail!("cannot parse --since {s:?} (use 30d, 4w or YYYY-MM-DD)")
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rate {
    pub value: Option<f64>,
    pub n: u64,
}

impl Rate {
    fn new(num: u64, den: u64) -> Self {
        Self { value: (den > 0).then(|| num as f64 / den as f64), n: den }
    }

    fn render(&self) -> String {
        match self.value {
            Some(v) => format!("{:.1}% (n={})", v * 100.0, self.n),
            None => "n/a".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Row {
    pub name: String,
    pub turns: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    /// Sum over turns that could be priced; `None` when none could.
    pub est_cost: Option<f64>,
    pub unpriced_turns: u64,
    pub tool_calls: u64,
    pub tool_error_rate: Rate,
    pub denied: u64,
    pub reread_rate: Rate,
    pub cache_hit_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub by: String,
    pub since: Option<String>,
    pub pricing_version: String,
    pub currency: String,
    pub rows: Vec<Row>,
    /// Total groups before any row limit.
    pub total_groups: usize,
}

#[derive(Default)]
struct Acc {
    turns: u64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: f64,
    priced_turns: u64,
    unpriced_turns: u64,
    calls: u64,
    ok: u64,
    err: u64,
    denied: u64,
    reads: u64,
    rereads: u64,
}

struct TurnInfo {
    group: String,
}

fn ts_string(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn build(store: &Store, pricing: &PriceTable, opts: &ReportOptions) -> Result<Report> {
    let cutoff = opts.since.map(ts_string);
    let mut groups: BTreeMap<String, Acc> = BTreeMap::new();
    let mut turn_info: HashMap<String, Option<TurnInfo>> = HashMap::new();

    let mut st = store.conn().prepare(
        "SELECT turn_id, session_id, is_subagent, project_name, model, ts,
                input_tokens, output_tokens, cache_read_tokens, cache_write_5m_tokens, cache_write_1h_tokens
         FROM turns ORDER BY turn_id",
    )?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let turn_id: String = r.get(0)?;
        let session_id: String = r.get(1)?;
        let is_sub: bool = r.get(2)?;
        let project_name: String = r.get(3)?;
        let model: String = r.get(4)?;
        let ts: Option<String> = r.get(5)?;
        let included = (opts.include_synthetic || model != SYNTHETIC_MODEL)
            && (opts.include_subagents || !is_sub)
            && match (&cutoff, &ts) {
                (None, _) => true,
                (Some(c), Some(t)) => t >= c,
                (Some(_), None) => false,
            };
        if !included {
            turn_info.insert(turn_id, None);
            continue;
        }
        let group = match opts.by {
            GroupBy::Model if is_sub => format!("{model} [subagent]"),
            GroupBy::Model => model.clone(),
            GroupBy::Repo => project_name,
            GroupBy::Session => session_id,
        };
        let tokens = Tokens {
            input: r.get(6)?,
            output: r.get(7)?,
            cache_read: r.get(8)?,
            cache_write_5m: r.get(9)?,
            cache_write_1h: r.get(10)?,
        };
        let a = groups.entry(group.clone()).or_default();
        a.turns += 1;
        a.input += tokens.input.unwrap_or(0);
        a.output += tokens.output.unwrap_or(0);
        a.cache_read += tokens.cache_read.unwrap_or(0);
        a.cache_write += tokens.cache_write_5m.unwrap_or(0) + tokens.cache_write_1h.unwrap_or(0);
        match pricing.cost(&model, tokens) {
            Some(c) => {
                a.cost += c;
                a.priced_turns += 1;
            }
            None => a.unpriced_turns += 1,
        }
        turn_info.insert(turn_id, Some(TurnInfo { group }));
    }
    drop(rows);
    drop(st);

    // Tool calls in file order per (session, agent); re-read state must see excluded turns too.
    let mut st = store.conn().prepare(
        "SELECT turn_id, session_id, agent_id, tool_name, target_hash, result_status, permission
         FROM tool_calls ORDER BY session_id, agent_id, file_id, seq",
    )?;
    let mut rows = st.query([])?;
    let mut seen: HashMap<(String, String), HashSet<String>> = HashMap::new();
    while let Some(r) = rows.next()? {
        let turn_id: String = r.get(0)?;
        let session_id: String = r.get(1)?;
        let agent_id: String = r.get(2)?;
        let tool: String = r.get(3)?;
        let target: Option<String> = r.get(4)?;
        let status: String = r.get(5)?;
        let permission: String = r.get(6)?;

        let mut is_reread = None;
        if let Some(t) = &target {
            let set = seen.entry((session_id, agent_id)).or_default();
            match tool.as_str() {
                "Read" => is_reread = Some(!set.insert(t.clone())),
                "Write" | "Edit" | "NotebookEdit" => {
                    set.remove(t);
                }
                _ => {}
            }
        }
        let Some(Some(info)) = turn_info.get(&turn_id) else { continue };
        let a = groups.entry(info.group.clone()).or_default();
        a.calls += 1;
        match status.as_str() {
            "ok" => a.ok += 1,
            "error" => a.err += 1,
            _ => {}
        }
        if permission == "denied" {
            a.denied += 1;
        }
        if let Some(rr) = is_reread {
            a.reads += 1;
            a.rereads += rr as u64;
        }
    }

    let mut out: Vec<Row> = groups
        .into_iter()
        .map(|(name, a)| {
            let denom = a.input + a.cache_read + a.cache_write;
            Row {
                name,
                turns: a.turns,
                input_tokens: a.input,
                output_tokens: a.output,
                cache_read_tokens: a.cache_read,
                cache_write_tokens: a.cache_write,
                est_cost: (a.priced_turns > 0).then_some(a.cost),
                unpriced_turns: a.unpriced_turns,
                tool_calls: a.calls,
                tool_error_rate: Rate::new(a.err, a.ok + a.err),
                denied: a.denied,
                reread_rate: Rate::new(a.rereads, a.reads),
                cache_hit_ratio: (denom > 0).then(|| a.cache_read as f64 / denom as f64),
            }
        })
        .collect();
    // Cost descending (unpriced last), then total tokens, then name: deterministic.
    out.sort_by(|a, b| {
        let ca = a.est_cost.unwrap_or(f64::NEG_INFINITY);
        let cb = b.est_cost.unwrap_or(f64::NEG_INFINITY);
        let ta = a.input_tokens + a.output_tokens + a.cache_read_tokens + a.cache_write_tokens;
        let tb = b.input_tokens + b.output_tokens + b.cache_read_tokens + b.cache_write_tokens;
        cb.total_cmp(&ca).then(tb.cmp(&ta)).then(a.name.cmp(&b.name))
    });
    let total_groups = out.len();
    if opts.by == GroupBy::Session {
        out.truncate(SESSION_LIMIT);
    }
    Ok(Report {
        by: opts.by.label().to_string(),
        since: opts.since.map(ts_string),
        pricing_version: pricing.version.clone(),
        currency: pricing.currency.clone(),
        rows: out,
        total_groups,
    })
}

fn group_digits(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn money(r: &Row) -> String {
    match r.est_cost {
        None => "unpriced".to_string(),
        Some(c) => {
            let base = if c > 0.0 && c < 0.01 { "<$0.01".to_string() } else { format!("${c:.2}") };
            if r.unpriced_turns > 0 { format!("{base} (+{} unpriced)", r.unpriced_turns) } else { base }
        }
    }
}

fn cells(r: &Row) -> Vec<String> {
    vec![
        r.name.clone(),
        group_digits(r.turns as i64),
        group_digits(r.input_tokens),
        group_digits(r.output_tokens),
        group_digits(r.cache_read_tokens),
        group_digits(r.cache_write_tokens),
        money(r),
        group_digits(r.tool_calls as i64),
        r.tool_error_rate.render(),
        group_digits(r.denied as i64),
        r.reread_rate.render(),
        r.cache_hit_ratio.map_or("n/a".to_string(), |v| format!("{:.1}%", v * 100.0)),
    ]
}

const HEADERS: [&str; 12] = [
    "group", "turns", "in", "out", "cache_r", "cache_w", "est_cost", "tool_calls", "tool_err", "denied", "reread", "cache_hit",
];

impl Report {
    fn heading(&self) -> String {
        format!(
            "Model Ledger report: by {}{} | pricing {} ({}) | costs are estimates",
            self.by,
            self.since.as_ref().map(|s| format!(", since {s}")).unwrap_or_default(),
            self.pricing_version,
            self.currency
        )
    }

    fn footer(&self) -> Option<String> {
        (self.total_groups > self.rows.len())
            .then(|| format!("showing {} of {} groups", self.rows.len(), self.total_groups))
    }

    pub fn render(&self, format: Format) -> Result<String> {
        Ok(match format {
            Format::Json => serde_json::to_string_pretty(self)? + "\n",
            Format::Table => self.render_table(),
            Format::Md => self.render_md(),
        })
    }

    fn render_table(&self) -> String {
        let body: Vec<Vec<String>> = self.rows.iter().map(cells).collect();
        let mut w: Vec<usize> = HEADERS.iter().map(|h| h.len()).collect();
        for row in &body {
            for (i, c) in row.iter().enumerate() {
                w[i] = w[i].max(c.chars().count());
            }
        }
        let fmt = |row: &[String]| {
            let mut line = String::new();
            for (i, c) in row.iter().enumerate() {
                if i > 0 {
                    line.push_str("  ");
                }
                let pad = w[i] - c.chars().count();
                if i == 0 {
                    line.push_str(c);
                    line.push_str(&" ".repeat(pad));
                } else {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(c);
                }
            }
            line.trim_end().to_string()
        };
        let head: Vec<String> = HEADERS.iter().map(|s| s.to_string()).collect();
        let mut out = format!("{}\n\n{}\n", self.heading(), fmt(&head));
        for row in &body {
            out.push_str(&fmt(row));
            out.push('\n');
        }
        if let Some(f) = self.footer() {
            out.push_str(&format!("\n{f}\n"));
        }
        out
    }

    fn render_md(&self) -> String {
        let mut out = format!("**{}**\n\n| {} |\n|{}|\n", self.heading(), HEADERS.join(" | "), " --- |".repeat(HEADERS.len()));
        for r in &self.rows {
            out.push_str(&format!("| {} |\n", cells(r).join(" | ")));
        }
        if let Some(f) = self.footer() {
            out.push_str(&format!("\n{f}\n"));
        }
        out
    }
}
