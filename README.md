# DocSpace

Point it at a folder, then ask questions about what's inside.

DocSpace reads every document and source file in the folder (text, code, Markdown, PDFs) and
splits the text into chunks. Ollama's `nomic-embed-text` turns each chunk into a vector, and the
vectors are stored in a local SQLite database with the `sqlite-vec` extension. To answer a
question, it finds the chunks closest in meaning and has your Ollama chat model (default
`qwen3:8b`) write an answer, with numbered links back to the files it used.

Everything runs on your machine. Your files are only read, never changed.

---

## Setup (Windows 10/11, PowerShell)

Everything below is typed into **Windows PowerShell** (Start menu, type "PowerShell").

**1. Unzip** the release zip (for example `docspace-app-v0.2.0.zip`) into the folder you want to
run it from: right-click it, **Extract All…**. Open the `docspace-app` folder inside.

**2. Double-click `DocSpace.bat`.** It's the only launcher, and every run does the same four things:

1. Installs anything missing: the C++ Build Tools, WebView2, Rust, the Tauri CLI and the two Ollama
   models. The very first run downloads a few GB (20-30 minutes); approve the admin prompt.
   Ollama itself must already be installed and running.
2. If Downloads has a DocSpace zip newer than the copy you started, it unzips that and continues there.
3. Cleans up Downloads: keeps this version's zip and the most recent prior zip, and deletes older
   DocSpace zips and every other unzipped DocSpace folder.
4. Builds and runs DocSpace with one shared build cache, `C:\cargo-target\docspace`, so a new
   version only recompiles what changed.

Your index database, tags and settings live in `%APPDATA%` and are never touched.

The PowerShell equivalents, run from the `docspace-app` folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\docspace.ps1            # build + run (development)
powershell -ExecutionPolicy Bypass -File .\docspace.ps1 -Release   # build the Windows installer
powershell -ExecutionPolicy Bypass -File .\docspace.ps1 -Setup     # only check prerequisites
powershell -ExecutionPolicy Bypass -File .\docspace.ps1 -DryRun    # list what would be deleted, do nothing
```

## Updating

Save the new `docspace-app-vX.Y.Z.zip` to **Downloads** and double-click `DocSpace.bat` in either
the old or the new copy. To roll back, unzip the kept prior zip and run its `DocSpace.bat`.
`CHANGELOG.md` lists what changed in each version.

---

## Using it

1. **Add folder** opens a folder picker. Indexing starts right away, with a progress bar, and
   **Stop** keeps whatever was already processed.
2. Ask a question at the bottom. Sources appear under the answer: click a path to open the file,
   or **Show** to open its folder in Explorer. Click a `[2]` chip in the answer to highlight
   that source.
3. **Re-index** a folder after its files change. Only new or changed files are embedded again,
   and deleted files are dropped from the index.
4. The **gear** opens Settings: Ollama address, embedding model, answer model, how many chunks
   go into each answer, and the context window.

The database is `%APPDATA%\app.docspace.desktop\docspace.db`, which you can delete to start
over (or use **Reset index** in Settings).

### What gets indexed

- **Included:** text, Markdown, code and config files up to 2 MB, and PDFs up to 100 MB.
- **Skipped:** hidden files, anything matched by a `.gitignore`, `.git`, `node_modules`, `target`,
  `dist`, `build`, `bin`, `obj`, `.venv` and similar folders, and lock files.
- **Folder summaries:** each folder also gets a short summary built from its own files, without
  the AI model: the project type (from `package.json`, `Cargo.toml`, `index.html`…), its name,
  description and libraries, file counts per language, the start of its README, and its entry
  names. Summaries live in the database, never in your folders. Questions that mention folders,
  projects or apps search these first and get one result per project, so "show me all folders
  with JavaScript dog-walking apps" can list up to 15 projects.
- **Encoded or minified files:** files that are mostly one long run of characters (such as
  `get-pip.py`, base64 data or minified bundles) are detected and skipped.
- **Skip this file:** while indexing, the progress card can drop the current file and leave it
  out of future runs.
- **Skipped** (top bar) lists every file left out, with the reason and size. **Include** one, or
  filter (for example by `.md`) and **Include all shown**; they're indexed on the next Re-index,
  and the size limits no longer apply to them.
- **Scanned PDFs:** they're images with no text layer, so they're counted and reported after
  indexing instead of indexed.

### Resource use

- **While indexing:** only the embedding model is loaded. DocSpace tells Ollama to unload it
  when indexing finishes.
- **For each question:** the question is embedded and the embedding model is unloaded straight
  away. Then the chat model loads.
- **Context window:** set explicitly (8192 tokens by default). DocSpace only adds as many chunks
  as fit, so an 8B model is never silently cut off.

---

## Project layout

```
docspace-app/
├─ DocSpace.bat           double-click: set up, clean up, build and run
├─ docspace.ps1            the one script (-Release, -Setup, -DryRun)
├─ CHANGELOG.md            what changed in each version
├─ src/                    the UI (plain HTML/CSS/JS, no Node needed)
│  ├─ index.html
│  ├─ styles.css
│  └─ main.js
└─ src-tauri/              the Rust backend
   ├─ Cargo.toml
   ├─ tauri.conf.json
   ├─ capabilities/default.json
   ├─ icons/
   └─ src/
      ├─ lib.rs            Tauri commands the UI calls
      ├─ db.rs             SQLite + sqlite-vec: schema, insert, KNN search
      ├─ indexer.rs        folder → text → chunks → Ollama embeddings → database
      ├─ rag.rs            question → nearest chunks → streamed answer
      ├─ ollama.rs         Ollama HTTP client (embed, chat, unload)
      ├─ scan.rs           folder walking, skip rules, PDF/text extraction
      ├─ summary.rs        folder summaries built from each folder's files (with tests)
      └─ chunk.rs          text cleanup + overlapping chunking (with tests)
```

Run the unit tests with `cd src-tauri; cargo test` in PowerShell.

## Troubleshooting

| Problem | Fix |
|---|---|
| `linker 'link.exe' not found` | The Build Tools install didn't finish. Run `DocSpace.bat -Setup` again. |
| `cargo` / `ollama` is not recognized | Open a new PowerShell window. `docspace.ps1` finds Rust on its own. |
| Red "Ollama not reachable" pill | Start Ollama (Start menu → Ollama), then click the pill to re-check. |
| "Ollama doesn't have `…` yet" | `ollama pull <that model>` |
| "This index was built with `…`" | You changed the embedding model. Change it back, or **Reset index** and re-index. |
| First build very slow | Normal. Windows Defender scanning the build cache also slows it; you can add `C:\cargo-target` as an exclusion. |
