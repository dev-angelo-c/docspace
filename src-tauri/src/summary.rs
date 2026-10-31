//! Folder summaries: one short, searchable description per folder, assembled from the
//! folder's own files. No LLM is involved, so it costs a fraction of a second per folder.
//!
//! A summary holds: the folder's name and path, what kind of project it looks like
//! (from marker files such as package.json or Cargo.toml), the project's name,
//! description and libraries, the languages of all files beneath it, the start of
//! its README, and the names of its entries.

use crate::chunk;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_SUMMARY_CHARS: usize = 1100;
const MAX_NAMES: usize = 40;
const README_CHARS: usize = 450;
const MAX_DEPS: usize = 25;

/// The line that marks a summary as a recognised project (used when ranking folders).
pub const PROJECT_MARKER: &str = "\nLooks like: ";

/// Language → number of indexed files, for every folder, counting everything beneath it.
pub type LangCounts = HashMap<PathBuf, HashMap<&'static str, usize>>;

pub fn language_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "astro" => "Astro",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        "py" => "Python",
        "rs" => "Rust",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "dart" => "Dart",
        "lua" => "Lua",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "md" | "markdown" | "rst" | "txt" => "text",
        "pdf" => "PDF",
        _ => return None,
    })
}

/// Count each file's language into every folder above it, up to `root`.
pub fn count_languages<'a>(root: &Path, files: impl Iterator<Item = &'a Path>) -> LangCounts {
    let mut counts: LangCounts = HashMap::new();
    for f in files {
        let Some(lang) = language_of(f) else { continue };
        for dir in f.ancestors().skip(1) {
            if !dir.starts_with(root) {
                break;
            }
            *counts.entry(dir.to_path_buf()).or_default().entry(lang).or_default() += 1;
        }
    }
    counts
}

/// Libraries that mark a JavaScript project as a web app, with a readable name.
const WEB_FRAMEWORKS: &[(&str, &str)] = &[
    ("react", "React"), ("next", "Next.js"), ("vue", "Vue"), ("nuxt", "Nuxt"), ("svelte", "Svelte"),
    ("@sveltejs/kit", "SvelteKit"), ("@angular/core", "Angular"), ("astro", "Astro"), ("vite", "Vite"),
    ("express", "Express"), ("fastify", "Fastify"), ("koa", "Koa"), ("webpack", "webpack"),
    ("parcel", "Parcel"), ("jquery", "jQuery"), ("three", "Three.js"), ("@tauri-apps/api", "Tauri"),
    ("electron", "Electron"), ("react-native", "React Native"), ("expo", "Expo"),
];

pub fn folder_summary(dir: &Path, langs: Option<&HashMap<&'static str, usize>>) -> String {
    let Ok(read) = fs::read_dir(dir) else { return String::new() };
    let mut names: Vec<String> = Vec::new();
    let mut lower: Vec<String> = Vec::new();
    let mut readme: Option<PathBuf> = None;
    for e in read.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), ".DS_Store" | "desktop.ini" | "Thumbs.db") {
            continue;
        }
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        let l = name.to_lowercase();
        if !is_dir && l.starts_with("readme") && readme.is_none() {
            readme = Some(e.path());
        }
        lower.push(l);
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    if names.is_empty() {
        return String::new();
    }
    names.sort_unstable_by_key(|n| n.to_lowercase());
    let has = |n: &str| lower.iter().any(|x| x == n);
    let has_ext = |ext: &str| lower.iter().any(|x| x.ends_with(ext));

    let mut kinds: Vec<String> = Vec::new();
    let mut about: Vec<String> = Vec::new();
    let mut uses: BTreeSet<String> = BTreeSet::new();

    if has("package.json") {
        kinds.push("JavaScript/Node.js project".into());
        if let Some(pkg) = read_json(&dir.join("package.json")) {
            for key in ["name", "description"] {
                if let Some(s) = pkg[key].as_str().filter(|s| !s.trim().is_empty()) {
                    about.push(format!("{key}: {}", s.trim()));
                }
            }
            if let Some(kw) = pkg["keywords"].as_array() {
                let kw: Vec<&str> = kw.iter().filter_map(Value::as_str).collect();
                if !kw.is_empty() {
                    about.push(format!("keywords: {}", kw.join(", ")));
                }
            }
            for section in ["dependencies", "devDependencies"] {
                if let Some(obj) = pkg[section].as_object() {
                    uses.extend(obj.keys().cloned());
                }
            }
        }
        let frameworks: Vec<&str> = WEB_FRAMEWORKS
            .iter()
            .filter(|(dep, _)| uses.contains(*dep))
            .map(|(_, label)| *label)
            .collect();
        if !frameworks.is_empty() {
            kinds.push(format!("web app ({})", frameworks.join(", ")));
        }
    }
    if has("index.html") {
        kinds.push("website (index.html)".into());
    }
    if has("cargo.toml") {
        kinds.push("Rust project".into());
        if let Some(d) = toml_description(&dir.join("Cargo.toml")) {
            about.push(format!("description: {d}"));
        }
    }
    if has("tauri.conf.json") || has("src-tauri") {
        kinds.push("Tauri desktop app".into());
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        kinds.push("Python project".into());
        if let Some(d) = toml_description(&dir.join("pyproject.toml")) {
            about.push(format!("description: {d}"));
        }
    }
    if has("go.mod") {
        kinds.push("Go project".into());
    }
    if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
        kinds.push("Java/Kotlin project".into());
    }
    if has_ext(".csproj") || has_ext(".sln") {
        kinds.push(".NET/C# project".into());
    }
    if has("composer.json") {
        kinds.push("PHP project".into());
    }
    if has("gemfile") {
        kinds.push("Ruby project".into());
    }
    if has("pubspec.yaml") {
        kinds.push("Flutter/Dart app".into());
    }
    if has("androidmanifest.xml") {
        kinds.push("Android app".into());
    }
    if has("manifest.json") && !has("package.json") {
        kinds.push("browser extension or web app manifest".into());
    }
    if has("dockerfile") || has("docker-compose.yml") || has("compose.yaml") {
        kinds.push("Docker".into());
    }
    if has(".git") {
        kinds.push("Git repository".into());
    }

    let folder_name = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut text = format!("Folder: {folder_name}\nPath: {}", dir.display());
    if !kinds.is_empty() {
        text.push_str(PROJECT_MARKER);
        text.push_str(&kinds.join("; "));
    }
    if !about.is_empty() {
        text.push_str(&format!("\nAbout: {}", about.join("; ")));
    }
    if !uses.is_empty() {
        let list: Vec<&str> = uses.iter().take(MAX_DEPS).map(String::as_str).collect();
        text.push_str(&format!("\nUses: {}", list.join(", ")));
    }
    if let Some(langs) = langs.filter(|l| !l.is_empty()) {
        let mut v: Vec<(&&str, &usize)> = langs.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let list: Vec<String> = v.iter().take(6).map(|(l, n)| format!("{l} {n}")).collect();
        text.push_str(&format!("\nFiles by language (everything inside): {}", list.join(", ")));
    }
    if let Some(intro) = readme.as_deref().and_then(readme_intro) {
        text.push_str(&format!("\nREADME: {intro}"));
    }
    let total = names.len();
    names.truncate(MAX_NAMES);
    text.push_str(&format!("\nContains: {}", names.join(", ")));
    if total > MAX_NAMES {
        text.push_str(&format!(", … ({} more)", total - MAX_NAMES));
    }
    truncate(&text, MAX_SUMMARY_CHARS).to_string()
}

