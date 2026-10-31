//! Folder → vectors: scan, extract text, chunk, embed in batches with Ollama, store in SQLite.
//! Incremental: unchanged files (same mtime + size) are skipped, deleted files are removed.
//! Files you skip from the progress card are remembered and left out of every later run.
//! When done, the embedding model is unloaded from Ollama to give the memory back.

use crate::chunk;
use crate::db;
use crate::ollama::{embed_prefixes, Ollama};
use crate::scan::{self, Item, Kind};
use crate::summary;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CHUNK_CHARS: usize = 1200;
const CHUNK_OVERLAP: usize = 200;
/// Chunks gathered before they're embedded and written (one database transaction).
const BATCH: usize = 64;
/// Chunks per /api/embed request. Small, so Skip and Stop take effect within one short
/// request (seconds) rather than waiting minutes for a big one on slow hardware.
const REQUEST: usize = 16;

#[derive(Serialize, Clone)]
pub struct Progress {
    pub phase: String, // "scanning" | "embedding" | "unloading" | "done"
    pub done: usize,
    pub total: usize,
    /// What the card shows under the bar.
    pub current: String,
    /// The file being worked on, for the Skip button ("" when there's nothing to skip).
    pub file: String,
    pub chunks: usize,
}

#[derive(Serialize, Default)]
pub struct Summary {
    pub folder: String,
    pub embedded: usize,
    pub chunks: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub empty_pdfs: Vec<String>,
    /// Files that turned out to be encoded or minified data (get-pip.py, bundles), not indexed.
    pub encoded: Vec<String>,
    /// Pictures described by the picture model this run.
    pub pictures: usize,
    /// Files you skipped during this run.
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub cancelled: bool,
    pub seconds: f32,
}

struct Pending {
    path: String,
    kind: Kind,
    mtime: i64,
    size: i64,
    texts: Vec<String>,
}

/// Everything the indexing loop needs to talk back to the app.
pub struct Control<'a> {
    pub cancel: &'a AtomicBool,
    /// Set by the Skip button to the path of the file to drop.
    pub skip: &'a Mutex<Option<String>>,
    pub progress: &'a dyn Fn(Progress),
}

impl Control<'_> {
    fn report(&self, phase: &str, done: usize, total: usize, current: &str, file: &str, chunks: usize) {
        (self.progress)(Progress {
            phase: phase.into(),
            done,
            total,
            current: current.into(),
            file: file.into(),
            chunks,
        })
    }

    /// Take the pending skip request if it names a path `wanted` accepts; otherwise leave it.
    fn take_skip_if(&self, wanted: impl Fn(&str) -> bool) -> Option<String> {
        let mut slot = self.skip.lock().ok()?;
        if slot.as_deref().is_some_and(&wanted) {
            slot.take()
        } else {
            None
        }
    }
}

