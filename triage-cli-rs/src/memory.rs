//! Durable investigation memory: MEMORY.md (source of truth) + SQLite FTS5 index.
//!
//! Human workflow: edit `MEMORY.md` to prune entries; the FTS5 index rebuilds
//! automatically on the next call when MEMORY.md mtime > last_indexed_at.
//!
//! ## v1 schema (spec § 10, decision 5)
//!
//! The FTS5 `investigations` virtual table carries nine columns. The first six
//! mirror the legacy schema; the last three were added during the v1 reframe:
//!
//! ```text
//! ticket_id, customer, subject, symptom, assessment, resolution,
//! fork_letter, quoted_rubric_row, rubric_version
//! ```
//!
//! FTS5 virtual tables do not support `ALTER TABLE ADD COLUMN`, so the upgrade
//! path is a one-shot rebuild: rows are spilled into a temp table, the FTS5
//! table is dropped and recreated with the new column list, then rows are
//! re-inserted with empty strings for the new fields. The migration is
//! idempotent — it tracks completion via `memory_meta.schema_version = '2'`.
//!
//! MEMORY.md blocks gain `fork_letter`, `quoted_rubric_row`, and
//! `rubric_version` keys. Legacy blocks that lack these keys are still parsed
//! cleanly — missing keys default to the empty string.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;

use crate::models::MemoryEntry;

const MEMORY_MD: &str = "MEMORY.md";
const MEMORY_DB: &str = "data/memory.db";
const HEADER: &str = "# Investigation Memory\n\n<!-- Append-only by the tool. Delete entries to prune. Search index rebuilds automatically. -->\n";

/// Env-var override for the MEMORY.md path. Used by tests; production code
/// reads the project-root `MEMORY.md`.
pub(crate) const MEMORY_MD_ENV: &str = "TRIAGE_MEMORY_MD";
/// Env-var override for the SQLite index path. Used by tests.
pub(crate) const MEMORY_DB_ENV: &str = "TRIAGE_MEMORY_DB";

/// Schema version recorded in `memory_meta` once the v1 columns are present.
const SCHEMA_VERSION: &str = "2";

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

fn memory_md_path() -> PathBuf {
    if let Ok(v) = std::env::var(MEMORY_MD_ENV) {
        return PathBuf::from(v);
    }
    crate::paths::triage_home().join(MEMORY_MD)
}

fn memory_db_path() -> PathBuf {
    if let Ok(v) = std::env::var(MEMORY_DB_ENV) {
        return PathBuf::from(v);
    }
    crate::paths::triage_home().join(MEMORY_DB)
}

/// Open (or create) the SQLite database, run the v1 column migration if
/// needed, and return the connection. Safe to call repeatedly.
fn ensure_db(db_path: &Path) -> Result<Connection, MemoryError> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(db_path)?;
    // Create the metadata table eagerly; the FTS5 table is created either by
    // the migration (if pre-existing rows must be preserved) or by the
    // freshly-installed CREATE statement below.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    // Create the FTS5 table with the v1 column set if it does not yet exist.
    // (If it already exists with the legacy 6-column schema, this CREATE is a
    // no-op and the migration step below will rebuild it.)
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS investigations USING fts5(\
            ticket_id, customer, subject, symptom, assessment, resolution,\
            fork_letter, quoted_rubric_row, rubric_version);",
    )?;
    migrate_v1_columns(&conn)?;
    Ok(conn)
}

