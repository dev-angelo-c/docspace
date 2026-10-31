//! DocSpace: Tauri backend. The page (src/index.html) calls these commands with `invoke`
//! and listens for `index-progress` and `answer` events.

mod chunk;
mod db;
mod indexer;
mod log;
mod ollama;
mod rag;
mod scan;
mod summary;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    db_path: PathBuf,
    cancel: Arc<AtomicBool>,
    /// Path the Skip button asked the indexer to drop.
    skip: Arc<std::sync::Mutex<Option<String>>>,
    indexing: Arc<AtomicBool>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    ollama_url: String,
    embed_model: String,
    chat_model: String,
    top_k: usize,
    num_ctx: u32,
    /// Model that classifies pictures while indexing. Only used if it can read pictures
    /// (vision); empty = none, so pictures are listed under Skipped instead.
    vision_model: String,
    /// Keep models on the CPU (num_gpu = 0): often faster on weak integrated graphics.
    cpu_only: bool,
    /// false = show sources only, without asking the answer model to write anything.
    write_answer: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ollama_url: "http://127.0.0.1:11434".into(),
            embed_model: "nomic-embed-text".into(),
            chat_model: "qwen3:8b".into(),
            top_k: 8,
            num_ctx: 8192,
            vision_model: String::new(),
            cpu_only: false,
            write_answer: true,
        }
    }
}

fn load_settings(conn: &rusqlite::Connection) -> anyhow::Result<Settings> {
    Ok(db::get_meta(conn, "settings")?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

type CmdResult<T> = Result<T, String>;

/// Run blocking work (SQLite, HTTP to Ollama, file reads) off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// Clears the "indexing" flag however the indexing run ends.
struct IndexingGuard(Arc<AtomicBool>);
impl Drop for IndexingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---------- commands ----------

#[derive(Serialize)]
struct Overview {
    settings: Settings,
    folders: Vec<db::FolderInfo>,
    index_model: Option<String>,
    dim: Option<usize>,
    chunks: i64,
    db_path: String,
    indexing: bool,
    /// The last "Check my computer" result, if any.
    benchmark: Option<serde_json::Value>,
}

#[tauri::command]
async fn overview(state: State<'_, AppState>) -> CmdResult<Overview> {
    let db_path = state.db_path.clone();
    let indexing = state.indexing.load(Ordering::SeqCst);
    blocking(move || {
        let conn = db::open(&db_path)?;
        let index = db::index_model(&conn)?;
        Ok(Overview {
            settings: load_settings(&conn)?,
            folders: db::folders(&conn)?,
            index_model: index.as_ref().map(|(m, _)| m.clone()),
            dim: index.map(|(_, d)| d),
            chunks: db::chunk_count(&conn)?,
            db_path: db_path.display().to_string(),
            indexing,
            benchmark: db::get_meta(&conn, "benchmark")?.and_then(|b| serde_json::from_str(&b).ok()),
        })
    })
    .await
}

#[tauri::command]
async fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        db::set_meta(&conn, "settings", &serde_json::to_string(&settings)?)
    })
    .await
}

#[derive(Serialize)]
struct OllamaStatus {
    version: String,
    models: Vec<String>,
    /// Same models with size and capabilities (vision, embedding, ...).
    details: Vec<ollama::ModelInfo>,
}

#[tauri::command]
async fn check_ollama(url: String) -> CmdResult<OllamaStatus> {
    blocking(move || {
        let o = ollama::Ollama::new(&url)?;
        let details = o.models_info()?;
        let models = details.iter().map(|m| m.name.clone()).collect();
        Ok(OllamaStatus { version: o.version()?, models, details })
    })
    .await
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let picked = app.dialog().file().set_title("Choose a folder to index").blocking_pick_folder();
        Ok(picked.map(|p| p.into_path()).transpose()?.map(|p| p.display().to_string()))
    })
    .await
}

