//! Question → answer: embed the question, find the nearest chunks in SQLite,
//! fit as many as the context window allows, and stream an answer from the chat model.

use crate::db::{self, Hit};
use crate::indexer::normalize;
use crate::ollama::{embed_prefixes, Message, Ollama, Token};
use crate::summary::PROJECT_MARKER;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Folder summaries sent for a "which folders / projects / apps…" question.
const FOLDER_HITS: usize = 15;
/// Folder summaries added to an ordinary question, for context.
const FOLDER_HITS_OTHERWISE: usize = 2;
/// Words that make a question about folders rather than about file contents.
const FOLDER_WORDS: &[&str] = &[
    "folder", "folders", "directory", "directories", "dir", "dirs", "project", "projects", "repo", "repos",
    "repository", "repositories", "codebase", "codebases", "app", "apps", "application", "applications",
    "website", "websites", "site", "sites",
];

/// Tokens reserved for the answer.
const ANSWER_TOKENS: u32 = 1024;
/// Conservative characters-per-token estimate (prose ≈ 4, code and paths ≈ 3).
const CHARS_PER_TOKEN: usize = 3;

const SYSTEM_PROMPT: &str = "\
You are DocSpace, an assistant that answers questions about the user's local files.
You are given numbered excerpts found by semantic search. There are two kinds:
- file excerpts: a piece of a document or source file, with its path (an excerpt starting
  Picture: is a description of an image file, written by a vision model);
- folder summaries: one per folder, built from its files: what kind of project it looks like, its
  description and libraries, how many files of each language it holds, the start of its README,
  and the names of its entries.
Rules:
- Answer only from the excerpts. If they don't contain the answer, say so plainly instead of guessing.
- Cite the excerpts you rely on inline, like [2]. Mention file or folder paths when they help.
- For questions about which folders, projects or apps match, list each matching folder once as a
  bullet: its full path, a few words on why it matches, and its citation. Leave out folders that
  don't match.
- The excerpts are the closest matches, not everything on disk. For \"find all\" questions, \
say the list may be incomplete.
- Be concise. Use Markdown for lists and code.";

pub struct AskParams<'a> {
    pub chat_model: &'a str,
    /// false = "sources only": stop after finding the passages, no answer model.
    pub write_answer: bool,
    pub top_k: usize,
    pub num_ctx: u32,
}

pub enum Event<'a> {
    /// The passages being sent, and roughly how many tokens the model must read first.
    Sources(&'a [Hit], usize),
    /// One piece of reasoning text arrived (reasoning models only).
    Thinking,
    Token(&'a str),
    /// The answer is complete: Ollama's token counts and timings.
    Done(crate::ollama::ChatStats),
}

pub fn ask(conn: &Connection, ollama: &Ollama, p: &AskParams, question: &str, mut emit: impl FnMut(Event<'_>)) -> Result<Vec<Hit>> {
    let Some((index_model, dim)) = db::index_model(conn)? else {
        bail!("Nothing is indexed yet. Add a folder first.");
    };
    if db::chunk_count(conn)? == 0 {
        bail!("Nothing is indexed yet. Add a folder first.");
    }
    if p.write_answer {
        ollama.ensure_model(p.chat_model)?;
    }

    // Always embed questions with the model that built the index; vectors from
    // different models aren't comparable.
    let (_, query_prefix) = embed_prefixes(&index_model);
    // When an answer follows, unload the embedder straight away so the chat model has the
    // memory. In sources-only mode keep it loaded (Ollama's 5 minutes) so the next search is instant.
    let keep_alive = if p.write_answer { Some(0) } else { None };
    let mut q = ollama
        .embed(&index_model, &[format!("{query_prefix}{question}")], keep_alive)?
        .pop()
        .context("Ollama returned no embedding for the question")?;
    if q.len() != dim {
        bail!("The question embedding has {} dimensions but the index has {dim}.", q.len());
    }
    normalize(&mut q);

    // File chunks and folder summaries are searched separately. A question about folders gets
    // mostly folder summaries (one per project); any other question gets mostly file chunks.
    // Sources only: nothing has to fit in a model's context, so show more matches.
    let top_k = if p.write_answer { p.top_k.max(1) } else { p.top_k.max(12) };
    let hits: Vec<Hit> = if is_folder_question(question) {
        let folders = distinct_folders(db::search(conn, db::FOLDER_VECS, &q, FOLDER_HITS * 4)?, FOLDER_HITS);
        let files = db::search(conn, db::FILE_VECS, &q, (top_k / 2).max(3))?;
        folders.into_iter().chain(files).collect()
    } else {
        let files = db::search(conn, db::FILE_VECS, &q, top_k)?;
        let folders = distinct_folders(db::search(conn, db::FOLDER_VECS, &q, 10)?, FOLDER_HITS_OTHERWISE);
        files.into_iter().chain(folders).collect()
    };
    if !p.write_answer {
        if hits.is_empty() {
            bail!("Nothing relevant was found in the index.");
        }
        let mut sources = hits;
        attach_tags(conn, &mut sources)?;
        emit(Event::Sources(&sources, 0));
        return Ok(sources);
    }

    let budget = context_budget_chars(p.num_ctx, question);
    let (context, used) = build_context(&hits, budget);
    if used.is_empty() {
        bail!("Nothing relevant was found in the index.");
    }
    let mut sources: Vec<Hit> = used.iter().map(|&i| hits[i].clone()).collect();
    attach_tags(conn, &mut sources)?;
    let messages = [
        Message::system(SYSTEM_PROMPT),
        Message::user(format!("Excerpts:\n\n{context}\nQuestion: {question}")),
    ];
    // About 4 characters per token for English prose: shown while the model reads.
    let prompt_chars: usize = messages.iter().map(|m| m.content.len()).sum();
    emit(Event::Sources(&sources, prompt_chars / 4));

    let stats = ollama.chat(p.chat_model, &messages, p.num_ctx, ANSWER_TOKENS, |t| match t {
        Token::Thinking => emit(Event::Thinking),
        Token::Answer(s) => emit(Event::Token(s)),
    })?;
    emit(Event::Done(stats));
    Ok(sources)
}

fn attach_tags(conn: &Connection, sources: &mut [Hit]) -> Result<()> {
    let paths: Vec<String> = sources.iter().map(|h| h.path.clone()).collect();
    let mut tags = db::tags_for(conn, &paths)?;
    for h in sources.iter_mut() {
        h.tags = tags.remove(&h.path).unwrap_or_default();
    }
    Ok(())
}

fn context_budget_chars(num_ctx: u32, question: &str) -> usize {
    let est = |s: &str| s.len() / CHARS_PER_TOKEN + 1;
    let fixed = est(SYSTEM_PROMPT) + est(question) + 96; // + chat template scaffolding
    (num_ctx as usize).saturating_sub(ANSWER_TOKENS as usize).saturating_sub(fixed) * CHARS_PER_TOKEN
}

/// Pack hits best-first until the budget is used. Returns the context and which hits made it in;
/// they are numbered [1], [2], … in that order, matching the sources list.
fn build_context(hits: &[Hit], budget: usize) -> (String, Vec<usize>) {
    let mut context = String::new();
    let mut used = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        let kind = if h.is_dir { "folder summary" } else { "file" };
        let header = format!("[{}] {kind}: {}\n", used.len() + 1, h.path);
        let remaining = budget.saturating_sub(context.len());
        if header.len() + h.text.len() + 2 <= remaining {
            context.push_str(&header);
            context.push_str(&h.text);
        } else if used.is_empty() && remaining > header.len() + 200 {
            // Tiny context window: include as much of the best chunk as fits.
            context.push_str(&header);
            context.push_str(truncate(&h.text, remaining - header.len() - 2));
        } else {
            break;
        }
        context.push_str("\n\n");
        used.push(i);
    }
    (context, used)
}

fn is_folder_question(question: &str) -> bool {
    question
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| FOLDER_WORDS.contains(&w))
}

