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

**2. Double-click `Setup DocSpace.bat`.** It installs whatever is missing: the C++ Build Tools,
WebView2, Rust, the Tauri CLI and the two Ollama models. It doesn't need `winget`, and it's safe
to run again. The first run downloads a few GB and can take 20-30 minutes. Approve the Windows
admin prompt when it appears. Ollama itself must already be installed and running.

**3. Double-click `Start DocSpace.bat`.** The first start compiles the app and takes about
3-8 minutes. After that it opens in seconds.

The PowerShell equivalents, run from the `docspace-app` folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
powershell -ExecutionPolicy Bypass -File .\run.ps1
powershell -ExecutionPolicy Bypass -File .\run.ps1 -Installer   # optional: build a Windows installer
```

## Updating

New releases arrive as `docspace-app-vX.Y.Z.zip`. Save them to your **Downloads** folder; you
don't need to unzip them.

- **Start DocSpace.bat** checks Downloads every time it starts and installs a newer release
  automatically.
- **Update DocSpace.bat** does the same without starting the app.

Each update copies the new files over your `docspace-app` folder. It keeps the compiled build
cache, so only changed code is recompiled, and it never touches your index or tags, which
live in `%APPDATA%`. Afterwards it deletes older DocSpace zips from Downloads, keeping only the
newest two: the current version and the previous one, for rolling back. To roll back:

```powershell
powershell -ExecutionPolicy Bypass -File .\update.ps1 -Version 0.2.0   # e.g. back to 0.2.0 after 0.3.0 is installed
```

Add `-NoCleanup` to keep every old zip. `CHANGELOG.md` lists what changed in each version.

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
├─ Start DocSpace.bat      double-click: update if needed, then start
├─ Update DocSpace.bat     double-click: install the newest release from Downloads
├─ Setup DocSpace.bat      double-click: install prerequisites
├─ setup.ps1               installs prerequisites (run once)
├─ update.ps1              the updater
├─ CHANGELOG.md            what changed in each version
├─ run.ps1                 starts the app, or builds the installer with -Installer
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
| `linker 'link.exe' not found` | The Build Tools install didn't finish. Run `setup.ps1` again. |
| `cargo` / `ollama` is not recognized | Open a new PowerShell window. `run.ps1` finds Rust on its own. |
| Red "Ollama not reachable" pill | Start Ollama (Start menu → Ollama), then click the pill to re-check. |
| "Ollama doesn't have `…` yet" | `ollama pull <that model>` |
| "This index was built with `…`" | You changed the embedding model. Change it back, or **Reset index** and re-index. |
| First build very slow | Normal. Windows Defender scanning `target\` also slows it; you can add the project folder as an exclusion. |