#[tauri::command]
async fn index_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    recursive: Option<bool>,
) -> CmdResult<indexer::Summary> {
    if state.indexing.swap(true, Ordering::SeqCst) {
        return Err("A folder is already being indexed.".into());
    }
    let guard = IndexingGuard(state.indexing.clone());
    let db_path = state.db_path.clone();
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let skip = state.skip.clone();

    let root = path.trim().trim_end_matches(['\\', '/']).to_string();
    let root = if root.ends_with(':') { format!("{root}\\") } else { root }; // keep "C:\" valid
    blocking(move || {
        let _guard = guard;
        if !std::path::Path::new(&root).is_dir() {
            anyhow::bail!("{root} is not a folder.");
        }
        let mut conn = db::open(&db_path)?;
        if let Some(on) = recursive {
            db::set_folder_recursive(&conn, &root, on)?;
        }
        let settings = load_settings(&conn)?;
        let o = ollama::Ollama::new(&settings.ollama_url)?.with_cpu_only(settings.cpu_only);
        let report = |p: indexer::Progress| {
            let _ = app.emit("index-progress", p);
        };
        let ctl = indexer::Control { cancel: &cancel, skip: &skip, progress: &report };
        let (vision_owned, vision_note) = resolve_picture_model(&o, &settings.vision_model);
        let vision = vision_owned.as_deref();
        log::write(
            "INFO",
            "index",
            &format!(
                "indexing {root} with {} (pictures: {}; subfolders: {})",
                settings.embed_model,
                vision_note,
                if db::folder_recursive(&conn, &root).unwrap_or(true) { "on" } else { "off" }
            ),
        );
        let result = indexer::index_folder(&mut conn, &o, &settings.embed_model, vision, &root, &ctl);
        match &result {
            Ok(s) => {
                log::write(
                    "INFO",
                    "index",
                    &format!(
                        "{} {root} in {:.0}s: {} items, {} chunks, {} pictures described, {} unchanged, {} removed",
                        if s.cancelled { "stopped" } else { "finished" },
                        s.seconds, s.embedded, s.chunks, s.pictures, s.unchanged, s.removed
                    ),
                );
                for (path, err) in &s.failed {
                    log::write("WARN", "index", &format!("couldn't read {path}: {err}"));
                }
                for path in &s.encoded {
                    log::write("INFO", "index", &format!("skipped as encoded data: {path}"));
                }
                for path in &s.skipped {
                    log::write("INFO", "index", &format!("skipped by user: {path}"));
                }
            }
            Err(e) => log::write("ERROR", "index", &format!("{root}: {e:#}")),
        }
        result
    })
    .await
}

