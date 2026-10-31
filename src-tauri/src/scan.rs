//! Walking a folder and turning files into text.

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Directories never descended into (in addition to .gitignore'd and hidden ones).
pub const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "bower_components", "target", "dist", "build", "out",
    ".next", ".nuxt", ".svelte-kit", ".turbo", ".parcel-cache", ".venv", "venv", "env", "__pycache__",
    ".mypy_cache", ".pytest_cache", ".tox", ".cache", ".gradle", ".idea", ".vscode", "vendor",
    "Pods", ".terraform", "coverage", "bin", "obj", "packages", "$RECYCLE.BIN", "System Volume Information",
    "AppData", "Config.Msi", "Recovery", "PerfLogs",
];

/// Windows system folders, skipped only when they sit directly on a drive (C:\Windows),
/// so a project folder that happens to be called "windows" is still indexed.
const SYSTEM_ROOT_DIRS: &[&str] = &[
    "Windows", "Program Files", "Program Files (x86)", "ProgramData", "Intel", "AMD", "NVIDIA", "MSOCache",
];

fn name_in(name: &str, list: &[&str]) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name)) // Windows names are case-insensitive
}

/// Files and folders Windows marks as hidden or system (AppData, desktop.ini, pagefile.sys, ...).
#[cfg(windows)]
fn has_hidden_or_system_attr(e: &ignore::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    e.metadata().is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0)
}

#[cfg(not(windows))]
fn has_hidden_or_system_attr(_e: &ignore::DirEntry) -> bool {
    false
}

/// Should the walker skip this entry (and, for a folder, everything inside it)?
fn excluded(e: &ignore::DirEntry) -> bool {
    if e.depth() == 0 {
        return false; // never skip the folder the user picked
    }
    if has_hidden_or_system_attr(e) {
        return true;
    }
    if !e.file_type().is_some_and(|t| t.is_dir()) {
        return false;
    }
    let Some(name) = e.file_name().to_str() else { return false };
    if name_in(name, SKIP_DIRS) {
        return true;
    }
    // On a drive root: e.path() = C:\Windows, its parent C:\ has no parent.
    let on_drive_root = e.path().parent().is_some_and(|p| p.parent().is_none());
    on_drive_root && name_in(name, SYSTEM_ROOT_DIRS)
}

/// Generated / lock files: large, low-signal, they drown out real matches.
const SKIP_FILES: &[&str] = &[
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock", "composer.lock",
    "Gemfile.lock", "go.sum", "Pipfile.lock", "bun.lockb", ".DS_Store", "desktop.ini", "Thumbs.db",
];

const TEXT_EXTS: &[&str] = &[
    // prose & docs
    "md", "markdown", "txt", "rst", "org", "adoc", "tex", "csv", "tsv", "log",
    // web
    "html", "htm", "css", "scss", "sass", "less", "js", "mjs", "cjs", "jsx", "ts", "tsx", "vue", "svelte", "astro",
    // code
    "rs", "py", "go", "java", "kt", "kts", "scala", "c", "h", "cc", "cpp", "hpp", "cs", "fs", "vb", "rb", "php",
    "swift", "m", "dart", "lua", "r", "jl", "ex", "exs", "erl", "hs", "ml", "clj", "el", "zig", "nim", "sol", "sql",
    "sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd",
    // config & data
    "toml", "yaml", "yml", "json", "jsonc", "xml", "ini", "cfg", "conf", "properties", "gradle", "nix",
    "proto", "graphql", "gql", "tf", "hcl", "csproj", "sln",
];

const NAMED_TEXT_FILES: &[&str] = &[
    "Dockerfile", "Containerfile", "Makefile", "Justfile", "Procfile", "Gemfile", "Rakefile", "Vagrantfile",
    "LICENSE", "README", "CHANGELOG", "CMakeLists.txt",
];

pub const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;
/// Data-style files (exports, logs, dumps) bigger than this are mostly noise for questions,
/// and slow to embed: a 2 MB bookmarks.json is ~1,500 chunks.
pub const MAX_DATA_BYTES: u64 = 256 * 1024;
const DATA_EXTS: &[&str] = &["json", "jsonc", "xml", "log", "csv", "tsv"];

fn is_data_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| DATA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}
pub const MAX_PDF_BYTES: u64 = 100 * 1024 * 1024;
/// Hard ceiling for a text file you chose to include anyway (reading it is held in memory).
pub const MAX_INCLUDED_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// Why a file was left out automatically. Stored with the file so it can be listed and included later.
pub const REASON_TOO_LARGE: &str = "too_large";
pub const REASON_DATA_FILE: &str = "data_file";
pub const REASON_ENCODED: &str = "encoded";
pub const REASON_MANUAL: &str = "manual";
/// A picture, but the chosen picture model can't read pictures (or none is chosen).
pub const REASON_NEEDS_VISION: &str = "needs_vision";

/// A readable file left out because of its size, or because no model can classify it.
pub struct SizeSkip {
    pub path: PathBuf,
    pub size: i64,
    pub reason: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Text,
    Pdf,
    /// Described by a vision model when "Picture model" is set in Settings.
    Image,
}

const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
/// Smaller pictures are almost always icons, buttons and thumbnails: not worth describing.
pub const MIN_IMAGE_BYTES: u64 = 16 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File = 0,
    Directory = 1,
}

