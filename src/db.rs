use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub const MAX_NOTE_BYTES: usize = 512_000;

pub struct Note {
    pub id: i64,
    pub body: String,
    pub rev: i64,
    pub cursor: usize,
}

const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE notes (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,  -- never reuse ids: a stale window must not hit a new note
  body            TEXT    NOT NULL DEFAULT '' CHECK (length(CAST(body AS BLOB)) <= 512000),
  rev             INTEGER NOT NULL DEFAULT 0,
  cursor          INTEGER NOT NULL DEFAULT 0,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  category_manual TEXT,
  category_ai     TEXT,
  ai_rev          INTEGER,
  ai_model        TEXT
);
CREATE VIRTUAL TABLE notes_fts USING fts5(body, content='notes', content_rowid='id');
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
  INSERT INTO notes_fts(rowid, body) VALUES (new.id, new.body); END;
CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
  INSERT INTO notes_fts(notes_fts, rowid, body) VALUES ('delete', old.id, old.body); END;
CREATE TRIGGER notes_au AFTER UPDATE OF body ON notes BEGIN
  INSERT INTO notes_fts(notes_fts, rowid, body) VALUES ('delete', old.id, old.body);
  INSERT INTO notes_fts(rowid, body) VALUES (new.id, new.body); END;
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#];

fn configure(c: &Connection) -> rusqlite::Result<()> {
    let mode: String = c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "journal_mode is {mode}, expected wal"
        )));
    }
    c.execute_batch("PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")
}

/// Writer connection: pragmas, WAL check, then pending migrations.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    configure(&c)?;
    let version = c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = c.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version={}", i + 1))?;
        tx.commit()?;
    }
    Ok(c)
}

/// Reader connection for the UI thread; refuses writes.
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    configure(&c)?;
    c.execute_batch("PRAGMA query_only=1")?;
    Ok(c)
}

pub fn insert_note(c: &Connection, body: &str, now_ms: i64) -> rusqlite::Result<i64> {
    c.execute(
        "INSERT INTO notes (body, created_at, updated_at) VALUES (?1, ?2, ?2)",
        params![body, now_ms],
    )?;
    Ok(c.last_insert_rowid())
}

/// Some(new_rev) on success, None when `expected_rev` is stale (another window saved).
pub fn save_note(
    c: &Connection,
    id: i64,
    expected_rev: i64,
    body: &str,
    cursor: usize,
    now_ms: i64,
) -> rusqlite::Result<Option<i64>> {
    let tx = c.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE notes SET body=?1, rev=rev+1, cursor=?2, updated_at=?3 WHERE id=?4 AND rev=?5",
        params![body, cursor as i64, now_ms, id, expected_rev],
    )?;
    if changed == 0 {
        return Ok(None); // dropping tx rolls back
    }
    kv_set(&tx, "active_note_id", &id.to_string())?;
    tx.commit()?;
    Ok(Some(expected_rev + 1))
}

pub fn load_note(c: &Connection, id: i64) -> rusqlite::Result<Option<Note>> {
    c.query_row(
        "SELECT id, body, rev, cursor FROM notes WHERE id=?",
        [id],
        |r| {
            Ok(Note {
                id: r.get(0)?,
                body: r.get(1)?,
                rev: r.get(2)?,
                cursor: r.get::<_, i64>(3)?.max(0) as usize,
            })
        },
    )
    .optional()
}

pub fn kv_get(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0)).optional()
}