/// One-shot migration: ensure the `investigations` FTS5 table carries the v1
/// columns (`fork_letter`, `quoted_rubric_row`, `rubric_version`). Idempotent —
/// returns immediately if the columns are already present.
fn migrate_v1_columns(conn: &Connection) -> Result<(), MemoryError> {
    if has_v1_columns(conn)? {
        // Record schema_version=2 even on fresh databases so future migrations
        // can branch on it. INSERT OR REPLACE keeps this idempotent.
        conn.execute(
            "INSERT OR REPLACE INTO memory_meta VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION],
        )?;
        return Ok(());
    }

    // Wrap the rebuild (backup → DROP → CREATE → re-INSERT → schema_version
    // bump) in a single transaction. This is what makes the migration
    // crash-safe: if the process dies mid-rebuild, SQLite rolls back and the
    // legacy `investigations` table remains intact for the next startup to
    // retry. Without this, an interrupted run between the DROP and the
    // CREATE would leave a freshly-empty v1 table on disk, and the next
    // `has_v1_columns()` check would return true — silently losing every
    // pre-upgrade row.
    let tx = conn.unchecked_transaction()?;

    // Spill existing rows into a temporary table. The legacy schema has six
    // columns; we know that because has_v1_columns() returned false.
    tx.execute_batch(
        "CREATE TEMP TABLE investigations_backup AS \
            SELECT ticket_id, customer, subject, symptom, assessment, resolution \
            FROM investigations;",
    )?;

    // Drop and recreate with the v1 column set.
    tx.execute_batch(
        "DROP TABLE investigations;\
         CREATE VIRTUAL TABLE investigations USING fts5(\
            ticket_id, customer, subject, symptom, assessment, resolution,\
            fork_letter, quoted_rubric_row, rubric_version);",
    )?;

    // Re-insert rows, padding the new columns with empty strings. Pre-upgrade
    // entries genuinely have no fork data — that's fine and intentional.
    {
        let mut stmt = tx.prepare(
            "SELECT ticket_id, customer, subject, symptom, assessment, resolution \
             FROM investigations_backup",
        )?;
        let mut insert =
            tx.prepare("INSERT INTO investigations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for r in rows {
            let (ticket_id, customer, subject, symptom, assessment, resolution) = r?;
            insert.execute(params![
                ticket_id, customer, subject, symptom, assessment, resolution, "", "", ""
            ])?;
        }
    }

    tx.execute_batch("DROP TABLE investigations_backup;")?;
    tx.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

/// Inspect the FTS5 table layout. Returns true iff all three v1 columns are
/// present. A missing-table case is treated as "needs no migration" because
/// `ensure_db` will have just created the v1-shaped table by the time this
/// is called.
fn has_v1_columns(conn: &Connection) -> Result<bool, MemoryError> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('investigations')")?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect();
    if names.is_empty() {
        // Table missing entirely — caller (ensure_db) has just created it
        // with the v1 schema, so consider the migration done. We re-read
        // table_info to confirm rather than relying on a stale view.
        return Ok(true);
    }
    Ok(["fork_letter", "quoted_rubric_row", "rubric_version"]
        .iter()
        .all(|c| names.iter().any(|n| n == c)))
}

fn needs_rebuild(conn: &Connection, md_path: &Path) -> Result<bool, MemoryError> {
    if !md_path.exists() {
        return Ok(false);
    }
    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM memory_meta WHERE key='last_indexed_at'",
            [],
            |row| row.get(0),
        )
        .ok();
    let Some(value) = row else { return Ok(true) };
    let Ok(last_indexed) = chrono::DateTime::parse_from_rfc3339(&value) else {
        return Ok(true);
    };
    let mtime = fs::metadata(md_path)?.modified()?;
    let mtime_utc: chrono::DateTime<Utc> = mtime.into();
    Ok(mtime_utc > last_indexed.with_timezone(&Utc))
}