pub fn index_folder(
    conn: &mut Connection,
    ollama: &Ollama,
    embed_model: &str,
    vision_model: Option<&str>,
    root: &str,
    ctl: &Control,
) -> Result<Summary> {
    let started = Instant::now();
    let mut s = Summary { folder: root.to_string(), ..Default::default() };

    ollama.ensure_model(embed_model)?;
    if let Some(vm) = vision_model {
        ollama.ensure_model(vm)?;
    }
    if let Some((m, _)) = db::index_model(conn)? {
        if !crate::ollama::same_model(&m, embed_model) {
            anyhow::bail!(
                "This index was built with `{m}`. Set the embedding model back to `{m}` in Settings, or reset the index to switch to `{embed_model}`."
            );
        }
    }
    db::add_folder(conn, root)?;
    let recursive = db::folder_recursive(conn, root)?;
    let _ = ctl.take_skip_if(|_| true); // ignore a stale click from an earlier run

    ctl.report("scanning", 0, 0, root, "", 0);
    let included = db::included_paths(conn)?;
    let (mut items, too_big) = scan::scan(Path::new(root), &included, vision_model.is_some(), recursive);
    if !recursive {
        // Subfolders are left out: keep the folder's own summary, drop the others it found.
        items.retain(|i| i.kind == Kind::File || i.path == Path::new(root));
    }

    // Files you skipped by hand stay out (and any data stored for them is removed below).
    let never = db::manual_skips(conn)?;
    items.retain(|i| !never.contains(&key(&i.path)));
    // Keep the "Skipped files" list current: size-based skips are recomputed every run.
    let existing: HashSet<String> = items.iter().map(|i| key(&i.path)).collect();
    db::refresh_size_skips(conn, root, &too_big, &existing)?;

    // Folder summaries are assembled from each folder's files (no LLM). For folders, `size`
    // stores a fingerprint of the summary, so a folder is re-embedded only when its summary
    // changes (a README edit, a new dependency, files added anywhere inside it).
    let langs = summary::count_languages(
        Path::new(root),
        items.iter().filter(|i| i.kind == Kind::File).map(|i| i.path.as_path()),
    );
    let mut summaries: HashMap<String, String> = HashMap::new();
    for item in items.iter_mut().filter(|i| i.kind == Kind::Directory) {
        let text = summary::folder_summary(&item.path, langs.get(&item.path));
        item.mtime = 0;
        item.size = summary::fingerprint(&text);
        summaries.insert(key(&item.path), text);
    }

    // Forget files that disappeared (or became excluded) since last time.
    let known = db::file_states(conn, root)?;
    let stale: Vec<&String> = known.keys().filter(|k| !existing.contains(*k)).collect();
    if !stale.is_empty() {
        let tx = db::write_tx(conn)?;
        for path in &stale {
            db::delete_file(&tx, path)?;
        }
        tx.commit()?;
    }
    s.removed = stale.len();

    let todo: Vec<&Item> = items
        .iter()
        .filter(|i| match known.get(&key(&i.path)) {
            Some(k) => k.mtime != i.mtime || k.size != i.size || k.kind != i.kind,
            None => true,
        })
        .collect();
    s.unchanged = items.len() - todo.len();
    let total = todo.len();

    let mut batch: Vec<Pending> = Vec::new();
    let mut batch_chunks = 0usize;
    for (n, item) in todo.iter().enumerate() {
        if ctl.cancel.load(Ordering::Relaxed) {
            s.cancelled = true;
            break;
        }
        let path = key(&item.path);
        let skippable = if item.kind == Kind::File { path.as_str() } else { "" };
        ctl.report("embedding", n, total, &path, skippable, s.chunks);

        let texts = match item.kind {
            Kind::Directory => {
                let text = summaries.remove(&path).unwrap_or_default();
                if text.is_empty() { vec![] } else { vec![text] }
            }
            Kind::File if scan::is_image(&item.path) => match vision_model {
                Some(vm) => {
                    ctl.report("embedding", n, total, &format!("{path}  (describing picture with {vm})"), &path, s.chunks);
                    let described = std::fs::read(&item.path)
                        .map_err(anyhow::Error::from)
                        .and_then(|bytes| ollama.describe_image(vm, &bytes));
                    match described {
                        Ok(caption) => {
                            s.pictures += 1;
                            let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            vec![format!("Picture: {name}\n{caption}")]
                        }
                        Err(e) => {
                            s.failed.push((path.clone(), format!("{e:#}")));
                            vec![]
                        }
                    }
                }
                None => vec![],
            },
            Kind::File => match scan::extract_text(&item.path) {
                Ok(raw) if !included.contains(&path) && scan::looks_encoded(&raw) => {
                    // Stored with no chunks, so it isn't re-read until the file changes,
                    // and listed under Skipped files so you can include it.
                    db::record_skip(conn, &path, root, scan::REASON_ENCODED, item.size)?;
                    s.encoded.push(path.clone());
                    vec![]
                }
                Ok(raw) => {
                    let pieces = chunk::chunk(&chunk::clean(&raw), CHUNK_CHARS, CHUNK_OVERLAP);
                    if pieces.is_empty() && scan::is_pdf(&item.path) {
                        s.empty_pdfs.push(path.clone());
                    }
                    pieces
                }
                Err(e) => {
                    s.failed.push((path.clone(), format!("{e:#}")));
                    vec![]
                }
            },
        };

        // Skip clicked while this file was being read.
        if ctl.take_skip_if(|p| p == path).is_some() {
            db::record_skip(conn, &path, root, scan::REASON_MANUAL, item.size)?;
            s.skipped.push(path);
            continue;
        }

        batch_chunks += texts.len();
        batch.push(Pending { path, kind: item.kind, mtime: item.mtime, size: item.size, texts });
        if batch_chunks >= BATCH {
            let (files, chunks) = flush(conn, ollama, embed_model, root, &mut batch, ctl, n, total, s.chunks, &mut s.skipped)?;
            s.embedded += files;
            s.chunks += chunks;
            batch_chunks = 0;
        }
    }
    // Whatever was read before Stop is still written, so no work is thrown away.
    let (files, chunks) = flush(conn, ollama, embed_model, root, &mut batch, ctl, total, total, s.chunks, &mut s.skipped)?;
    s.embedded += files;
    s.chunks += chunks;
    if !s.cancelled {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        db::mark_indexed(conn, root, now)?;
    }

    // Resource management: free the embedding model's memory before any chat model is used.
    ctl.report("unloading", total, total, embed_model, "", s.chunks);
    let _ = ollama.unload(embed_model);
    if let Some(vm) = vision_model {
        let _ = ollama.unload(vm);
    }

    s.seconds = started.elapsed().as_secs_f32();
    ctl.report("done", total, total, "", "", s.chunks);
    Ok(s)
}