/// A stable fingerprint of a summary, so unchanged folders aren't re-embedded (FNV-1a).
pub fn fingerprint(text: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h as i64
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = read_head(path, 256 * 1024)?;
    serde_json::from_slice(&bytes).ok()
}

/// `description = "..."` from a Cargo.toml or pyproject.toml, without a TOML parser.
fn toml_description(path: &Path) -> Option<String> {
    let bytes = read_head(path, 64 * 1024)?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "description" {
            return None;
        }
        let v = value.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The first few hundred characters of a README, on one line.
fn readme_intro(path: &Path) -> Option<String> {
    let bytes = read_head(path, 16 * 1024)?;
    if bytes.contains(&0) {
        return None;
    }
    let cleaned = chunk::clean(&String::from_utf8_lossy(&bytes));
    let one_line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let one_line = one_line.trim_start_matches(['#', ' ']);
    if one_line.is_empty() {
        return None;
    }
    let mut s = truncate(one_line, README_CHARS).to_string();
    if s.len() < one_line.len() {
        s.push('…');
    }
    Some(s)
}

fn read_head(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("docspace-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn summarises_a_js_web_app() {
        let d = temp_dir("js");
        fs::write(
            d.join("package.json"),
            r#"{"name":"pawpal","description":"Book dog walkers near you","keywords":["dogs","walking"],
                "dependencies":{"react":"^18","express":"^4"},"devDependencies":{"vite":"^5"}}"#,
        )
        .unwrap();
        fs::write(d.join("README.md"), "# PawPal\n\nSchedule **dog walks** and pay walkers.\n").unwrap();
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join(".git")).unwrap();
        fs::write(d.join("src").join("app.jsx"), "export default 1").unwrap();

        let files = [d.join("src").join("app.jsx")];
        let langs = count_languages(&d, files.iter().map(|p| p.as_path()));
        let s = folder_summary(&d, langs.get(&d));

        assert!(s.contains(PROJECT_MARKER), "{s}");
        assert!(s.contains("JavaScript/Node.js project"));
        assert!(s.contains("web app (React, Vite, Express)"), "{s}");
        assert!(s.contains("Git repository"));
        assert!(s.contains("description: Book dog walkers near you"));
        assert!(s.contains("keywords: dogs, walking"));
        assert!(s.contains("Uses: express, react, vite"));
        assert!(s.contains("Files by language (everything inside): JavaScript 1"));
        assert!(s.contains("README: PawPal Schedule **dog walks** and pay walkers."), "{s}");
        assert!(s.contains("Contains: .git/, package.json, README.md, src/"), "{s}");
        assert!(s.len() <= MAX_SUMMARY_CHARS);
        assert_eq!(fingerprint(&s), fingerprint(&s.clone()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn plain_folder_has_no_project_marker() {
        let d = temp_dir("plain");
        fs::write(d.join("notes.txt"), "hello").unwrap();
        let s = folder_summary(&d, None);
        assert!(!s.contains(PROJECT_MARKER));
        assert!(s.contains("Contains: notes.txt"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn languages_roll_up_to_every_parent() {
        let root = PathBuf::from("/r");
        let files = [PathBuf::from("/r/a/b/x.js"), PathBuf::from("/r/a/y.py"), PathBuf::from("/r/z.md")];
        let c = count_languages(&root, files.iter().map(|p| p.as_path()));
        assert_eq!(c[&root]["JavaScript"], 1);
        assert_eq!(c[&root]["text"], 1);
        assert_eq!(c[&PathBuf::from("/r/a")]["Python"], 1);
        assert_eq!(c[&PathBuf::from("/r/a/b")].len(), 1);
        assert!(!c.contains_key(&PathBuf::from("/")));
    }
}