#[tauri::command]
fn cancel_index(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// Skip the file being indexed right now, and leave it out of future runs.
#[tauri::command]
fn skip_file(state: State<'_, AppState>, path: String) {
    if let Ok(mut slot) = state.skip.lock() {
        *slot = Some(path);
    }
}

#[tauri::command]
async fn list_skipped(state: State<'_, AppState>) -> CmdResult<Vec<db::Skipped>> {
    let db_path = state.db_path.clone();
    blocking(move || db::list_skipped(&db::open(&db_path)?)).await
}

/// Include skipped files: they're indexed on the next Re-index of their folders.
#[tauri::command]
async fn include_files(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let mut conn = db::open(&db_path)?;
        for p in &paths {
            db::include_path(&mut conn, p)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn remove_folder(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    if state.indexing.load(Ordering::SeqCst) {
        return Err("Wait for indexing to finish (or stop it) first.".into());
    }
    let db_path = state.db_path.clone();
    blocking(move || db::remove_folder(&mut db::open(&db_path)?, &path)).await
}

#[tauri::command]
async fn reset_index(state: State<'_, AppState>) -> CmdResult<()> {
    if state.indexing.load(Ordering::SeqCst) {
        return Err("Wait for indexing to finish (or stop it) first.".into());
    }
    let db_path = state.db_path.clone();
    blocking(move || db::reset(&db::open(&db_path)?)).await
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
enum AnswerEvent {
    Sources { id: u32, items: Vec<db::Hit>, prompt_tokens: usize },
    Thinking { id: u32 },
    Token { id: u32, text: String },
    Done { id: u32, stats: ollama::ChatStats },
}

#[tauri::command]
async fn ask(app: AppHandle, state: State<'_, AppState>, id: u32, question: String) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        let settings = load_settings(&conn)?;
        let o = ollama::Ollama::new(&settings.ollama_url)?.with_cpu_only(settings.cpu_only);
        let params = rag::AskParams {
            chat_model: &settings.chat_model,
            write_answer: settings.write_answer,
            top_k: settings.top_k,
            num_ctx: settings.num_ctx,
        };
        let started = std::time::Instant::now();
        let outcome = rag::ask(&conn, &o, &params, question.trim(), |ev| {
            let payload = match ev {
                rag::Event::Sources(hits, prompt_tokens) => {
                    AnswerEvent::Sources { id, items: hits.to_vec(), prompt_tokens }
                }
                rag::Event::Thinking => AnswerEvent::Thinking { id },
                rag::Event::Token(t) => AnswerEvent::Token { id, text: t.to_string() },
                rag::Event::Done(stats) => AnswerEvent::Done { id, stats },
            };
            let _ = app.emit("answer", payload);
        });
        match &outcome {
            Ok(_) if !settings.write_answer => log::write(
                "INFO",
                "ask",
                &format!("sources only in {:.1}s", started.elapsed().as_secs_f32()),
            ),
            Ok(_) => log::write(
                "INFO",
                "ask",
                &format!("answered with {} in {:.0}s", settings.chat_model, started.elapsed().as_secs_f32()),
            ),
            Err(e) => log::write("ERROR", "ask", &format!("{} ({}): {e:#}", question.trim(), settings.chat_model)),
        }
        outcome?;
        Ok(())
    })
    .await
}

/// The page reports failed commands and script errors here.
#[tauri::command]
fn log_client(level: String, message: String) {
    log::write(&level.to_uppercase(), "ui", &message);
}

/// The newest log lines, for the Log window.
#[tauri::command]
fn read_log() -> String {
    log::tail(400)
}

#[tauri::command]
fn clear_log() {
    log::clear();
    log::write("INFO", "app", "log cleared");
}

#[tauri::command]
fn log_file_path() -> String {
    log::path().map(|p| p.display().to_string()).unwrap_or_default()
}

#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Tag a file or folder. Returns the tag as stored (trimmed).
#[tauri::command]
async fn add_tag(state: State<'_, AppState>, path: String, is_dir: bool, tag: String) -> CmdResult<String> {
    let db_path = state.db_path.clone();
    blocking(move || db::add_tag(&db::open(&db_path)?, &path, is_dir, &tag)).await
}

#[tauri::command]
async fn remove_tag(state: State<'_, AppState>, path: String, tag: String) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || db::remove_tag(&db::open(&db_path)?, &path, &tag)).await
}

/// Every tag in use, with counts.
#[tauri::command]
async fn list_tags(state: State<'_, AppState>) -> CmdResult<Vec<db::TagCount>> {
    let db_path = state.db_path.clone();
    blocking(move || db::list_tags(&db::open(&db_path)?)).await
}

/// Files and folders with `tag` ("" = everything tagged).
#[tauri::command]
async fn tagged(state: State<'_, AppState>, tag: String) -> CmdResult<Vec<db::Tagged>> {
    let db_path = state.db_path.clone();
    blocking(move || db::tagged(&db::open(&db_path)?, &tag)).await
}

/// Include or leave out a folder's subfolders. Takes effect on its next Re-index.
#[tauri::command]
async fn set_folder_recursive(state: State<'_, AppState>, path: String, on: bool) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || db::set_folder_recursive(&db::open(&db_path)?, &path, on)).await
}

/// Pictures are classified only by the model you chose, and only if that model can read
/// pictures. Anything else (no model, a text-only model, a model that isn't installed) means
/// pictures are not classified; they're listed under Skipped as "needs a picture model".
/// Returns the model to use and a note for the log.
fn resolve_picture_model(o: &ollama::Ollama, setting: &str) -> (Option<String>, String) {
    let s = setting.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("auto") {
        return (None, "none chosen".into());
    }
    match o.models_info() {
        Ok(models) => match models.iter().find(|m| ollama::same_model(&m.name, s)) {
            Some(m) if m.has("vision") => (Some(m.name.clone()), m.name.clone()),
            Some(m) => (None, format!("{} can't read pictures", m.name)),
            None => (None, format!("{s} isn't installed")),
        },
        Err(e) => (None, format!("couldn't check {s}: {e:#}")),
    }
}

#[tauri::command]
async fn set_cpu_only(state: State<'_, AppState>, on: bool) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        let mut settings = load_settings(&conn)?;
        settings.cpu_only = on;
        db::set_meta(&conn, "settings", &serde_json::to_string(&settings)?)
    })
    .await
}

