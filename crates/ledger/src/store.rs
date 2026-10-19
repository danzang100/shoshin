//! SQLite working store. Rows hold hashes and metadata only (spec 003 section 6.1).
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::redact::Hasher;
use crate::schema::{ParsedFile, SCHEMA_VERSION};

const DB_VERSION: i64 = 2;

const MIGRATION_1: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    head_hash TEXT NOT NULL,
    lines INTEGER NOT NULL DEFAULT 0,
    bad_lines INTEGER NOT NULL DEFAULT 0,
    partial_tail INTEGER NOT NULL DEFAULT 0,
    ingested_at TEXT NOT NULL
);
CREATE TABLE sessions (
    file_id INTEGER PRIMARY KEY REFERENCES files(id),
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    source TEXT NOT NULL,
    source_version TEXT,
    project TEXT NOT NULL,
    project_name TEXT NOT NULL,
    cwd_hash TEXT,
    git_branch TEXT,
    started_at TEXT,
    ended_at TEXT,
    schema_version INTEGER NOT NULL
);
CREATE TABLE turns (
    turn_id TEXT PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    is_subagent INTEGER NOT NULL,
    parent_session_id TEXT,
    project TEXT NOT NULL,
    project_name TEXT NOT NULL,
    source_version TEXT,
    model TEXT NOT NULL,
    ts TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_5m_tokens INTEGER,
    cache_write_1h_tokens INTEGER,
    stop_reason TEXT,
    is_api_error INTEGER NOT NULL,
    is_aborted INTEGER NOT NULL,
    schema_version INTEGER NOT NULL
);
CREATE INDEX turns_file ON turns(file_id);
CREATE INDEX turns_ts ON turns(ts);
CREATE TABLE tool_calls (
    tool_use_id TEXT PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    ts TEXT,
    tool_name TEXT NOT NULL,
    args_hash TEXT NOT NULL,
    target_hash TEXT,
    result_status TEXT NOT NULL,
    permission TEXT NOT NULL,
    denial_kind TEXT,
    schema_version INTEGER NOT NULL
);
CREATE INDEX tool_calls_file ON tool_calls(file_id);
CREATE INDEX tool_calls_turn ON tool_calls(turn_id);
CREATE TABLE signals (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    session_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    ts TEXT,
    kind TEXT NOT NULL,
    schema_version INTEGER NOT NULL
);
CREATE INDEX signals_file ON signals(file_id);
CREATE TABLE unknown_types (
    file_id INTEGER NOT NULL REFERENCES files(id),
    type TEXT NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (file_id, type)
);
";

/// Version 1 built turn ids from the session id, so copies of a turn in resumed sessions were
/// counted twice. The ids cannot be recomputed in SQL, so drop the derived rows and forget the
/// file fingerprints; the next ingest rebuilds them. Rows whose transcript has since been
/// deleted are lost.
const MIGRATION_2: &str = "
DELETE FROM tool_calls;
DELETE FROM turns;
DELETE FROM signals;
DELETE FROM unknown_types;
DELETE FROM sessions;
DELETE FROM files;
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub path: String,
    pub size: i64,
    pub mtime_ns: i64,
    pub head_hash: String,
}

