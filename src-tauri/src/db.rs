//! The vector database: one SQLite file with the sqlite-vec extension.
//!
//!   meta(key, value)               settings + which embedding model built the index
//!   folders(path, indexed_at)      folders the user added
//!   files(path, root, kind, …)     one row per indexed file/directory (mtime+size for incremental runs)
//!   chunks(id, path, text)         the text pieces
//!   chunk_vecs(rowid, embedding)   sqlite-vec table for file chunks; rowid = chunks.id
//!   folder_vecs(rowid, embedding)  sqlite-vec table for folder summaries; rowid = chunks.id
//!
//! The vector tables are created on the first embedding, because its column needs the vector
//! size (768 for nomic-embed-text) and that depends on the model.

use crate::ollama::same_model;
use crate::scan::Kind;
use anyhow::{bail, Context, Result};
use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

static REGISTER_VEC: Once = Once::new();

pub fn open(path: &Path) -> Result<Connection> {
    // Make sqlite-vec available to every connection opened from now on.
    REGISTER_VEC.call_once(|| unsafe {
        let _ = sqlite3_auto_extension(Some(std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ())));
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(15))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS meta    (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS folders (path TEXT PRIMARY KEY, indexed_at INTEGER);
         CREATE TABLE IF NOT EXISTS files (
             path   TEXT PRIMARY KEY,
             root   TEXT NOT NULL,
             kind   INTEGER NOT NULL,
             mtime  INTEGER NOT NULL,
             size   INTEGER NOT NULL,
             chunks INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS files_root ON files(root);
         CREATE TABLE IF NOT EXISTS chunks (id INTEGER PRIMARY KEY, path TEXT NOT NULL, text TEXT NOT NULL);
         CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
         CREATE TABLE IF NOT EXISTS skipped (
             path       TEXT PRIMARY KEY,
             root       TEXT NOT NULL,
             skipped_at INTEGER NOT NULL,
             reason     TEXT NOT NULL DEFAULT 'manual',
             size       INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS skipped_root ON skipped(root);
         CREATE TABLE IF NOT EXISTS included (path TEXT PRIMARY KEY);
         DROP TABLE IF EXISTS ratings;
         CREATE TABLE IF NOT EXISTS tags (
             path     TEXT NOT NULL,
             tag      TEXT NOT NULL COLLATE NOCASE,
             is_dir   INTEGER NOT NULL,
             added_at INTEGER NOT NULL,
             PRIMARY KEY (path, tag)
         );
         CREATE INDEX IF NOT EXISTS tags_tag ON tags(tag);",
    )?;
    // v0.2.2 created `skipped` without these columns.
    ensure_column(&conn, "skipped", "reason", "TEXT NOT NULL DEFAULT 'manual'")?;
    ensure_column(&conn, "skipped", "size", "INTEGER NOT NULL DEFAULT 0")?;
    // 1 = index subfolders too (the default), 0 = only the files directly in the folder.
    ensure_column(&conn, "folders", "recursive", "INTEGER NOT NULL DEFAULT 1")?;
    conn.query_row("SELECT vec_version()", [], |r| r.get::<_, String>(0))
        .context("the sqlite-vec extension did not load")?;
    Ok(conn)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<_>>()?;
    if !names.iter().any(|n| n == column) {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Start a transaction that takes the write lock immediately. With the default (deferred)
/// kind, two writers that both read first can deadlock, and SQLite then fails one of them
/// at once with "database is locked" instead of waiting. Immediate ones wait their turn
/// (up to the 15 s busy timeout).
pub fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
}

// ---------- meta ----------

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// (embedding model, vector size) the index was built with, if anything has been embedded yet.
pub fn index_model(conn: &Connection) -> Result<Option<(String, usize)>> {
    let model = get_meta(conn, "embed_model")?;
    let dim = get_meta(conn, "dim")?.and_then(|d| d.parse().ok());
    Ok(model.zip(dim))
}

/// The two vector tables: file chunks, and one summary per folder. Keeping folder summaries
/// apart lets "which folders…" questions search folders only and get one hit per folder.
pub const FILE_VECS: &str = "chunk_vecs";
pub const FOLDER_VECS: &str = "folder_vecs";

fn vec_table(kind: Kind) -> &'static str {
    match kind {
        Kind::File => FILE_VECS,
        Kind::Directory => FOLDER_VECS,
    }
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM sqlite_master WHERE name = ?1", [name], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Create the vector tables on first use, and refuse to mix embedding models afterwards.
pub fn ensure_vectors(conn: &Connection, model: &str, dim: usize) -> Result<()> {
    match index_model(conn)? {
        Some((m, d)) if same_model(&m, model) && d == dim => {}
        Some((m, d)) => bail!(
            "This index was built with `{m}` ({d}-dim vectors) and can't mix in `{model}`. \
             Switch the embedding model back in Settings, or reset the index."
        ),
        None => {
            conn.execute_batch("DROP TABLE IF EXISTS chunk_vecs; DROP TABLE IF EXISTS folder_vecs;")?;
            set_meta(conn, "embed_model", model)?;
            set_meta(conn, "dim", &dim.to_string())?;
        }
    }
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_vecs USING vec0(embedding float[{dim}]);
         CREATE VIRTUAL TABLE IF NOT EXISTS folder_vecs USING vec0(embedding float[{dim}]);"
    ))?;
    Ok(())
}

fn blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

// ---------- files & chunks ----------

pub struct FileState {
    pub kind: Kind,
    pub mtime: i64,
    pub size: i64,
}

pub fn file_states(conn: &Connection, root: &str) -> Result<HashMap<String, FileState>> {
    let mut stmt = conn.prepare("SELECT path, kind, mtime, size FROM files WHERE root = ?1")?;
    let rows = stmt.query_map([root], |r| {
        Ok((r.get::<_, String>(0)?, FileState { kind: Kind::from_i64(r.get(1)?), mtime: r.get(2)?, size: r.get(3)? }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn delete_file(tx: &Transaction, path: &str) -> Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare_cached("SELECT id FROM chunks WHERE path = ?1")?;
        let rows = stmt.query_map([path], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !ids.is_empty() {
        // A path's vectors live in one of the two tables; deleting a missing rowid is a no-op.
        for table in [FILE_VECS, FOLDER_VECS] {
            if !has_table(tx, table)? {
                continue;
            }
            let mut del = tx.prepare_cached(&format!("DELETE FROM {table} WHERE rowid = ?1"))?;
            for id in &ids {
                del.execute([id])?;
            }
        }
    }
    tx.execute("DELETE FROM chunks WHERE path = ?1", [path])?;
    tx.execute("DELETE FROM files WHERE path = ?1", [path])?;
    Ok(())
}

/// Replace everything stored for `path` with the given chunks (+ their vectors).
pub fn put_file(
    tx: &Transaction,
    path: &str,
    root: &str,
    kind: Kind,
    mtime: i64,
    size: i64,
    chunks: &[(String, Vec<f32>)],
) -> Result<()> {
    delete_file(tx, path)?;
    tx.execute(
        "INSERT INTO files(path, root, kind, mtime, size, chunks) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![path, root, kind as i64, mtime, size, chunks.len() as i64],
    )?;
    if chunks.is_empty() {
        return Ok(());
    }
    // A file that now has real text is no longer "encoded data".
    tx.execute("DELETE FROM skipped WHERE path = ?1 AND reason = 'encoded'", [path])?;
    let mut ins_chunk = tx.prepare_cached("INSERT INTO chunks(path, text) VALUES (?1, ?2)")?;
    let mut ins_vec =
        tx.prepare_cached(&format!("INSERT INTO {}(rowid, embedding) VALUES (?1, ?2)", vec_table(kind)))?;
    for (text, vector) in chunks {
        ins_chunk.execute(params![path, text])?;
        ins_vec.execute(params![tx.last_insert_rowid(), blob(vector)])?;
    }
    Ok(())
}

pub fn chunk_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?)
}

// ---------- search ----------

#[derive(Serialize, Clone)]
pub struct Hit {
    pub path: String,
    pub is_dir: bool,
    pub text: String,
    /// Cosine similarity (vectors are unit length, so it's derived from L2 distance).
    pub score: f32,
    /// Your tags on this file or folder.
    pub tags: Vec<String>,
}

/// Nearest `k` entries in `table` (FILE_VECS or FOLDER_VECS), best first.
pub fn search(conn: &Connection, table: &str, query: &[f32], k: usize) -> Result<Vec<Hit>> {
    if !has_table(conn, table)? {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(&format!(
        "WITH knn AS (
             SELECT rowid, distance FROM {table} WHERE embedding MATCH ?1 AND k = ?2
         )
         SELECT c.path, COALESCE(f.kind, 0), c.text, knn.distance
           FROM knn
           JOIN chunks c ON c.id = knn.rowid
           LEFT JOIN files f ON f.path = c.path
          ORDER BY knn.distance"
    ))?;
    let rows = stmt.query_map(params![blob(query), k as i64], |r| {
        let distance: f64 = r.get(3)?;
        Ok(Hit {
            path: r.get(0)?,
            is_dir: r.get::<_, i64>(1)? == Kind::Directory as i64,
            text: r.get(2)?,
            score: (1.0 - distance * distance / 2.0) as f32,
            tags: Vec::new(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------- folders ----------

#[derive(Serialize)]
pub struct FolderInfo {
    pub path: String,
    pub files: i64,
    pub dirs: i64,
    pub empty: i64,
    pub chunks: i64,
    pub indexed_at: Option<i64>,
    /// Your tags on the folder itself.
    pub tags: Vec<String>,
    /// Include subfolders when indexing.
    pub recursive: bool,
}

pub fn folders(conn: &Connection) -> Result<Vec<FolderInfo>> {
    let mut stmt = conn.prepare(
        "SELECT fo.path, fo.indexed_at,
                COALESCE(SUM(f.kind = 0 AND f.chunks > 0), 0),
                COALESCE(SUM(f.kind = 1), 0),
                COALESCE(SUM(f.kind = 0 AND f.chunks = 0), 0),
                COALESCE(SUM(f.chunks), 0),
                fo.recursive
           FROM folders fo LEFT JOIN files f ON f.root = fo.path
          GROUP BY fo.path ORDER BY fo.path COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(FolderInfo {
            path: r.get(0)?,
            indexed_at: r.get(1)?,
            files: r.get(2)?,
            dirs: r.get(3)?,
            empty: r.get(4)?,
            chunks: r.get(5)?,
            tags: Vec::new(),
            recursive: r.get::<_, i64>(6)? != 0,
        })
    })?;
    let mut list: Vec<FolderInfo> = rows.collect::<rusqlite::Result<_>>()?;
    let paths: Vec<String> = list.iter().map(|f| f.path.clone()).collect();
    let mut tags = tags_for(conn, &paths)?;
    for f in &mut list {
        f.tags = tags.remove(&f.path).unwrap_or_default();
    }
    Ok(list)
}

/// Whether indexing `root` includes its subfolders (true unless you turned it off).
pub fn folder_recursive(conn: &Connection, root: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT recursive FROM folders WHERE path = ?1", [root], |r| r.get::<_, i64>(0))
        .optional()?
        .map_or(true, |v| v != 0))
}

pub fn set_folder_recursive(conn: &Connection, root: &str, on: bool) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO folders(path) VALUES (?1)", [root])?;
    conn.execute("UPDATE folders SET recursive = ?2 WHERE path = ?1", params![root, on as i64])?;
    Ok(())
}

pub fn add_folder(conn: &Connection, root: &str) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO folders(path) VALUES (?1)", [root])?;
    Ok(())
}

pub fn mark_indexed(conn: &Connection, root: &str, unix_secs: i64) -> Result<()> {
    conn.execute("UPDATE folders SET indexed_at = ?2 WHERE path = ?1", params![root, unix_secs])?;
    Ok(())
}

pub fn remove_folder(conn: &mut Connection, root: &str) -> Result<()> {
    let paths: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM files WHERE root = ?1")?;
        let rows = stmt.query_map([root], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let tx = write_tx(conn)?;
    for p in &paths {
        delete_file(&tx, p)?;
    }
    tx.execute("DELETE FROM folders WHERE path = ?1", [root])?;
    tx.commit()?;
    Ok(())
}

/// Delete all indexed data (keeps settings). Needed after switching embedding models.
pub fn reset(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS chunk_vecs;
         DROP TABLE IF EXISTS folder_vecs;
         DELETE FROM chunks;
         DELETE FROM files;
         UPDATE folders SET indexed_at = NULL;
         DELETE FROM meta WHERE key IN ('embed_model', 'dim');",
    )?;
    conn.execute_batch("VACUUM")?;
    Ok(())
}

// ---------- tags ----------
// Your own labels on files and folders ("invoice", "client-x", "keep"). Stored by path, so the
// same tags can be shown by a file explorer. They survive Remove folder and Reset index.

#[derive(Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct Tagged {
    pub path: String,
    pub is_dir: bool,
    pub tags: Vec<String>,
    pub added_at: i64,
}

/// Trim, collapse inner spaces, cap the length. Empty means "no tag".
pub fn clean_tag(tag: &str) -> String {
    let t = tag.split_whitespace().collect::<Vec<_>>().join(" ");
    t.trim_start_matches('#').chars().take(40).collect::<String>().trim().to_string()
}

pub fn add_tag(conn: &Connection, path: &str, is_dir: bool, tag: &str) -> Result<String> {
    let tag = clean_tag(tag);
    if tag.is_empty() {
        bail!("A tag can't be empty.");
    }
    conn.execute(
        "INSERT OR IGNORE INTO tags(path, tag, is_dir, added_at) VALUES (?1, ?2, ?3, ?4)",
        params![path, tag, is_dir as i64, now_secs()],
    )?;
    Ok(tag)
}

pub fn remove_tag(conn: &Connection, path: &str, tag: &str) -> Result<()> {
    conn.execute("DELETE FROM tags WHERE path = ?1 AND tag = ?2 COLLATE NOCASE", params![path, tag])?;
    Ok(())
}

/// Tags for each of `paths` (paths without tags are left out of the map).
pub fn tags_for(conn: &Connection, paths: &[String]) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare_cached("SELECT tag FROM tags WHERE path = ?1 ORDER BY tag COLLATE NOCASE")?;
    let mut out = HashMap::new();
    for p in paths {
        let tags: Vec<String> = stmt.query_map([p], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        if !tags.is_empty() {
            out.insert(p.clone(), tags);
        }
    }
    Ok(out)
}

/// Every tag in use, with how many files and folders carry it, most used first.
pub fn list_tags(conn: &Connection) -> Result<Vec<TagCount>> {
    let mut stmt = conn.prepare(
        "SELECT tag, count(*) FROM tags GROUP BY tag COLLATE NOCASE ORDER BY count(*) DESC, tag COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| Ok(TagCount { tag: r.get(0)?, count: r.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Files and folders carrying `tag` (all tagged items when `tag` is empty), newest first.
pub fn tagged(conn: &Connection, tag: &str) -> Result<Vec<Tagged>> {
    let mut stmt = conn.prepare(
        "SELECT path, MAX(is_dir), MAX(added_at) FROM tags
          WHERE ?1 = '' OR path IN (SELECT path FROM tags WHERE tag = ?1 COLLATE NOCASE)
          GROUP BY path ORDER BY MAX(added_at) DESC",
    )?;
    let rows: Vec<(String, bool, i64)> = stmt
        .query_map([tag], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let paths: Vec<String> = rows.iter().map(|(p, _, _)| p.clone()).collect();
    let mut tags = tags_for(conn, &paths)?;
    Ok(rows
        .into_iter()
        .map(|(path, is_dir, added_at)| {
            let t = tags.remove(&path).unwrap_or_default();
            Tagged { path, is_dir, tags: t, added_at }
        })
        .collect())
}

// ---------- skipped files ----------
// Every readable file left out of the index, with why:
//   manual     you clicked "Skip this file" (kept until you include it)
//   too_large  over the size limit (2 MB text, 100 MB PDF)
//   data_file  .json/.xml/.log/.csv/.tsv over 256 KB
//   encoded    really encoded or minified data (get-pip.py, base64 blobs, bundles)
//   needs_vision  a picture, but the chosen picture model can't read pictures (or none is chosen)
// The size-based entries are recomputed on every Re-index. "Include" removes the entry and,
// for automatic skips, puts the file on the `included` list so the limits no longer apply.

#[derive(Serialize)]
pub struct Skipped {
    pub path: String,
    pub root: String,
    pub reason: String,
    pub size: i64,
    pub skipped_at: i64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Record a skipped file. A manual skip also takes the file off the `included` list.
pub fn record_skip(conn: &Connection, path: &str, root: &str, reason: &str, size: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO skipped(path, root, skipped_at, reason, size) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET root = excluded.root, skipped_at = excluded.skipped_at,
                                         reason = excluded.reason, size = excluded.size",
        params![path, root, now_secs(), reason, size],
    )?;
    if reason == crate::scan::REASON_MANUAL {
        conn.execute("DELETE FROM included WHERE path = ?1", [path])?;
    }
    Ok(())
}

/// Refresh the size-based skips for one folder after a scan, and drop "encoded" entries
/// for files that no longer exist there. Manual skips are left alone.
pub fn refresh_size_skips(
    conn: &mut Connection,
    root: &str,
    too_big: &[crate::scan::SizeSkip],
    existing: &std::collections::HashSet<String>,
) -> Result<()> {
    let encoded: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM skipped WHERE root = ?1 AND reason = 'encoded'")?;
        let rows = stmt.query_map([root], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let tx = write_tx(conn)?;
    tx.execute("DELETE FROM skipped WHERE root = ?1 AND reason IN ('too_large', 'data_file', 'needs_vision')", [root])?;
    for p in encoded.iter().filter(|p| !existing.contains(*p)) {
        tx.execute("DELETE FROM skipped WHERE path = ?1", [p])?;
    }
    for f in too_big {
        let path = f.path.to_string_lossy();
        // Don't overwrite a manual skip with a size reason.
        tx.execute(
            "INSERT INTO skipped(path, root, skipped_at, reason, size) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO NOTHING",
            params![&*path, root, now_secs(), f.reason, f.size],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Files you skipped by hand: left out of every run.
pub fn manual_skips(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT path FROM skipped WHERE reason = 'manual'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Files you chose to include despite the automatic limits.
pub fn included_paths(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT path FROM included")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Include a skipped file: it is indexed on the next Re-index of its folder.
pub fn include_path(conn: &mut Connection, path: &str) -> Result<()> {
    let reason: Option<String> =
        conn.query_row("SELECT reason FROM skipped WHERE path = ?1", [path], |r| r.get(0)).optional()?;
    let tx = write_tx(conn)?;
    tx.execute("DELETE FROM skipped WHERE path = ?1", [path])?;
    if reason.as_deref() != Some(crate::scan::REASON_MANUAL) {
        tx.execute("INSERT OR IGNORE INTO included(path) VALUES (?1)", [path])?;
    }
    // An "encoded" file is stored as already read; forget that so Re-index reads it again.
    delete_file(&tx, path)?;
    tx.commit()?;
    Ok(())
}

pub fn list_skipped(conn: &Connection) -> Result<Vec<Skipped>> {
    let mut stmt = conn.prepare(
        "SELECT path, root, reason, size, skipped_at FROM skipped
          ORDER BY CASE reason WHEN 'manual' THEN 0 ELSE 1 END, size DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Skipped { path: r.get(0)?, root: r.get(1)?, reason: r.get(2)?, size: r.get(3)?, skipped_at: r.get(4)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