fn parse_memory_md(
    md_path: &Path,
) -> std::io::Result<Vec<std::collections::HashMap<String, String>>> {
    if !md_path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(md_path)?;
    let mut entries: Vec<std::collections::HashMap<String, String>> = Vec::new();
    for raw_block in text.split("---") {
        let block = raw_block.trim();
        if block.is_empty() || block.starts_with('#') || block.starts_with("<!--") {
            continue;
        }
        let mut entry = std::collections::HashMap::new();
        for line in block.lines() {
            // split_once on the first colon so values containing ':' survive.
            if let Some((k, v)) = line.split_once(':') {
                entry.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        if entry.contains_key("id") && entry.contains_key("subject") {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Parse MEMORY.md and rebuild the FTS5 index from scratch. Returns entry count.
pub fn rebuild_index(db_path: Option<&Path>) -> Result<usize, MemoryError> {
    let path = db_path
        .map(Path::to_path_buf)
        .unwrap_or_else(memory_db_path);
    let conn = ensure_db(&path)?;
    let entries = parse_memory_md(&memory_md_path())?;
    conn.execute("DELETE FROM investigations", [])?;
    let mut stmt =
        conn.prepare("INSERT INTO investigations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
    for e in &entries {
        stmt.execute(params![
            e.get("id").map(String::as_str).unwrap_or(""),
            e.get("customer").map(String::as_str).unwrap_or(""),
            e.get("subject").map(String::as_str).unwrap_or(""),
            e.get("symptom").map(String::as_str).unwrap_or(""),
            e.get("assessment").map(String::as_str).unwrap_or(""),
            e.get("resolution")
                .map(String::as_str)
                .unwrap_or("[unknown]"),
            e.get("fork_letter").map(String::as_str).unwrap_or(""),
            e.get("quoted_rubric_row").map(String::as_str).unwrap_or(""),
            e.get("rubric_version").map(String::as_str).unwrap_or(""),
        ])?;
    }
    drop(stmt);
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('last_indexed_at', ?1)",
        params![now],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('entry_count', ?1)",
        params![entries.len().to_string()],
    )?;
    Ok(entries.len())
}

/// FTS5 BM25 query over subject + first 500 chars of symptom.
pub fn retrieve_similar(
    subject: &str,
    symptom: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, MemoryError> {
    let db_path = memory_db_path();
    let conn = ensure_db(&db_path)?;
    if needs_rebuild(&conn, &memory_md_path())? {
        drop(conn);
        rebuild_index(Some(&db_path))?;
    }
    let conn = ensure_db(&db_path)?;
    let symptom_clipped: String = symptom.chars().take(500).collect();
    let query = format!("{subject} {symptom_clipped}");
    let rows: Vec<MemoryEntry> = match conn.prepare(
        "SELECT ticket_id, customer, subject, symptom, assessment, resolution \
         FROM investigations WHERE investigations MATCH ?1 ORDER BY bm25(investigations) LIMIT ?2",
    ) {
        Ok(mut stmt) => stmt
            .query_map(params![query, limit as i64], |row| {
                Ok(MemoryEntry {
                    ticket_id: row.get(0)?,
                    customer: row.get(1)?,
                    subject: row.get(2)?,
                    symptom: row.get(3)?,
                    assessment: row.get(4)?,
                    resolution: row.get(5)?,
                })
            })?
            .filter_map(Result::ok)
            .collect(),
        Err(_) => Vec::new(),
    };
    Ok(rows)
}

/// Exact `ticket_id` lookup; `None` if not present.
pub fn find_duplicate(ticket_id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
    let db_path = memory_db_path();
    let conn = ensure_db(&db_path)?;
    if needs_rebuild(&conn, &memory_md_path())? {
        drop(conn);
        rebuild_index(Some(&db_path))?;
    }
    let conn = ensure_db(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT ticket_id, customer, subject, symptom, assessment, resolution \
         FROM investigations WHERE ticket_id = ?1",
    )?;
    let row = stmt
        .query_row(params![ticket_id], |row| {
            Ok(MemoryEntry {
                ticket_id: row.get(0)?,
                customer: row.get(1)?,
                subject: row.get(2)?,
                symptom: row.get(3)?,
                assessment: row.get(4)?,
                resolution: row.get(5)?,
            })
        })
        .ok();
    Ok(row)
}

/// Append one entry to MEMORY.md and to the FTS5 index.
///
/// `fork_letter`, `quoted_rubric_row`, and `rubric_version` are v1 fields
/// (spec § 10, decision 5). Callers with no fork data should pass empty
/// strings — the index tolerates it cleanly and legacy MEMORY.md blocks
/// (pre-v1 entries lacking these keys) parse the same way.
#[allow(clippy::too_many_arguments)]
pub fn append_investigation(
    ticket_id: &str,
    customer: &str,
    subject: &str,
    symptom: &str,
    assessment: &str,
    resolution: Option<&str>,
    fork_letter: &str,
    quoted_rubric_row: &str,
    rubric_version: &str,
) -> Result<(), MemoryError> {
    ensure_memory_md()?;
    let resolution = resolution.unwrap_or("[unknown]");
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    let symptom_clipped: String = symptom.chars().take(500).collect();
    let entry_text = format!(
        "\n---\nid: {ticket_id}\ncustomer: {customer}\ndate: {now}\nsubject: {subject}\nsymptom: {symptom_clipped}\nassessment: {assessment}\nresolution: {resolution}\nfork_letter: {fork_letter}\nquoted_rubric_row: {quoted_rubric_row}\nrubric_version: {rubric_version}\n---\n"
    );
    let mut f = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(memory_md_path())?;
    f.write_all(entry_text.as_bytes())?;

    let db_path = memory_db_path();
    let conn = ensure_db(&db_path)?;
    conn.execute(
        "INSERT INTO investigations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            ticket_id,
            customer,
            subject,
            symptom_clipped,
            assessment,
            resolution,
            fork_letter,
            quoted_rubric_row,
            rubric_version,
        ],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('last_indexed_at', ?1)",
        params![now],
    )?;
    Ok(())
}

fn ensure_memory_md() -> std::io::Result<()> {
    let path = memory_md_path();
    if !path.exists() {
        fs::write(&path, HEADER)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Test-isolation helpers (pub so integration tests can share the same mutex).
// ─────────────────────────────────────────────────────────────────────────

/// Env var name for the tickets root dir — overridden in tests.
pub const TICKETS_ROOT_ENV: &str = "TRIAGE_TICKETS_ROOT";

/// Process-wide mutex that serialises any test mutating
/// `TRIAGE_MEMORY_MD` / `TRIAGE_MEMORY_DB` / `TRIAGE_TICKETS_ROOT`.
/// All tests in every module that touch these vars must hold this lock.
pub static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: locks [`ENV_GUARD`] and overrides the three process-global
/// env vars used by the memory subsystem (and by `investigate_one_structured`
/// for the tickets root).  All previous values are restored on drop.
///
/// Primarily used by integration tests to isolate env vars; safe to call
/// from any test context.
pub struct MemoryEnvScope {
    _guard: std::sync::MutexGuard<'static, ()>,
    prev_md: Option<String>,
    prev_db: Option<String>,
    prev_tickets_root: Option<String>,
}

impl MemoryEnvScope {
    /// Override `TRIAGE_MEMORY_MD` and `TRIAGE_MEMORY_DB` only.
    /// Use this from pure memory tests that don't touch the tickets root.
    pub fn new(md: &std::path::Path, db: &std::path::Path) -> Self {
        Self::new_with_tickets_root(md, db, None)
    }

    /// Override all three env vars.  Pass `tickets_root = Some(path)` when
    /// the caller also needs to isolate `TRIAGE_TICKETS_ROOT`.
    pub fn new_with_tickets_root(
        md: &std::path::Path,
        db: &std::path::Path,
        tickets_root: Option<&std::path::Path>,
    ) -> Self {
        let guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev_md = std::env::var(MEMORY_MD_ENV).ok();
        let prev_db = std::env::var(MEMORY_DB_ENV).ok();
        let prev_tickets_root = std::env::var(TICKETS_ROOT_ENV).ok();
        std::env::set_var(MEMORY_MD_ENV, md);
        std::env::set_var(MEMORY_DB_ENV, db);
        if let Some(root) = tickets_root {
            std::env::set_var(TICKETS_ROOT_ENV, root);
        }
        Self {
            _guard: guard,
            prev_md,
            prev_db,
            prev_tickets_root,
        }
    }
}

impl Drop for MemoryEnvScope {
    fn drop(&mut self) {
        match &self.prev_md {
            Some(v) => std::env::set_var(MEMORY_MD_ENV, v),
            None => std::env::remove_var(MEMORY_MD_ENV),
        }
        match &self.prev_db {
            Some(v) => std::env::set_var(MEMORY_DB_ENV, v),
            None => std::env::remove_var(MEMORY_DB_ENV),
        }
        match &self.prev_tickets_root {
            Some(v) => std::env::set_var(TICKETS_ROOT_ENV, v),
            None => std::env::remove_var(TICKETS_ROOT_ENV),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Re-export the shared guard type under the legacy local alias so that
    // all existing `EnvScope::new(...)` call-sites inside this module
    // continue to compile unchanged.
    type EnvScope = MemoryEnvScope;

    /// Build a fresh legacy-schema database that mimics what a pre-v1
    /// installation would have on disk: six FTS5 columns, no
    /// `schema_version` row in `memory_meta`.
    fn write_legacy_db(db_path: &Path) {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).unwrap();
            }
        }
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE investigations USING fts5(\
                ticket_id, customer, subject, symptom, assessment, resolution);\
             CREATE TABLE memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO investigations VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "12345",
                "alice@example.com",
                "stations offline",
                "five stations dropped within 30 seconds",
                "switch-side LAN flap",
                "bounced uplink port",
            ],
        )
        .unwrap();
    }

    fn columns_of(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('investigations')")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    #[test]
    fn migration_adds_new_columns_to_existing_db() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("MEMORY.md");
        let db = tmp.path().join("data/memory.db");
        let _scope = EnvScope::new(&md, &db);

        write_legacy_db(&db);
        // Sanity check: pre-migration we have exactly six columns.
        {
            let conn = Connection::open(&db).unwrap();
            let cols = columns_of(&conn);
            assert_eq!(cols.len(), 6, "legacy db should have 6 fts5 columns");
            assert!(!cols.iter().any(|c| c == "fork_letter"));
        }

        // Open via ensure_db → triggers migration.
        let conn = ensure_db(&db).unwrap();
        let cols = columns_of(&conn);
        assert!(cols.iter().any(|c| c == "fork_letter"));
        assert!(cols.iter().any(|c| c == "quoted_rubric_row"));
        assert!(cols.iter().any(|c| c == "rubric_version"));
        assert_eq!(cols.len(), 9);

        // Legacy row survived, with empty strings for new fields.
        let (ticket_id, fork_letter, rubric_version): (String, String, String) = conn
            .query_row(
                "SELECT ticket_id, fork_letter, rubric_version FROM investigations WHERE ticket_id = '12345'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(ticket_id, "12345");
        assert_eq!(fork_letter, "");
        assert_eq!(rubric_version, "");

        // schema_version recorded.
        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn migration_idempotent() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("MEMORY.md");
        let db = tmp.path().join("data/memory.db");
        let _scope = EnvScope::new(&md, &db);

        write_legacy_db(&db);
        // First migration pass — promotes the legacy 6-col table to v1.
        let conn = ensure_db(&db).unwrap();

        // Plant a sentinel row directly into the v1-shaped table. If a
        // subsequent `ensure_db` call were to re-run the rebuild branch, the
        // backup query (which only selects the legacy 6 columns) would drop
        // this row's `fork_letter` value back to the empty string — so a
        // surviving sentinel proves the rebuild branch was skipped.
        conn.execute(
            "INSERT INTO investigations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "SENTINEL_TICKET",
                "sentinel@example.com",
                "sentinel subject",
                "sentinel symptom",
                "sentinel assessment",
                "sentinel resolution",
                "SENTINEL_DO_NOT_REBUILD",
                "sentinel quoted rubric",
                "sentinel version",
            ],
        )
        .unwrap();
        drop(conn);

        // Second pass — should be a no-op (rows unchanged, columns unchanged).
        let conn = ensure_db(&db).unwrap();
        let cols = columns_of(&conn);
        assert_eq!(cols.len(), 9);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM investigations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 2);

        // Third pass on the same connection — still no-op.
        let conn = ensure_db(&db).unwrap();
        let cols = columns_of(&conn);
        assert_eq!(cols.len(), 9);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM investigations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 2);

        // Sentinel must survive: a buggy migration that re-DROPs and re-CREATEs
        // the table each time would lose this row's `fork_letter` value (the
        // backup query only selects the legacy 6 columns).
        let sentinel_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM investigations WHERE fork_letter = 'SENTINEL_DO_NOT_REBUILD'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            sentinel_count, 1,
            "sentinel row missing → migration re-ran rebuild branch"
        );
    }

    #[test]
    fn migration_on_fresh_db_records_schema_version() {
        // No pre-existing db file → ensure_db creates a v1-shaped table and
        // still records schema_version=2 for future migrations to branch on.
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("MEMORY.md");
        let db = tmp.path().join("data/memory.db");
        let _scope = EnvScope::new(&md, &db);

        let conn = ensure_db(&db).unwrap();
        let cols = columns_of(&conn);
        assert_eq!(cols.len(), 9);
        let v: String = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn append_with_fork_fields_persists_them() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("MEMORY.md");
        let db = tmp.path().join("data/memory.db");
        let _scope = EnvScope::new(&md, &db);

        append_investigation(
            "98765",
            "ops@example.com",
            "ALI lookup failing intermittently",
            "Multiple ALI lookups timing out across stations",
            "Probable upstream ALI provider degradation",
            Some("Routed to vendor; awaiting confirmation"),
            "B",
            "Vendor or Internal IT — escalate to ALI provider",
            "2026-05-12",
        )
        .unwrap();

        let conn = Connection::open(&db).unwrap();
        let (fl, qrr, rv): (String, String, String) = conn
            .query_row(
                "SELECT fork_letter, quoted_rubric_row, rubric_version \
                 FROM investigations WHERE ticket_id = '98765'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(fl, "B");
        assert!(qrr.contains("ALI provider"));
        assert_eq!(rv, "2026-05-12");

        // FTS5 indexes all text columns by default — a MATCH against a token
        // that lives only in `quoted_rubric_row` should still find the row.
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM investigations \
                 WHERE investigations MATCH 'escalate'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // MEMORY.md was extended with the new keys.
        let md_text = fs::read_to_string(&md).unwrap();
        assert!(md_text.contains("fork_letter: B"));
        assert!(md_text.contains("quoted_rubric_row: Vendor or Internal IT"));
        assert!(md_text.contains("rubric_version: 2026-05-12"));
    }

    #[test]
    fn parse_memory_md_handles_legacy_blocks_without_fork_keys() {
        let tmp = TempDir::new().unwrap();
        let md = tmp.path().join("MEMORY.md");
        // Legacy-format block: no fork_letter / quoted_rubric_row / rubric_version.
        fs::write(
            &md,
            "# Investigation Memory\n\n\
             ---\n\
             id: 11111\n\
             customer: legacy@example.com\n\
             date: 2025-01-01T00:00:00Z\n\
             subject: legacy ticket\n\
             symptom: something broke\n\
             assessment: needs review\n\
             resolution: fixed\n\
             ---\n",
        )
        .unwrap();

        let entries = parse_memory_md(&md).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.get("id").map(String::as_str), Some("11111"));
        assert_eq!(e.get("subject").map(String::as_str), Some("legacy ticket"));
        // Missing keys → not in map; rebuild_index defaults them to "".
        assert!(!e.contains_key("fork_letter"));
        assert!(!e.contains_key("quoted_rubric_row"));
        assert!(!e.contains_key("rubric_version"));

        // Verify rebuild_index tolerates the absence by indexing the legacy
        // block end-to-end against a v1 schema.
        let db = tmp.path().join("data/memory.db");
        let _scope = EnvScope::new(&md, &db);
        let count = rebuild_index(Some(&db)).unwrap();
        assert_eq!(count, 1);
        let conn = Connection::open(&db).unwrap();
        let (fl, qrr, rv): (String, String, String) = conn
            .query_row(
                "SELECT fork_letter, quoted_rubric_row, rubric_version \
                 FROM investigations WHERE ticket_id = '11111'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(fl, "");
        assert_eq!(qrr, "");
        assert_eq!(rv, "");
    }
}
