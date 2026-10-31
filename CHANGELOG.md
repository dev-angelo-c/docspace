# DocSpace changelog

## v0.5.1 (2026-10-31)

- One launcher: `DocSpace.bat` + `docspace.ps1` replace Start/Update/Setup DocSpace.bat, run.ps1, update.ps1 and setup.ps1.
- Each run installs missing prerequisites, picks up a newer zip from Downloads, keeps this version's zip and the most recent prior zip, deletes older zips and every other unzipped DocSpace copy, then builds and runs.
- Shared build cache at `C:\cargo-target\docspace` (this window only, no global setting), so new versions no longer recompile every crate. The first run moves an existing cache there.
- `-Release` builds the installer; `-DryRun` lists what would be deleted.

## v0.5.0 (2026-10-24)

- Tags replace ratings. Add your own tags to any file or folder ("+ tag" on sources and folder cards), remove them with x, and browse everything by tag in the new Tags window. Ratings and the Rated window are gone, and stored ratings are deleted. Tags are stored by file path, ready for the file explorer.
- Pictures are classified only by the picture model you choose, and only if that model can read pictures. The Picture model setting is now a menu of installed picture-capable models, plus None. There's no automatic switching. Pictures that weren't classified are listed under Skipped as "Needs a picture model".

## v0.4.0 (2026-10-24)

- Automatic picture model: "Picture model" now defaults to auto. When a folder has pictures, DocSpace uses your smallest installed vision model (for example qwen2.5vl:3b) without you switching anything. "off" skips pictures; a model name forces that model.
- Check my computer (Settings): times each installed answer model on this computer, estimates a typical answer time for each, recommends one ("Use qwen3:4b"), and tests whether CPU-only is faster.
- "Run models on the CPU only" setting: tells Ollama to keep models on the CPU without restarting it.
- The answer-model menu shows each model's measured answer time and lists picture models separately; choosing a picture model for answers asks first.
- Fixed: saving Settings no longer resets "Write an answer".

## v0.3.4 (2026-10-23)

- Fixed: confirmation boxes (Remove, Reset index, subfolders off, Include all, Clear log) were blocked by a missing permission, and actions could go ahead without waiting for your answer. They now ask and wait.
- Fixed: "database is locked" when two changes ran at once (for example clicking Remove twice). Writes now wait their turn, and a second click on Remove is ignored while the first runs.
- The log now records each indexing run's picture model, subfolder setting and number of pictures described.

## v0.3.3 (2026-10-23)

- "Include subfolders" for each folder. Off = only the files directly inside the folder are indexed. Set it before adding a folder ("Include subfolders when adding", under + Add folder) or change it on the folder's card, which re-indexes the folder. Turning it off removes the subfolders' files from the index; your files aren't touched.

## v0.3.2 (2026-10-22)

- "Write an answer" switch under the question box. Off = sources only: matching files appear in seconds with a two-line excerpt of the matching text, and the answer model isn't used at all. Up to 12 file matches are shown (plus folders), since nothing has to fit in a model's context.
- In sources-only mode the embedding model stays loaded between searches, so repeat searches are near-instant.

## v0.3.1 (2026-10-22)

- Fixed two build errors in v0.3.0 (a stray pair of quotes in the answer instructions, and an ambiguous type in the file scanner).

## v0.3.0 (2026-10-22)

- Pictures: set a "Picture model" in Settings (for example moondream) and indexing describes each .jpg, .png, .webp, .gif and .bmp picture in a few sentences. The descriptions are searchable, so questions like "photos of a dog on a beach" find matching pictures. Off by default.
- Pictures under 16 KB (icons, thumbnails) are ignored; pictures over 25 MB are listed under Skipped.
- Sources from pictures are marked "(picture)".

## v0.2.6 (2026-10-22)

- While the model reads: "The model is reading about 3,240 tokens before it writes... 0:45".
- While it writes: a live counter under the answer ("Writing... 142 tokens, 4.6 tokens/s").
- When it's done: Ollama's own figures ("Model loaded in 14s, read 3,612 tokens in 48s, wrote 96 tokens in 21s (4.5 tokens/s)").
- Reasoning models show how many thinking tokens they've produced.

## v0.2.5 (2026-10-22)

- Error log: "Log" in the top bar shows errors, indexing runs and answer times, newest last. "Copy" adds your version and settings, ready to paste into a chat. Stored in %APPDATA%\app.docspace.desktop\docspace.log.
- The answer status shows a running timer ("Writing the answer... 0:45") and explains slow first model loads.

## v0.2.4 (2026-10-22)

- Skip this file and Stop respond in seconds instead of minutes: text goes to Ollama 16 chunks per request instead of 64, and both buttons are checked between requests.
- Stop now works in the middle of a big file. Finished files are kept; the file in progress is read again on the next Re-index.

## v0.2.3 (2026-10-22)

- New "Skipped" button in the top bar: every file left out of the index, with the reason (skipped by you, too large, large data file, encoded or minified) and its size.
- "Include" on any of them (or "Include all shown" after filtering, e.g. by ".md") indexes it on the next Re-index. Size limits and the encoded-data check stop applying to included files.
- The skipped list moved out of Settings into its own window.

## v0.2.2 (2026-10-22)

- "Skip this file" on the progress card: drops the file being indexed within one batch and leaves it out of every later run. Settings lists skipped files, with "Index again" to undo.
- Files that are really encoded or minified data (get-pip.py, base64 blobs, minified bundles) are detected and skipped automatically.
- The progress card shows the file path and the chunk count on separate lines, so long paths don't hide the file name.
- If Ollama doesn't answer an embedding request for 10 minutes, indexing stops with a message instead of waiting forever. Finished files are kept.

## v0.2.1 (2026-10-22)

- The progress card now moves while a big file is being embedded ("embedding 320 of 1,500 chunks").
- Data-style files (.json, .xml, .log, .csv, .tsv) over 256 KB are skipped, such as large bookmark exports. They were slow to embed and rarely useful for questions.
- run.ps1 -Installer finds the installer when projects share a build folder (CARGO_TARGET_DIR).
- Removed a harmless compiler warning.

## v0.2.0 (2026-10-22)

- Folder summaries: questions that mention folders, projects or apps list up to 15 matching projects, one each.
- Star ratings (1-5) for files and folders: under each answer's sources and on folder cards. "Rated" in the top bar lists them.
- Answer-model picker next to the question box. Switching unloads the previous model to free memory.
- Updater: update.ps1 installs the newest docspace-app-vX.Y.Z.zip from Downloads and keeps only the newest two zips there.
- Double-click launchers: Start DocSpace.bat, Update DocSpace.bat, Setup DocSpace.bat.
- Skips Windows system and hidden folders (Windows, Program Files, AppData, ...) as well as node_modules and .git.
- Version number shown next to the DocSpace name.

## v0.1.0 (2026-10-22)

- First release: index folders with Ollama embeddings into SQLite + sqlite-vec, ask questions, answers cite their sources.