/// Embed the chunks in `batch` (≤ BATCH per request) and write them in one transaction.
/// Between requests it reports progress and honours the Skip button: a skipped file's
/// remaining chunks are dropped, nothing of it is stored, and it is remembered as skipped.
/// Returns (items written, chunks written).
#[allow(clippy::too_many_arguments)]
fn flush(
    conn: &mut Connection,
    ollama: &Ollama,
    model: &str,
    root: &str,
    batch: &mut Vec<Pending>,
    ctl: &Control,
    done_files: usize,
    total_files: usize,
    chunks_before: usize,
    skipped_out: &mut Vec<String>,
) -> Result<(usize, usize)> {
    if batch.is_empty() {
        return Ok((0, 0));
    }
    let (doc_prefix, _) = embed_prefixes(model);
    // (owner index in batch, text to embed)
    let inputs: Vec<(usize, String)> = batch
        .iter()
        .enumerate()
        .flat_map(|(owner, p)| {
            p.texts.iter().map(move |t| {
                let text = match p.kind {
                    // The path is embedded with the text: "where" is often as telling as "what".
                    Kind::File => format!("{doc_prefix}File: {}\n\n{t}", p.path),
                    Kind::Directory => format!("{doc_prefix}{t}"),
                };
                (owner, text)
            })
        })
        .collect();

    // The file with the most chunks is the one worth showing (and skipping) on the card.
    let biggest = batch.iter().filter(|p| p.kind == Kind::File).max_by_key(|p| p.texts.len());
    let (label_path, skip_path) = match biggest {
        Some(p) => (p.path.clone(), p.path.clone()),
        None => (String::from("folder summaries"), String::new()),
    };

    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
    let mut skipped: HashSet<usize> = HashSet::new();
    let mut cursor = 0;
    let mut embedded = 0;
    let mut stopped = false;
    loop {
        if let Some(p) = ctl.take_skip_if(|p| batch.iter().any(|b| b.path == p)) {
            if let Some(owner) = batch.iter().position(|b| b.path == p) {
                skipped.insert(owner);
            }
        }
        // Stop pressed: keep what's finished, leave the rest for the next Re-index.
        if ctl.cancel.load(Ordering::Relaxed) {
            stopped = true;
            break;
        }
        let mut part: Vec<usize> = Vec::with_capacity(REQUEST);
        while cursor < inputs.len() && part.len() < REQUEST {
            if !skipped.contains(&inputs[cursor].0) {
                part.push(cursor);
            }
            cursor += 1;
        }
        if part.is_empty() {
            break;
        }
        let texts: Vec<String> = part.iter().map(|&j| inputs[j].1.clone()).collect();
        let result = ollama.embed(model, &texts, None)?;
        for (j, v) in part.iter().zip(result) {
            vectors[*j] = Some(v);
        }
        embedded += part.len();
        if inputs.len() > REQUEST {
            let remaining: usize = inputs.iter().filter(|(o, _)| !skipped.contains(o)).count();
            ctl.report(
                "embedding",
                done_files,
                total_files,
                &format!("{label_path}  (embedding {embedded} of {remaining} chunks)"),
                &skip_path,
                chunks_before + embedded,
            );
        }
    }

    if let Some(dim) = vectors.iter().flatten().map(Vec::len).next() {
        db::ensure_vectors(conn, model, dim)?;
    }

    // Hand each file its own vectors back, in order. After Stop, a file whose chunks weren't
    // all embedded is left out (not stored), so the next Re-index reads it again in full.
    let mut per_owner: Vec<Vec<Vec<f32>>> = vec![Vec::new(); batch.len()];
    let mut incomplete: HashSet<usize> = HashSet::new();
    for ((owner, _), v) in inputs.iter().zip(vectors) {
        match v {
            Some(mut v) => {
                normalize(&mut v);
                per_owner[*owner].push(v);
            }
            None if !skipped.contains(owner) => {
                incomplete.insert(*owner);
            }
            None => {}
        }
    }
    debug_assert!(stopped || incomplete.is_empty());

    let items = batch.len() - skipped.len() - incomplete.len();
    let mut written = 0;
    let tx = db::write_tx(conn)?;
    for (owner, (p, vecs)) in batch.drain(..).zip(per_owner).enumerate() {
        if incomplete.contains(&owner) {
            continue;
        }
        if skipped.contains(&owner) {
            db::delete_file(&tx, &p.path)?;
            db::record_skip(&tx, &p.path, root, scan::REASON_MANUAL, p.size)?;
            skipped_out.push(p.path);
            continue;
        }
        let chunks: Vec<(String, Vec<f32>)> = p.texts.into_iter().zip(vecs).collect();
        written += chunks.len();
        // Files with no text are stored too, so they aren't re-read on every run.
        db::put_file(&tx, &p.path, root, p.kind, p.mtime, p.size, &chunks)?;
    }
    tx.commit()?;
    Ok((items, written))
}

pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