pub fn kv_set(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn data_version(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("PRAGMA data_version", [], |r| r.get(0))
}

#[cfg(test)]
pub fn test_db() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("notes.db");
    let c = open(&p).unwrap();
    (d, p, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sets_pragmas_and_migrates() {
        let (_d, _p, c) = test_db();
        let jm: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(jm, "wal");
        assert_eq!(c.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0)).unwrap(), 2); // FULL
        assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn reopen_is_idempotent() {
        let (_d, p, _c) = test_db();
        open(&p).unwrap();
        open(&p).unwrap();
    }

    #[test]
    fn reader_cannot_write() {
        let (_d, p, _c) = test_db();
        assert!(insert_note(&open_reader(&p).unwrap(), "x", 1).is_err());
    }

    #[test]
    fn save_with_stale_rev_is_conflict() {
        let (_d, _p, c) = test_db();
        let id = insert_note(&c, "a", 1).unwrap();
        assert_eq!(save_note(&c, id, 0, "b", 1, 2).unwrap(), Some(1));
        assert_eq!(save_note(&c, id, 0, "c", 1, 3).unwrap(), None);
        let n = load_note(&c, id).unwrap().unwrap();
        assert_eq!((n.body.as_str(), n.rev, n.cursor), ("b", 1, 1));
        assert_eq!(kv_get(&c, "active_note_id").unwrap(), Some(id.to_string()));
    }

    #[test]
    fn body_limit_enforced_by_check() {
        let (_d, _p, c) = test_db();
        let id = insert_note(&c, "", 1).unwrap();
        assert!(save_note(&c, id, 0, &"a".repeat(MAX_NOTE_BYTES), 0, 2).unwrap().is_some());
        assert!(save_note(&c, id, 1, &"a".repeat(MAX_NOTE_BYTES + 1), 0, 3).is_err());
        assert_eq!(load_note(&c, id).unwrap().unwrap().rev, 1);
    }

    #[test]
    fn fts_follows_updates_and_deletes() {
        let (_d, _p, c) = test_db();
        let hits = |q: &str| {
            c.query_row("SELECT count(*) FROM notes_fts WHERE notes_fts MATCH ?", [q], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let id = insert_note(&c, "alpha", 1).unwrap();
        save_note(&c, id, 0, "beta", 0, 2).unwrap();
        assert_eq!((hits("alpha"), hits("beta")), (0, 1));
        c.execute("DELETE FROM notes WHERE id=?", [id]).unwrap();
        assert_eq!(hits("beta"), 0);
    }
}

pub struct ListItem {
    pub id: i64,
    pub title: String,
    pub group: String,
    pub stale: bool,
}

fn title_of(body: &str) -> String {
    match body.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => line.chars().take(60).collect(),
        None => "(empty)".to_string(),
    }
}

/// Notes grouped by display category: manual, else AI, else `Uncategorized` (last).
pub fn list_notes(c: &Connection) -> rusqlite::Result<Vec<ListItem>> {
    // ponytail: titles come from the first 300 chars; enough for a 60-char first line
    let mut stmt = c.prepare(
        "SELECT id, substr(body, 1, 300),
                COALESCE(category_manual, category_ai, 'Uncategorized'),
                category_manual IS NULL AND category_ai IS NOT NULL AND ai_rev < rev
         FROM notes
         ORDER BY category_manual IS NULL AND category_ai IS NULL, 3, updated_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ListItem {
            id: r.get(0)?,
            title: title_of(&r.get::<_, String>(1)?),
            group: r.get(2)?,
            stale: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// Plain-text FTS5 query: every whitespace token becomes a quoted phrase, so
/// `"`, `*`, `AND`, `(` are searched for, never parsed as syntax.
pub fn fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" "))
}

pub fn search(c: &Connection, input: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let Some(q) = fts_query(input) else { return Ok(Vec::new()) };
    let mut stmt = c.prepare(
        "SELECT rowid, snippet(notes_fts, 0, '[', ']', '…', 8) FROM notes_fts
         WHERE notes_fts MATCH ? ORDER BY bm25(notes_fts) LIMIT 50",
    )?;
    let rows = stmt.query_map([q], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

pub fn set_manual_category(c: &Connection, id: i64, category: Option<&str>) -> rusqlite::Result<()> {
    c.execute("UPDATE notes SET category_manual=?1 WHERE id=?2", params![category, id])?;
    Ok(())
}

pub fn delete_empty_notes(c: &Connection) -> rusqlite::Result<usize> {
    c.execute("DELETE FROM notes WHERE body = ''", [])
}

pub fn newest_note_id(c: &Connection) -> rusqlite::Result<Option<i64>> {
    c.query_row("SELECT id FROM notes ORDER BY updated_at DESC, id DESC LIMIT 1", [], |r| r.get(0))
        .optional()
}

/// Next note for the AI: never manual, not yet evaluated at this rev/model,
/// not saved in the last 4 s; the active note first.
pub fn next_ai_job(c: &Connection, model: &str, active: Option<i64>, now_ms: i64) -> rusqlite::Result<Option<(i64, String, i64)>> {
    c.query_row(
        "SELECT id, body, rev FROM notes
         WHERE category_manual IS NULL
           AND (ai_rev IS NULL OR ai_rev < rev OR ai_model IS NOT ?1)
           AND updated_at <= ?2
         ORDER BY id = ?3 DESC, updated_at DESC
         LIMIT 1",
        params![model, now_ms - 4000, active.unwrap_or(-1)],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
}

/// Stale-safe: writes nothing if the note changed (rev), got a manual category,
/// or already holds a result from a newer model version (versions are unix ms).
pub fn write_ai_result(c: &Connection, id: i64, rev: i64, category: Option<&str>, model: &str) -> rusqlite::Result<bool> {
    let changed = c.execute(
        "UPDATE notes SET category_ai=?1, ai_rev=?2, ai_model=?3
         WHERE id=?4 AND rev=?2 AND category_manual IS NULL
           AND (ai_model IS NULL OR CAST(ai_model AS INTEGER) <= CAST(?3 AS INTEGER))",
        params![category, rev, model, id],
    )?;
    Ok(changed == 1)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod list_search_tests {
    use super::*;

    #[test]
    fn list_groups_and_staleness() {
        let (_d, _p, c) = test_db();
        let a = insert_note(&c, "a", 1).unwrap();
        let b = insert_note(&c, "b", 2).unwrap();
        let _u = insert_note(&c, "u", 3).unwrap();
        c.execute("UPDATE notes SET category_ai='zeta', ai_rev=0, ai_model='m' WHERE id=?", [a]).unwrap();
        save_note(&c, a, 0, "a2", 0, 4).unwrap(); // makes the AI result stale
        set_manual_category(&c, b, Some("alpha")).unwrap();
        let l = list_notes(&c).unwrap();
        let g: Vec<_> = l.iter().map(|i| (i.group.as_str(), i.stale)).collect();
        assert_eq!(g, [("alpha", false), ("zeta", true), ("Uncategorized", false)]);
    }

    #[test]
    fn fts_query_escapes_syntax() {
        let (_d, _p, c) = test_db();
        insert_note(&c, r#"say "hi" AND (x)*"#, 1).unwrap();
        for q in [r#"""#, "AND", "(x)*", "-", r#"say "hi""#] {
            search(&c, q).unwrap();
        }
        assert_eq!(search(&c, r#""hi""#).unwrap().len(), 1);
        assert_eq!(fts_query("   "), None);
    }

    #[test]
    fn deleted_note_id_is_not_reused() {
        let (_d, _p, c) = test_db();
        insert_note(&c, "a", 1).unwrap();
        let b = insert_note(&c, "", 1).unwrap();
        delete_empty_notes(&c).unwrap();
        assert!(insert_note(&c, "c", 1).unwrap() > b);
    }

    #[test]
    fn older_model_cannot_overwrite_newer_result() {
        let (_d, _p, c) = test_db();
        let id = insert_note(&c, "a", 0).unwrap();
        assert!(write_ai_result(&c, id, 0, Some("new"), "200").unwrap());
        assert!(!write_ai_result(&c, id, 0, Some("old"), "100").unwrap());
    }

    #[test]
    fn delete_empty_only_removes_blank() {
        let (_d, _p, c) = test_db();
        insert_note(&c, "", 1).unwrap();
        let k = insert_note(&c, "k", 1).unwrap();
        assert_eq!(delete_empty_notes(&c).unwrap(), 1);
        assert_eq!(newest_note_id(&c).unwrap(), Some(k));
    }
}
