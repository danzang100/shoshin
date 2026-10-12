//! discover -> fingerprint -> parse -> store. Transcripts are only ever read.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::adapter::{Adapter, DiscoveredFile};
use crate::store::{Fingerprint, Store};

#[derive(Debug, Default, Serialize)]
pub struct IngestSummary {
    pub files_seen: u64,
    pub files_skipped: u64,
    pub files_parsed: u64,
    /// Files that could not be read this run (kept going).
    pub files_failed: u64,
    pub bad_lines: u64,
    pub partial_tails: u64,
    /// Unknown line types in the files parsed this run.
    pub unknown_types: BTreeMap<String, u64>,
    /// Totals in the database after this run.
    pub sessions: i64,
    pub turns: i64,
    pub tool_calls: i64,
    pub elapsed_ms: u128,
}

fn fingerprint(file: &DiscoveredFile, store: &Store) -> Result<Fingerprint> {
    let meta = std::fs::metadata(&file.path)?;
    let mtime_ns = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let mut head = Vec::with_capacity(4096);
    File::open(&file.path)?.take(4096).read_to_end(&mut head)?;
    Ok(Fingerprint {
        path: file.path.to_string_lossy().into_owned(),
        size: meta.len() as i64,
        mtime_ns,
        head_hash: store.hasher().hash_bytes(&head),
    })
}

/// Refuse a database that lives under the transcript root, so ingest can never write there.
pub fn ensure_db_outside_root(db: &Path, root: &Path) -> Result<()> {
    let root = root.canonicalize().with_context(|| format!("transcript root {}", root.display()))?;
    let parent = db.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // The parent may not exist yet; walk up to the nearest existing ancestor.
    let mut anc = parent.to_path_buf();
    while !anc.exists() {
        match anc.parent() {
            Some(p) => anc = p.to_path_buf(),
            None => break,
        }
    }
    let anc = anc.canonicalize().unwrap_or(anc);
    if anc.starts_with(&root) {
        bail!("database path {} is inside the transcript root {}; refusing", db.display(), root.display());
    }
    Ok(())
}

pub fn ingest(store: &mut Store, adapter: &dyn Adapter, root: &Path) -> Result<IngestSummary> {
    let start = Instant::now();
    if !root.is_dir() {
        bail!("transcript root {} does not exist or is not a directory", root.display());
    }
    let mut s = IngestSummary::default();
    for file in adapter.discover(root)? {
        s.files_seen += 1;
        let fp = match fingerprint(&file, store) {
            Ok(fp) => fp,
            Err(_) => {
                s.files_failed += 1;
                continue;
            }
        };
        if store.is_unchanged(&fp)? {
            s.files_skipped += 1;
            continue;
        }
        let parsed = match adapter.parse(&file, store.hasher()) {
            Ok(p) => p,
            Err(_) => {
                s.files_failed += 1;
                continue;
            }
        };
        s.bad_lines += parsed.stats.bad_lines;
        s.partial_tails += parsed.stats.partial_tail as u64;
        for (k, v) in &parsed.stats.unknown_types {
            *s.unknown_types.entry(k.clone()).or_default() += v;
        }
        store.replace_file(&fp, &parsed)?;
        s.files_parsed += 1;
    }
    s.sessions = store.conn().query_row(
        "SELECT COUNT(*) FROM (SELECT session_id FROM turns UNION SELECT session_id FROM sessions)",
        [],
        |r| r.get(0),
    )?;
    s.turns = store.count("turns")?;
    s.tool_calls = store.count("tool_calls")?;
    s.elapsed_ms = start.elapsed().as_millis();
    Ok(s)
}

impl IngestSummary {
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "files: {} seen, {} parsed, {} unchanged, {} failed\n",
            self.files_seen, self.files_parsed, self.files_skipped, self.files_failed
        ));
        o.push_str(&format!(
            "database: {} sessions, {} turns, {} tool calls\n",
            self.sessions, self.turns, self.tool_calls
        ));
        o.push_str(&format!("this run: {} bad lines, {} partial tails\n", self.bad_lines, self.partial_tails));
        if !self.unknown_types.is_empty() {
            let mut v: Vec<_> = self.unknown_types.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let top: Vec<String> = v.iter().take(10).map(|(k, n)| format!("{k}={n}")).collect();
            o.push_str(&format!("unknown line types (top 10): {}\n", top.join(", ")));
        }
        o.push_str(&format!("elapsed: {} ms\n", self.elapsed_ms));
        o
    }
}