/// Keep the best folder hits, one per project: subfolders of a listed project (its src/,
/// components/…) are dropped, a project replaces plain subfolders of it listed earlier, and
/// a plain folder that only contains something already listed is skipped.
fn distinct_folders(hits: Vec<Hit>, n: usize) -> Vec<Hit> {
    let is_project = |h: &Hit| h.text.contains(PROJECT_MARKER);
    let mut kept: Vec<Hit> = Vec::new();
    for h in hits {
        let p = PathBuf::from(&h.path);
        if kept.iter().any(|k| is_project(k) && p.starts_with(&k.path)) {
            continue;
        }
        if is_project(&h) {
            kept.retain(|k| is_project(k) || !Path::new(&k.path).starts_with(&p));
        } else if kept.iter().any(|k| Path::new(&k.path).starts_with(&p)) {
            continue;
        }
        kept.push(h);
        if kept.len() >= n {
            break;
        }
    }
    kept
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

    fn hit(path: &str, project: bool, score: f32) -> Hit {
        let text = if project {
            format!("Folder: x\nPath: {path}{PROJECT_MARKER}JavaScript/Node.js project")
        } else {
            format!("Folder: x\nPath: {path}\nContains: a")
        };
        Hit { path: path.into(), is_dir: true, text, score, tags: Vec::new() }
    }

    #[test]
    fn folder_questions() {
        assert!(is_folder_question("Show me all folders with JavaScript apps about dog walking"));
        assert!(is_folder_question("which projects use React?"));
        assert!(!is_folder_question("What does the lease say about pets?"));
        assert!(!is_folder_question("happy apple")); // whole words only
    }

    #[test]
    fn one_hit_per_project() {
        let hits = vec![
            hit("/code/pawpal/src", false, 0.9),       // plain subfolder first...
            hit("/code/pawpal", true, 0.85),           // ...replaced by its project
            hit("/code/pawpal/web", false, 0.8),       // inside a listed project: dropped
            hit("/code", false, 0.7),                  // only contains a listed project: dropped
            hit("/code/walkies", true, 0.6),
            hit("/old/dogs", false, 0.5),
        ];
        let kept: Vec<String> = distinct_folders(hits, 10).into_iter().map(|h| h.path).collect();
        assert_eq!(kept, vec!["/code/pawpal", "/code/walkies", "/old/dogs"]);
    }

    #[test]
    fn stops_at_n() {
        let hits = (0..10).map(|i| hit(&format!("/p{i}"), true, 1.0)).collect();
        assert_eq!(distinct_folders(hits, 4).len(), 4);
    }
}