pub struct Store {
    conn: Connection,
    hasher: Hasher,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        Self::from_conn(Connection::open(path).with_context(|| format!("open db {}", path.display()))?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > DB_VERSION {
            bail!("database is version {v}, this build understands up to {DB_VERSION}");
        }
        if v < 1 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(MIGRATION_1)?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES ('salt', ?1)",
                params![Hasher::random_salt()],
            )?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
            tx.execute_batch("PRAGMA user_version = 1")?;
            tx.commit()?;
        }
        if v < 2 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(MIGRATION_2)?;
            tx.execute_batch("PRAGMA user_version = 2")?;
            tx.commit()?;
        }
        let salt: String = conn.query_row("SELECT value FROM meta WHERE key = 'salt'", [], |r| r.get(0))?;
        Ok(Self { conn, hasher: Hasher::new(salt) })
    }

    pub fn hasher(&self) -> &Hasher {
        &self.hasher
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn is_unchanged(&self, fp: &Fingerprint) -> Result<bool> {
        let row: Option<(i64, i64, String)> = self
            .conn
            .query_row(
                "SELECT size, mtime_ns, head_hash FROM files WHERE path = ?1",
                params![fp.path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row.is_some_and(|(s, m, h)| s == fp.size && m == fp.mtime_ns && h == fp.head_hash))
    }

    /// Replace everything stored for one file in a single transaction.
    pub fn replace_file(&mut self, fp: &Fingerprint, parsed: &ParsedFile) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        tx.execute(
            "INSERT INTO files(path, size, mtime_ns, head_hash, lines, bad_lines, partial_tail, ingested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(path) DO UPDATE SET size = ?2, mtime_ns = ?3, head_hash = ?4, lines = ?5,
                 bad_lines = ?6, partial_tail = ?7, ingested_at = ?8",
            params![
                fp.path,
                fp.size,
                fp.mtime_ns,
                fp.head_hash,
                parsed.stats.lines as i64,
                parsed.stats.bad_lines as i64,
                parsed.stats.partial_tail,
                now
            ],
        )?;
        let file_id: i64 = tx.query_row("SELECT id FROM files WHERE path = ?1", params![fp.path], |r| r.get(0))?;
        for table in ["sessions", "turns", "tool_calls", "signals", "unknown_types"] {
            tx.execute(&format!("DELETE FROM {table} WHERE file_id = ?1"), params![file_id])?;
        }
        let sv = SCHEMA_VERSION;
        if let Some(s) = &parsed.session {
            tx.execute(
                "INSERT INTO sessions VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    file_id,
                    s.session_id,
                    s.agent_id,
                    s.source,
                    s.source_version,
                    s.project,
                    s.project_name,
                    s.cwd_hash,
                    s.git_branch,
                    s.started_at,
                    s.ended_at,
                    sv
                ],
            )?;
        }
        {
            let mut st = tx.prepare(
                "INSERT OR IGNORE INTO turns VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            )?;
            for t in &parsed.turns {
                st.execute(params![
                    t.turn_id,
                    file_id,
                    t.session_id,
                    t.agent_id,
                    t.is_subagent,
                    t.parent_session_id,
                    t.project,
                    t.project_name,
                    t.source_version,
                    t.model,
                    t.ts,
                    t.input_tokens,
                    t.output_tokens,
                    t.cache_read_tokens,
                    t.cache_write_5m_tokens,
                    t.cache_write_1h_tokens,
                    t.stop_reason,
                    t.is_api_error,
                    t.is_aborted,
                    sv
                ])?;
            }
            let mut st = tx.prepare("INSERT OR IGNORE INTO tool_calls VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)")?;
            for c in &parsed.tool_calls {
                st.execute(params![
                    c.tool_use_id,
                    file_id,
                    c.turn_id,
                    c.session_id,
                    c.agent_id,
                    c.seq,
                    c.ts,
                    c.tool_name,
                    c.args_hash,
                    c.target_hash,
                    c.result_status.as_str(),
                    c.permission.as_str(),
                    c.denial_kind,
                    sv
                ])?;
            }
            let mut st = tx.prepare("INSERT INTO signals(file_id, session_id, agent_id, ts, kind, schema_version) VALUES (?1,?2,?3,?4,?5,?6)")?;
            for s in &parsed.signals {
                st.execute(params![file_id, s.session_id, s.agent_id, s.ts, s.kind.as_str(), sv])?;
            }
            let mut st = tx.prepare("INSERT INTO unknown_types VALUES (?1,?2,?3)")?;
            for (ty, n) in &parsed.stats.unknown_types {
                st.execute(params![file_id, ty, *n as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self, table: &str) -> Result<i64> {
        Ok(self.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    }
}