/// Typical DocSpace question: read ~2,500 tokens of passages, write ~150 tokens.
const TYPICAL_READ: f64 = 2500.0;
const TYPICAL_WRITE: f64 = 150.0;

fn typical_answer_secs(b: &ollama::Bench) -> f64 {
    if b.read_tps <= 0.0 || b.write_tps <= 0.0 {
        return f64::INFINITY;
    }
    TYPICAL_READ / b.read_tps + TYPICAL_WRITE / b.write_tps
}

#[derive(Serialize, Clone)]
struct BenchRow {
    model: String,
    size: u64,
    read_tps: f64,
    write_tps: f64,
    load_secs: f64,
    /// Estimated seconds for a typical answer (reading + writing; loading not included).
    answer_secs: f64,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
struct CpuTest {
    model: String,
    current_secs: f64,
    cpu_secs: f64,
}

#[derive(Serialize, Clone)]
struct BenchReport {
    rows: Vec<BenchRow>,
    recommended: Option<String>,
    cpu_test: Option<CpuTest>,
    cpu_only: bool,
    ran_at: i64,
}

#[derive(Serialize, Clone)]
struct BenchProgress {
    step: usize,
    steps: usize,
    label: String,
    row: Option<BenchRow>,
}

/// "Check my computer": time every installed answer model on this machine, recommend one,
/// and see whether CPU-only is faster. Takes a few minutes; each model is unloaded after its test.
#[tauri::command]
async fn benchmark_models(app: AppHandle, state: State<'_, AppState>) -> CmdResult<BenchReport> {
    if state.indexing.load(Ordering::SeqCst) {
        return Err("Wait for indexing to finish first: it would slow the test down and skew the results.".into());
    }
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        let settings = load_settings(&conn)?;
        let o = ollama::Ollama::new(&settings.ollama_url)?;
        let models: Vec<ollama::ModelInfo> = o
            .models_info()?
            .into_iter()
            .filter(|m| m.has("completion") && !m.has("vision") && !m.has("embedding"))
            .collect();
        if models.is_empty() {
            anyhow::bail!("No answer models are installed. In PowerShell run:  ollama pull qwen3:4b");
        }
        let steps = models.len() + usize::from(!settings.cpu_only);
        let emit = |step: usize, label: String, row: Option<BenchRow>| {
            let _ = app.emit("bench-progress", BenchProgress { step, steps, label, row });
        };
        log::write("INFO", "bench", &format!("testing {} model(s), cpu_only={}", models.len(), settings.cpu_only));

        let mut rows = Vec::new();
        for (i, m) in models.iter().enumerate() {
            emit(i, format!("Testing {} ({} of {})…", m.name, i + 1, models.len()), None);
            let row = match o.benchmark(&m.name, settings.cpu_only) {
                Ok(b) => BenchRow {
                    model: m.name.clone(),
                    size: m.size,
                    read_tps: b.read_tps,
                    write_tps: b.write_tps,
                    load_secs: b.load_secs,
                    answer_secs: typical_answer_secs(&b),
                    error: None,
                },
                Err(e) => BenchRow {
                    model: m.name.clone(),
                    size: m.size,
                    read_tps: 0.0,
                    write_tps: 0.0,
                    load_secs: 0.0,
                    answer_secs: f64::INFINITY,
                    error: Some(format!("{e:#}")),
                },
            };
            let _ = o.unload(&m.name);
            log::write(
                "INFO",
                "bench",
                &format!(
                    "{}: reads {:.1} tok/s, writes {:.1} tok/s, typical answer {:.0}s{}",
                    row.model,
                    row.read_tps,
                    row.write_tps,
                    row.answer_secs,
                    row.error.as_deref().map(|e| format!(" (error: {e})")).unwrap_or_default()
                ),
            );
            emit(i + 1, String::new(), Some(row.clone()));
            rows.push(row);
        }

        // Recommend the biggest (usually best) model that answers within 2 minutes;
        // if none does, the fastest one.
        let ok: Vec<&BenchRow> = rows.iter().filter(|r| r.error.is_none() && r.answer_secs.is_finite()).collect();
        let recommended = ok
            .iter()
            .filter(|r| r.answer_secs <= 120.0)
            .max_by_key(|r| r.size)
            .or_else(|| ok.iter().min_by(|a, b| a.answer_secs.total_cmp(&b.answer_secs)))
            .map(|r| r.model.clone());

        // Is CPU-only faster than what Ollama does by default (which may use the GPU)?
        let mut cpu_test = None;
        if !settings.cpu_only {
            if let Some(model) = &recommended {
                emit(models.len(), format!("Comparing CPU-only for {model}…"), None);
                if let Ok(b) = o.benchmark(model, true) {
                    let current = rows.iter().find(|r| &r.model == model).map_or(f64::INFINITY, |r| r.answer_secs);
                    cpu_test = Some(CpuTest { model: model.clone(), current_secs: current, cpu_secs: typical_answer_secs(&b) });
                }
                let _ = o.unload(model);
            }
        }

        let report = BenchReport {
            rows,
            recommended,
            cpu_test,
            cpu_only: settings.cpu_only,
            ran_at: unix_now(),
        };
        db::set_meta(&conn, "benchmark", &serde_json::to_string(&report)?)?;
        emit(steps, "Done".into(), None);
        Ok(report)
    })
    .await
}