impl Kind {
    pub fn from_i64(v: i64) -> Kind {
        if v == 1 { Kind::Directory } else { Kind::File }
    }
}

pub struct Item {
    pub path: PathBuf,
    pub kind: Kind,
    pub mtime: i64,
    pub size: i64,
}

pub fn classify(path: &Path) -> Option<FileType> {
    let name = path.file_name()?.to_str()?;
    if SKIP_FILES.contains(&name) || name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".map") {
        return None;
    }
    match path.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase) {
        Some(ext) if ext == "pdf" => Some(FileType::Pdf),
        Some(ext) if IMAGE_EXTS.contains(&ext.as_str()) => Some(FileType::Image),
        Some(ext) if TEXT_EXTS.contains(&ext.as_str()) => Some(FileType::Text),
        _ if NAMED_TEXT_FILES.contains(&name) || name.starts_with("README") => Some(FileType::Text),
        _ => None,
    }
}

/// Walk `root`, returning indexable files plus every directory visited, and the readable
/// files left out because of their size. Paths in `included` (files you chose to include)
/// skip the size limits.
/// Pictures are only collected when `pictures` is true (a picture model is set).
/// With `recursive` false, only the files directly inside `root` are collected.
pub fn scan(root: &Path, included: &HashSet<String>, pictures: bool, recursive: bool) -> (Vec<Item>, Vec<SizeSkip>) {
    let walker = WalkBuilder::new(root)
        .hidden(true) // skip dotfiles/dotdirs (keeps .env secrets out of the index too)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false) // honour .gitignore even outside a git checkout
        .follow_links(false)
        .max_depth(if recursive { None } else { Some(1) })
        .filter_entry(|e| !excluded(e))
        .build();

    let mut items = Vec::new();
    let mut too_big = Vec::new();
    for entry in walker.flatten() {
        let Some(file_type) = entry.file_type() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);

        if file_type.is_dir() {
            items.push(Item { path: entry.into_path(), kind: Kind::Directory, mtime, size: 0 });
        } else if file_type.is_file() {
            let file_type = match classify(entry.path()) {
                Some(t) => t,
                None => continue,
            };
            if file_type == FileType::Image && meta.len() < MIN_IMAGE_BYTES {
                continue; // icons and thumbnails
            }
            if file_type == FileType::Image && !pictures {
                // No model that can read pictures: list it under Skipped instead of indexing it.
                too_big.push(SizeSkip { path: entry.into_path(), size: meta.len() as i64, reason: REASON_NEEDS_VISION });
                continue;
            }
            let forced = included.contains(&*entry.path().to_string_lossy());
            let (limit, reason) = match file_type {
                FileType::Image if forced => (u64::MAX, REASON_TOO_LARGE),
                FileType::Image => (MAX_IMAGE_BYTES, REASON_TOO_LARGE),
                _ if forced => (if file_type == FileType::Pdf { u64::MAX } else { MAX_INCLUDED_TEXT_BYTES }, REASON_TOO_LARGE),
                FileType::Text if is_data_file(entry.path()) => (MAX_DATA_BYTES, REASON_DATA_FILE),
                FileType::Text => (MAX_TEXT_BYTES, REASON_TOO_LARGE),
                FileType::Pdf => (MAX_PDF_BYTES, REASON_TOO_LARGE),
            };
            if meta.len() <= limit {
                items.push(Item { path: entry.into_path(), kind: Kind::File, mtime, size: meta.len() as i64 });
            } else {
                too_big.push(SizeSkip { path: entry.into_path(), size: meta.len() as i64, reason });
            }
        }
    }
    (items, too_big)
}

/// True for text that is really encoded or minified data rather than something to read:
/// get-pip.py's embedded archive, base64 blobs, minified bundles, compact data dumps.
/// Normal prose and code is 10-25% whitespace; these are under ~4%.
pub fn looks_encoded(text: &str) -> bool {
    const MIN_LEN: usize = 20 * 1024;
    if text.len() < MIN_LEN {
        return false;
    }
    let whitespace = text.bytes().filter(|b| b.is_ascii_whitespace()).count();
    (whitespace as f64) / (text.len() as f64) < 0.04
}

pub fn is_image(path: &Path) -> bool {
    classify(path) == Some(FileType::Image)
}

pub fn is_pdf(path: &Path) -> bool {
    classify(path) == Some(FileType::Pdf)
}

/// Plain text of a file. Returns "" for files that turn out to be binary.
pub fn extract_text(path: &Path) -> Result<String> {
    match classify(path) {
        Some(FileType::Pdf) => extract_pdf(path),
        Some(FileType::Text) => {
            let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            if bytes[..bytes.len().min(8192)].contains(&0) {
                return Ok(String::new()); // NUL byte => binary despite the extension
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        // Pictures have no text of their own; the indexer asks the picture model instead.
        Some(FileType::Image) | None => Ok(String::new()),
    }
}

/// PDF → text with the pure-Rust pdf-extract crate. No OCR: scanned PDFs (page images,
/// no text layer) come back empty and are reported after indexing.
fn extract_pdf(path: &Path) -> Result<String> {
    // pdf-extract can panic on malformed PDFs; contain it so one bad file can't stop indexing.
    let owned = path.to_path_buf();
    match std::panic::catch_unwind(move || pdf_extract::extract_text(&owned)) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => bail!("could not read PDF: {e}"),
        Err(_) => bail!("could not read PDF (malformed or unsupported)"),
    }
}