/// Turn writing answers on or off ("sources only").
#[tauri::command]
async fn set_write_answer(state: State<'_, AppState>, on: bool) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        let mut settings = load_settings(&conn)?;
        settings.write_answer = on;
        db::set_meta(&conn, "settings", &serde_json::to_string(&settings)?)
    })
    .await
}

/// Make `model` the answer model and unload the previous one, so only one sits in memory.
#[tauri::command]
async fn switch_chat_model(state: State<'_, AppState>, model: String) -> CmdResult<()> {
    let db_path = state.db_path.clone();
    blocking(move || {
        let conn = db::open(&db_path)?;
        let mut settings = load_settings(&conn)?;
        let model = model.trim().to_string();
        if model.is_empty() || ollama::same_model(&settings.chat_model, &model) {
            return Ok(());
        }
        let previous = std::mem::replace(&mut settings.chat_model, model);
        db::set_meta(&conn, "settings", &serde_json::to_string(&settings)?)?;
        if let Ok(o) = ollama::Ollama::new(&settings.ollama_url).map(|o| o.with_cpu_only(settings.cpu_only)) {
            if o.loaded_models().unwrap_or_default().iter().any(|m| ollama::same_model(m, &previous)) {
                let _ = o.unload(&previous);
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
fn open_path(app: AppHandle, path: String) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_path(app: AppHandle, path: String) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // %APPDATA%\app.docspace.desktop\docspace.db on Windows
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            log::init(dir.join("docspace.log"));
            app.manage(AppState {
                db_path: dir.join("docspace.db"),
                cancel: Arc::new(AtomicBool::new(false)),
                skip: Arc::new(std::sync::Mutex::new(None)),
                indexing: Arc::new(AtomicBool::new(false)),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            overview,
            save_settings,
            check_ollama,
            pick_folder,
            index_folder,
            cancel_index,
            remove_folder,
            reset_index,
            ask,
            open_path,
            reveal_path,
            app_version,
            add_tag,
            remove_tag,
            list_tags,
            tagged,
            switch_chat_model,
            skip_file,
            list_skipped,
            include_files,
            log_client,
            read_log,
            clear_log,
            log_file_path,
            set_write_answer,
            set_folder_recursive,
            set_cpu_only,
            benchmark_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running DocSpace");
}
