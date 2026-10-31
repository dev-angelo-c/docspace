// DocSpace front end. Talks to the Rust backend via Tauri's global API (withGlobalTauri: true).
const rawInvoke = window.__TAURI__.core.invoke;

// Yes/No question. In Tauri the dialog plugin replaces window.confirm with a native dialog
// that answers later (a Promise), so always await it.
async function askYesNo(message) {
  try {
    const answer = window.confirm(message);
    return answer instanceof Promise ? await answer : !!answer;
  } catch {
    return false;
  }
}

// ---------------- error log ----------------
// Every failed command is written to docspace.log (the backend also logs indexing and answers).
const SERVER_LOGGED = new Set(["ask", "index_folder", "log_client", "read_log"]);
function logClient(level, message) {
  rawInvoke("log_client", { level, message: String(message).slice(0, 4000) }).catch(() => {});
}
async function invoke(cmd, args) {
  try {
    return await rawInvoke(cmd, args);
  } catch (e) {
    if (!SERVER_LOGGED.has(cmd)) logClient("error", `${cmd} failed: ${e}`);
    throw e;
  }
}
window.addEventListener("error", (e) => logClient("error", `script error: ${e.message} (${e.filename}:${e.lineno})`));
window.addEventListener("unhandledrejection", (e) => logClient("error", `unhandled: ${e.reason}`));
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const state = { overview: null, asking: false, nextId: 1, cards: new Map(), indexingPath: null };

// ---------------- helpers ----------------
function toast(msg, bad = false, ms = 5000) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.toggle("bad", bad);
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), ms);
}
const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const baseName = (p) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
function ago(unixSecs) {
  if (!unixSecs) return "indexing not finished";
  const s = Date.now() / 1000 - unixSecs;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(unixSecs * 1000).toLocaleDateString();
}
const plural = (n, w) => `${n.toLocaleString()} ${w}${n === 1 ? "" : "s"}`;

// ---------------- tags ----------------
// Tag chips for a file or folder, with × to remove and "+ tag" to add one. Every chip row
// showing the same path is kept in step.
function tagWidget(path, isDir, tags) {
  const wrap = document.createElement("span");
  wrap.className = "tags";
  wrap.dataset.path = path;
  wrap.dataset.dir = isDir ? "1" : "";
  renderTags(wrap, tags || []);
  return wrap;
}

function renderTags(wrap, tags) {
  wrap.dataset.tags = JSON.stringify(tags);
  wrap.innerHTML = "";
  for (const t of tags) {
    const chip = document.createElement("span");
    chip.className = "tag";
    chip.innerHTML = `${esc(t)}<button type="button" title="Remove tag" aria-label="Remove tag ${esc(t)}">×</button>`;
    chip.querySelector("button").onclick = async (e) => {
      e.stopPropagation();
      try {
        await invoke("remove_tag", { path: wrap.dataset.path, tag: t });
        setTagsEverywhere(wrap.dataset.path, tags.filter((x) => x !== t));
      } catch (err) { toast(String(err), true); }
    };
    wrap.appendChild(chip);
  }
  const add = document.createElement("button");
  add.type = "button";
  add.className = "tag-add";
  add.textContent = "+ tag";
  add.onclick = (e) => { e.stopPropagation(); startTagInput(wrap, add); };
  wrap.appendChild(add);
}

function startTagInput(wrap, addBtn) {
  const input = document.createElement("input");
  input.className = "tag-input";
  input.placeholder = "tag name";
  input.setAttribute("list", "tag-names");
  addBtn.replaceWith(input);
  input.focus();
  let done = false;
  const finish = async (save) => {
    if (done) return;
    done = true;
    const value = input.value.trim();
    const tags = JSON.parse(wrap.dataset.tags || "[]");
    if (save && value) {
      try {
        const stored = await invoke("add_tag", { path: wrap.dataset.path, isDir: !!wrap.dataset.dir, tag: value });
        if (!tags.some((t) => t.toLowerCase() === stored.toLowerCase())) tags.push(stored);
        tags.sort((a, b) => a.localeCompare(b));
        loadTagNames();
      } catch (err) { toast(String(err), true); }
    }
    setTagsEverywhere(wrap.dataset.path, tags);
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); finish(true); }
    if (e.key === "Escape") { e.preventDefault(); finish(false); }
  };
  input.onblur = () => finish(true);
}

function setTagsEverywhere(path, tags) {
  document.querySelectorAll(".tags").forEach((w) => { if (w.dataset.path === path) renderTags(w, tags); });
  if ($("#tagsdlg").open) showTags(state.tagFilter || "");
}

// Suggestions for the tag input: every tag in use.
async function loadTagNames() {
  try {
    const all = await invoke("list_tags");
    state.tagCounts = all;
    let dl = $("#tag-names");
    if (!dl) {
      dl = document.createElement("datalist");
      dl.id = "tag-names";
      document.body.appendChild(dl);
    }
    dl.innerHTML = all.map((t) => `<option value="${esc(t.tag)}">`).join("");
  } catch { /* suggestions are optional */ }
}

async function showTags(filter) {
  state.tagFilter = filter;
  try {
    const [counts, items] = await Promise.all([invoke("list_tags"), invoke("tagged", { tag: filter })]);
    const cloud = $("#tag-cloud");
    cloud.innerHTML = "";
    const allChip = document.createElement("span");
    allChip.className = "tag" + (filter ? "" : " on");
    allChip.textContent = "All";
    allChip.onclick = () => showTags("");
    if (counts.length) cloud.appendChild(allChip);
    for (const c of counts) {
      const chip = document.createElement("span");
      chip.className = "tag" + (filter && filter.toLowerCase() === c.tag.toLowerCase() ? " on" : "");
      chip.innerHTML = `${esc(c.tag)}<span class="n">${c.count}</span>`;
      chip.onclick = () => showTags(c.tag);
      cloud.appendChild(chip);
    }
    const list = $("#tagged-list");
    list.innerHTML = "";
    for (const it of items) {
      const li = document.createElement("li");
      const p = document.createElement("button");
      p.className = "p";
      p.title = `Open ${it.path}`;
      p.innerHTML = `<div class="mono ellipsis"><bdi>${esc(it.path)}</bdi></div><div class="muted when">${it.is_dir ? "Folder" : "File"} · tagged ${ago(it.added_at)}</div>`;
      p.onclick = () => invoke("open_path", { path: it.path }).catch((e) => toast(String(e), true));
      li.appendChild(p);
      li.appendChild(tagWidget(it.path, it.is_dir, it.tags));
      const show = document.createElement("button");
      show.className = "btn small ghost";
      show.textContent = "Show";
      show.onclick = () => invoke("reveal_path", { path: it.path }).catch((e) => toast(String(e), true));
      li.appendChild(show);
      list.appendChild(li);
    }
    $("#tags-empty").hidden = counts.length > 0;
  } catch (e) { toast(String(e), true); }
}

// Tiny Markdown renderer for answers: code blocks, inline code, bold, italics,
// headings, lists, paragraphs, and [n] citations → clickable chips.
function renderMarkdown(src, id) {
  const blocks = [];
  src = src.replace(/```[^\n]*\n?([\s\S]*?)(```|$)/g, (_, code) => {
    blocks.push(`<pre><code>${esc(code.replace(/\n$/, ""))}</code></pre>`);
    return `\u0000${blocks.length - 1}\u0000`;
  });
  const inline = (s) =>
    esc(s)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|[^*])\*([^*\n]+)\*/g, "$1<em>$2</em>")
      .replace(/\[(\d{1,2}(?:\s*,\s*\d{1,2})*)\]/g, (_, nums) =>
        nums.split(/\s*,\s*/).map((n) => `<button class="cite" data-card="${id}" data-n="${n}">${n}</button>`).join(" "));
  let html = "", list = null, para = [];
  const flushPara = () => { if (para.length) { html += `<p>${inline(para.join(" "))}</p>`; para = []; } };
  const closeList = () => { if (list) { html += `</${list}>`; list = null; } };
  for (const line of src.split("\n")) {
    const block = line.match(/^\u0000(\d+)\u0000$/);
    const head = line.match(/^#{1,4}\s+(.*)/);
    const ul = line.match(/^\s*[-*•]\s+(.*)/);
    const ol = line.match(/^\s*\d+[.)]\s+(.*)/);
    if (block) { flushPara(); closeList(); html += blocks[+block[1]]; }
    else if (head) { flushPara(); closeList(); html += `<h4>${inline(head[1])}</h4>`; }
    else if (ul || ol) {
      flushPara();
      const tag = ul ? "ul" : "ol";
      if (list !== tag) { closeList(); html += `<${tag}>`; list = tag; }
      html += `<li>${inline((ul || ol)[1])}</li>`;
    } else if (!line.trim()) { flushPara(); closeList(); }
    else { closeList(); para.push(line.trim()); }
  }
  flushPara(); closeList();
  return html.replace(/\u0000(\d+)\u0000/g, (_, i) => blocks[+i]);
}

// ---------------- Ollama status & settings ----------------
async function checkOllama() {
  const pill = $("#ollama-pill");
  const url = state.overview?.settings.ollama_url || "http://127.0.0.1:11434";
  pill.classList.remove("ok", "bad");
  $("#ollama-text").textContent = "Checking Ollama…";
  try {
    const st = await invoke("check_ollama", { url });
    pill.classList.add("ok");
    $("#ollama-text").textContent = `Ollama ${st.version} · ${plural(st.models.length, "model")}`;
    $("#model-list").innerHTML = st.models.map((m) => `<option value="${esc(m)}">`).join("");
    state.models = st.models;
    state.modelInfo = st.details || [];
    fillModelPicker(st.models, state.overview?.settings.chat_model);
    const s = state.overview?.settings;
    if (s) {
      const has = (m) => st.models.some((x) => x === m || x === `${m}:latest`);
      const missing = [s.embed_model, s.chat_model].filter((m) => !has(m));
      if (missing.length) toast(`Not downloaded in Ollama yet: ${missing.join(", ")}\nRun in PowerShell:  ${missing.map((m) => `ollama pull ${m}`).join("; ")}`, true, 9000);
    }
  } catch (e) {
    pill.classList.add("bad");
    $("#ollama-text").textContent = "Ollama not reachable";
    pill.title = String(e);
  }
}

function openSettings() {
  const s = state.overview.settings;
  const f = $("#settings-form");
  for (const k of ["ollama_url", "embed_model", "chat_model", "top_k", "num_ctx"]) f.elements[k].value = s[k] ?? "";
  f.elements.cpu_only.checked = !!s.cpu_only;
  fillPictureModelSelect(f.elements.vision_model, s.vision_model || "");
  $("#db-path").textContent = `Index file: ${state.overview.db_path}` +
    (state.overview.index_model ? ` · built with ${state.overview.index_model} (${state.overview.dim}-dim)` : "");
  $("#settings").showModal();
}

// ---------------- log window ----------------
// What gets copied: a short header about this setup, then the newest log lines.
async function showLog() {
  let lines = "";
  try { lines = await invoke("read_log"); } catch (e) { lines = String(e); }
  if ($("#log-errors").checked) {
    lines = lines.split("\n").filter((l) => / (ERROR|WARN |PANIC) /.test(l)).join("\n");
  }
  const o = state.overview || {};
  const st = o.settings || {};
  const header = [
    `DocSpace ${$("#app-version").textContent || ""}`.trim(),
    `Ollama: ${$("#ollama-text").textContent}`,
    `Embedding model: ${st.embed_model || "?"} · Answer model: ${st.chat_model || "?"} · Chunks per answer: ${st.top_k ?? "?"} · Context: ${st.num_ctx ?? "?"}`,
    `Index: ${o.chunks ?? "?"} chunks in ${(o.folders || []).length} folder(s)`,
    `System: ${navigator.userAgent.match(/Windows NT [\d.]+/)?.[0] || navigator.platform}`,
    "----",
  ].join("\n");
  $("#log-text").value = `${header}\n${lines || "(no log entries)"}`;
  const ta = $("#log-text");
  ta.scrollTop = ta.scrollHeight;
}

// ---------------- skipped files ----------------
const REASONS = {
  manual: "Skipped by you",
  too_large: "Too large",
  data_file: "Large data file",
  encoded: "Encoded or minified",
  needs_vision: "Needs a picture model",
};
function fmtSize(bytes) {
  if (!bytes) return "";
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}
function whyText(it) {
  const size = fmtSize(it.size);
  switch (it.reason) {
    case "too_large": return `${size}; over the ${/\.pdf$/i.test(it.path) ? "100 MB" : "2 MB"} limit`;
    case "data_file": return `${size}; data files over 256 KB are skipped`;
    case "needs_vision": return `${size}; a picture, and no picture model is chosen (Settings → Picture model)`;
    case "encoded": return `${size ? size + "; " : ""}mostly one long run of characters, not readable text`;
    default: return `${size ? size + "; " : ""}you clicked Skip this file`;
  }
}

async function showSkipped() {
  try { state.skipped = await invoke("list_skipped"); }
  catch (e) { toast(String(e), true); state.skipped = []; }
  renderSkipped();
}

function shownSkipped() {
  const reason = $("#skip-reason").value;
  const q = $("#skip-search").value.trim().toLowerCase();
  return (state.skipped || []).filter((it) => (!reason || it.reason === reason) && (!q || it.path.toLowerCase().includes(q)));
}

function renderSkipped() {
  const all = state.skipped || [];
  const items = shownSkipped();
  const ul = $("#skipped-list");
  ul.innerHTML = "";
  const LIMIT = 500;
  for (const it of items.slice(0, LIMIT)) {
    const li = document.createElement("li");
    li.innerHTML = `<div style="min-width:0"><div class="mono ellipsis" title="${esc(it.path)}"><bdi>${esc(it.path)}</bdi></div>
        <div class="muted why">${esc(whyText(it))}</div></div>
      <span class="reason ${esc(it.reason)}">${esc(REASONS[it.reason] || it.reason)}</span>
      ${it.reason === "needs_vision"
        ? `<button type="button" class="btn small ghost" data-pm="1">Choose a model</button>`
        : `<button type="button" class="btn small">Include</button>`}`;
    li.querySelector("button").onclick = it.reason === "needs_vision"
      ? () => { $("#skipped").close(); openSettings(); }
      : () => includeFiles([it.path]);
    ul.appendChild(li);
  }
  $("#skip-count").textContent = items.length === all.length
    ? plural(all.length, "file")
    : `${items.length.toLocaleString()} of ${plural(all.length, "file")}` + (items.length > LIMIT ? ` (first ${LIMIT} shown)` : "");
  $("#include-all").disabled = items.length === 0;
  const empty = $("#skipped-empty");
  empty.hidden = items.length > 0;
  empty.textContent = all.length ? "No skipped files match the filter." : "Nothing has been skipped yet.";
}

async function includeFiles(paths) {
  if (!paths.length) return;
  try {
    await invoke("include_files", { paths });
    const set = new Set(paths);
    state.skipped = (state.skipped || []).filter((it) => !set.has(it.path));
    renderSkipped();
    toast(paths.length === 1
      ? `${baseName(paths[0])} will be indexed the next time you Re-index its folder.`
      : `${paths.length.toLocaleString()} files will be indexed the next time you Re-index their folders.`);
  } catch (e) { toast(String(e), true); }
}

async function saveSettings(ev) {
  ev.preventDefault();
  const f = ev.target;
  const settings = {
    ...state.overview.settings, // keep settings this form doesn't show (e.g. Write an answer)
    ollama_url: f.elements.ollama_url.value.trim(),
    embed_model: f.elements.embed_model.value.trim(),
    chat_model: f.elements.chat_model.value.trim(),
    top_k: parseInt(f.elements.top_k.value, 10),
    num_ctx: parseInt(f.elements.num_ctx.value, 10),
    vision_model: f.elements.vision_model.value,
    cpu_only: f.elements.cpu_only.checked,
  };
  const im = state.overview.index_model;
  if (im && state.overview.chunks > 0 && im.replace(/:latest$/, "") !== settings.embed_model.replace(/:latest$/, "")) {
    if (!(await askYesNo(`The index was built with ${im}. Vectors from different embedding models can't be mixed, so new folders won't index until you reset the index.\n\nSave anyway?`))) return;
  }
  try {
    await invoke("save_settings", { settings });
    $("#settings").close();
    await refresh();
    checkOllama();
  } catch (e) { toast(String(e), true); }
}

async function resetIndex() {
  if (!(await askYesNo("Delete all indexed text and vectors? Your folders stay in the list and your files are not touched. You'll need to re-index."))) return;
  try {
    await invoke("reset_index");
    $("#settings").close();
    await refresh();
    toast("Index cleared. Use Re-index on each folder.");
  } catch (e) { toast(String(e), true); }
}

// ---------------- folders & indexing ----------------
async function refresh() {
  state.overview = await invoke("overview");
  const { folders, chunks, index_model, settings } = state.overview;
  const ul = $("#folders");
  ul.innerHTML = "";
  for (const f of folders) {
    const li = document.createElement("li");
    li.className = "folder" + (f.indexed_at ? "" : " stale");
    li.innerHTML = `
      <div class="folder-head"><div class="folder-name">${esc(baseName(f.path))}</div></div>
      <div class="folder-path mono muted ellipsis" title="${esc(f.path)}"><bdi>${esc(f.path)}</bdi></div>
      <div class="folder-stats muted">${plural(f.files, "file")} · ${plural(f.chunks, "chunk")}${f.empty ? ` · ${f.empty} without text` : ""} · ${ago(f.indexed_at)}</div>
      <label class="folder-sub muted" title="Off: only the files directly inside this folder are indexed. Changing it re-indexes the folder.">
        <input type="checkbox" data-act="recursive" ${f.recursive ? "checked" : ""} /> Include subfolders
      </label>
      <div class="folder-actions">
        <button class="btn small" data-act="reindex">Re-index</button>
        <button class="btn small ghost" data-act="reveal">Open</button>
        <button class="btn small ghost danger" data-act="remove">Remove</button>
      </div>`;
    li.querySelector(".folder-stats").after(tagWidget(f.path, true, f.tags));
    li.querySelector('[data-act="reindex"]').onclick = () => indexFolder(f.path);
    li.querySelector('[data-act="reveal"]').onclick = () => invoke("open_path", { path: f.path }).catch((e) => toast(String(e), true));
    li.querySelector('[data-act="remove"]').onclick = () => removeFolder(f.path);
    li.querySelector('[data-act="recursive"]').onchange = (e) => setRecursive(f.path, e.target.checked, e.target);
    ul.appendChild(li);
  }
  $("#no-folders").hidden = folders.length > 0;
  $("#index-stats").textContent = chunks
    ? `${plural(chunks, "chunk")} indexed · vectors by ${index_model}`
    : "Index is empty";
  fillModelPicker(state.models || [], settings.chat_model);
  $("#write-answer").checked = settings.write_answer ?? true;
  document.querySelector(".model-pick").classList.toggle("off", !(settings.write_answer ?? true));
  setBusy(state.overview.indexing || state.indexingPath !== null);
}

// Answer-model dropdown: every downloaded model except embedding models.
// Answer models first, labelled with the time "Check my computer" measured; picture (vision)
// models in their own group, since they answer text more slowly and less well.
const sameModel = (a, b) => a === b || a === `${b}:latest` || b === `${a}:latest`;
function modelKind(name) {
  const info = (state.modelInfo || []).find((m) => sameModel(m.name, name));
  if (!info) return /embed/i.test(name) ? "embedding" : "chat";
  if (info.capabilities.includes("embedding")) return "embedding";
  if (info.capabilities.includes("vision")) return "vision";
  return "chat";
}
function benchSecs(name) {
  const row = state.overview?.benchmark?.rows?.find((r) => sameModel(r.model, name));
  return row && typeof row.answer_secs === "number" ? row.answer_secs : null;
}
function fmtDur(secs) {
  if (secs == null || !isFinite(secs)) return "";
  if (secs < 60) return `${Math.round(secs)}s`;
  const m = Math.floor(secs / 60), s = Math.round(secs % 60);
  return s ? `${m}m ${s}s` : `${m}m`;
}
function fillModelPicker(models, current) {
  const sel = $("#model-select");
  const names = models.filter((m) => modelKind(m) !== "embedding");
  if (current && !names.some((m) => sameModel(m, current))) names.unshift(current);
  const option = (m) => {
    const t = benchSecs(m);
    const label = `${m}${t != null ? ` · ~${fmtDur(t)} per answer` : ""}`;
    return `<option value="${esc(m)}"${current && sameModel(m, current) ? " selected" : ""}>${esc(label)}</option>`;
  };
  const chat = names.filter((m) => modelKind(m) !== "vision");
  const vision = names.filter((m) => modelKind(m) === "vision");
  sel.innerHTML = chat.map(option).join("") +
    (vision.length ? `<optgroup label="Picture models (slower, weaker answers)">${vision.map(option).join("")}</optgroup>` : "");
}

// Picture model menu: None, plus only installed models that can read pictures. A saved value
// that can't read pictures (or isn't installed) is shown as such, and isn't used.
function fillPictureModelSelect(sel, current) {
  const vision = (state.modelInfo || []).filter((m) => m.capabilities.includes("vision")).map((m) => m.name);
  const cur = ["auto", "off", "none"].includes(current.toLowerCase()) ? "" : current;
  let html = `<option value="">None (pictures are listed under Skipped)</option>`;
  html += vision.map((m) => `<option value="${esc(m)}"${sameModel(m, cur) ? " selected" : ""}>${esc(m)}</option>`).join("");
  if (cur && !vision.some((m) => sameModel(m, cur))) {
    html += `<option value="${esc(cur)}" selected>${esc(cur)} (can't read pictures, not used)</option>`;
  }
  sel.innerHTML = html;
  if (!cur) sel.value = "";
  $("#picture-note").textContent = vision.length
    ? ""
    : "No installed model can read pictures yet. To add one, run: ollama pull qwen2.5vl:3b (or moondream).";
}

function setBusy(busy) {
  $("#add-folder").disabled = busy;
  document.querySelectorAll('.folder [data-act="reindex"], .folder [data-act="remove"], .folder [data-act="recursive"]').forEach((b) => (b.disabled = busy));
}

async function addFolder() {
  try {
    const path = await invoke("pick_folder");
    if (path) await indexFolder(path, $("#add-recursive").checked);
  } catch (e) { toast(String(e), true); }
}

// Change a folder's "Include subfolders" setting, then re-index it so the change applies.
// Turning it off is quick: files in subfolders are simply removed from the index.
async function setRecursive(path, on, box) {
  if (!on && !(await askYesNo(`Stop indexing the subfolders of ${baseName(path)}? Their files will be removed from the index (your files aren't touched).`))) {
    box.checked = true;
    return;
  }
  try {
    await invoke("set_folder_recursive", { path, on });
    await indexFolder(path);
  } catch (e) {
    box.checked = !on;
    toast(String(e), true);
  }
}

async function indexFolder(path, recursive) {
  state.indexingPath = path;
  setBusy(true);
  const card = $("#progress");
  card.hidden = false;
  $("#progress-phase").textContent = `Indexing ${baseName(path)}`;
  $("#progress-fill").classList.add("indeterminate");
  $("#progress-count").textContent = "Scanning…";
  $("#progress-chunks").textContent = "";
  $("#progress-current").innerHTML = `<bdi>${esc(path)}</bdi>`;
  $("#skip-btn").hidden = true;
  $("#progress-sub").textContent = "";
  try {
    const s = await invoke("index_folder", { path, recursive: recursive ?? null });
    const parts = [`${plural(s.embedded, "item")} embedded (${plural(s.chunks, "chunk")})`];
    if (s.pictures) parts.push(`${plural(s.pictures, "picture")} described`);
    if (s.unchanged) parts.push(`${s.unchanged.toLocaleString()} unchanged`);
    if (s.removed) parts.push(`${s.removed.toLocaleString()} removed`);
    let msg = `${s.cancelled ? "Stopped" : "Done"} in ${s.seconds.toFixed(1)}s: ${parts.join(", ")}.`;
    if (s.empty_pdfs.length) msg += `\n${s.empty_pdfs.length} PDF(s) had no text layer (scanned images) and were skipped.`;
    if (s.encoded.length) msg += `\n${s.encoded.length} file(s) looked like encoded or minified data and were skipped, e.g. ${baseName(s.encoded[0])}.`;
    if (s.skipped.length) msg += `\nSkipped by you: ${s.skipped.map(baseName).join(", ")}.`;
    if (s.encoded.length || s.skipped.length) msg += `\nSee "Skipped" in the top bar to include any of them later.`;
    if (s.failed.length) msg += `\n${s.failed.length} file(s) couldn't be read, e.g. ${baseName(s.failed[0][0])}: ${s.failed[0][1]}`;
    toast(msg, false, 9000);
  } catch (e) {
    toast(String(e), true, 10000);
  } finally {
    state.indexingPath = null;
    card.hidden = true;
    await refresh();
  }
}

listen("index-progress", ({ payload: p }) => {
  const fill = $("#progress-fill");
  if (p.phase === "embedding" && p.total > 0) {
    fill.classList.remove("indeterminate");
    fill.style.width = `${Math.round((p.done / p.total) * 100)}%`;
    $("#progress-count").textContent = `${p.done.toLocaleString()} / ${p.total.toLocaleString()} files`;
    $("#progress-chunks").textContent = plural(p.chunks, "chunk");
    // "C:\\path\\file.py  (embedding 640 of 2,113 chunks)" -> path on one line, detail below it,
    // so a long path never hides the file name or the chunk count.
    const m = p.current.match(/^(.*?)  \((.*)\)$/);
    $("#progress-current").innerHTML = `<bdi>${esc(m ? m[1] : p.current)}</bdi>`;
    $("#progress-sub").textContent = m ? m[2] : "";
    state.skippable = p.file || "";
    const skip = $("#skip-btn");
    skip.hidden = !state.skippable;
    if (skip.dataset.path !== state.skippable) {
      skip.dataset.path = state.skippable;
      skip.disabled = false;
      skip.textContent = "Skip this file";
    }
  } else if (p.phase === "unloading") {
    $("#skip-btn").hidden = true;
    fill.style.width = "100%";
    $("#progress-count").textContent = "Unloading embedding model…";
    $("#progress-current").textContent = "";
  } else if (p.phase === "embedding" && p.total === 0) {
    $("#progress-count").textContent = "Everything is up to date";
  }
});

async function removeFolder(path) {
  if (state.removing) return; // a second click while the first removal runs
  state.removing = true;
  try { await removeFolderNow(path); } finally { state.removing = false; }
}

async function removeFolderNow(path) {
  if (!(await askYesNo(`Remove ${path} from the index? (Your files are not touched.)`))) return;
  try { await invoke("remove_folder", { path }); await refresh(); }
  catch (e) { toast(String(e), true); }
}

// ---------------- asking ----------------
function newCard(question) {
  $("#welcome")?.remove();
  const id = state.nextId++;
  const el = document.createElement("article");
  el.className = "qa";
  el.innerHTML = `<div class="q"></div><div class="a"><div class="status">Searching your files…</div><div class="answer"></div><div class="stats muted" hidden></div><div class="sources" hidden><h4>Sources</h4><div class="list"></div></div></div>`;
  el.querySelector(".q").textContent = question;
  $("#transcript").appendChild(el);
  const card = { el, text: "", sources: [], pending: false, done: false };
  state.cards.set(id, card);
  scrollDown();
  return id;
}

function scrollDown() {
  const t = $("#transcript");
  t.scrollTop = t.scrollHeight;
}

function paint(id, done = false) {
  const card = state.cards.get(id);
  if (!card) return;
  if (done) card.done = true;
  done = card.done;
  const ans = card.el.querySelector(".answer");
  ans.innerHTML = renderMarkdown(card.text, id);
  ans.classList.toggle("cursor", !done);
  const t = $("#transcript");
  if (t.scrollHeight - t.scrollTop - t.clientHeight < 160) scrollDown();
}

listen("answer", ({ payload: ev }) => {
  const card = state.cards.get(ev.id);
  if (!card) return;
  const status = card.el.querySelector(".status");
  if (ev.type === "sources") {
    card.sources = ev.items;
    if (card.sourcesOnly) {
      status.textContent = `Found ${plural(ev.items.length, "match").replace("matchs", "matches")} in ${((Date.now() - card.started) / 1000).toFixed(1)}s. Answer writing is off.`;
      status.classList.add("done");
    }
    if (!card.sourcesOnly) card.base = `Found ${plural(ev.items.length, "relevant passage")}. The model is reading about ${ev.prompt_tokens.toLocaleString()} tokens before it writes…`;
    tick(card);
    const list = card.el.querySelector(".sources .list");
    list.innerHTML = "";
    ev.items.forEach((h, i) => {
      const row = document.createElement("div");
      row.className = "source";
      row.dataset.n = i + 1;
      row.innerHTML = `<span class="n">[${i + 1}]</span>
        <button class="p" title="Open ${esc(h.path)}"><div class="mono ellipsis"><bdi>${esc(h.path)}${h.is_dir ? " (folder summary)" : /\.(jpe?g|png|webp|gif|bmp)$/i.test(h.path) ? " (picture)" : ""}</bdi></div>${card.sourcesOnly ? `<div class="snippet">${esc(snippet(h))}</div>` : ""}</button>
        <span class="score muted">${(h.score * 100).toFixed(0)}%</span>
        <button class="btn small ghost reveal">Show</button>`;
      row.appendChild(tagWidget(h.path, h.is_dir, h.tags));
      row.querySelector(".p").onclick = () => invoke("open_path", { path: h.path }).catch((e) => toast(String(e), true));
      row.querySelector(".reveal").onclick = () => invoke("reveal_path", { path: h.path }).catch((e) => toast(String(e), true));
      list.appendChild(row);
    });
    card.el.querySelector(".sources").hidden = false;
  } else if (ev.type === "done") {
    // Ollama's own figures: how long it spent reading the passages and writing the answer.
    const s = ev.stats;
    const st = card.el.querySelector(".stats");
    const parts = [];
    if (s.load_secs >= 1) parts.push(`model loaded in ${s.load_secs.toFixed(0)}s`);
    if (s.prompt_tokens) parts.push(`read ${s.prompt_tokens.toLocaleString()} tokens in ${s.prompt_secs.toFixed(0)}s`);
    if (s.answer_tokens) {
      const rate = s.answer_secs > 0 ? ` (${(s.answer_tokens / s.answer_secs).toFixed(1)} tokens/s)` : "";
      parts.push(`wrote ${s.answer_tokens.toLocaleString()} tokens in ${s.answer_secs.toFixed(0)}s${rate}`);
    }
    if (parts.length) {
      st.hidden = false;
      st.textContent = parts.join(" · ").replace(/^./, (c) => c.toUpperCase());
    }
  } else if (ev.type === "thinking") {
    card.thinking = (card.thinking || 0) + 1;
    card.base = `The model is thinking before it answers: ${plural(card.thinking, "token")}…`;
    tick(card);
  } else if (ev.type === "token") {
    status.hidden = true;
    card.text += ev.text;
    card.tokens = (card.tokens || 0) + 1;
    if (!card.firstToken) card.firstToken = Date.now();
    const secs = (Date.now() - card.firstToken) / 1000;
    const rate = secs > 1 ? ` · ${(card.tokens / secs).toFixed(1)} tokens/s` : "";
    const st = card.el.querySelector(".stats");
    st.hidden = false;
    st.textContent = `Writing… ${plural(card.tokens, "token")}${rate}`;
    if (!card.pending) {
      card.pending = true;
      requestAnimationFrame(() => { card.pending = false; paint(ev.id); });
    }
  }
});

// First couple of lines of a matching passage, shown under each source in sources-only mode.
function snippet(h) {
  let t = h.text.replace(/^(Folder|Path|Picture): .*$/m, "").replace(/\s+/g, " ").trim();
  if (h.is_dir) t = h.text.replace(/\s+/g, " ").replace(/^Folder: .*? Path: \S+ ?/, "");
  return t.length > 260 ? t.slice(0, 260) + "…" : t;
}

// Status line with a running timer, so a slow model load doesn't look like a freeze.
function tick(card) {
  const status = card.el.querySelector(".status");
  if (!status || status.hidden || status.classList.contains("error") || status.classList.contains("done")) return;
  const secs = Math.floor((Date.now() - card.started) / 1000);
  const clock = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
  let text = `${card.base} ${clock}`;
  if (secs >= 180 && !card.text) {
    text += " · Still waiting. Run `ollama ps` to check the model is loaded, or pick a smaller answer model.";
  } else if (secs >= 20 && !card.text && card.base.startsWith("Found")) {
    text += " · Loading the answer model can take a minute or two the first time.";
  }
  status.textContent = text;
}

async function ask(question) {
  question = question.trim();
  if (!question || state.asking) return;
  state.asking = true;
  $("#ask-btn").disabled = true;
  const id = newCard(question);
  const card = state.cards.get(id);
  card.started = Date.now();
  card.sourcesOnly = !(state.overview?.settings?.write_answer ?? true);
  card.base = "Searching your files…";
  card.timer = setInterval(() => tick(card), 1000);
  try {
    await invoke("ask", { id, question });
    paint(id, true);
    if (!card.text.trim() && !card.sourcesOnly) card.el.querySelector(".status").textContent = "The model returned an empty answer.";
  } catch (e) {
    const status = card.el.querySelector(".status");
    status.hidden = false;
    status.className = "error";
    status.textContent = String(e);
    paint(id, true);
  } finally {
    clearInterval(card.timer);
    state.asking = false;
    $("#ask-btn").disabled = false;
    $("#question").focus();
  }
}

// Clicking a [n] citation highlights that source.
document.addEventListener("click", (e) => {
  const c = e.target.closest(".cite");
  if (!c) return;
  const card = state.cards.get(+c.dataset.card);
  const row = card?.el.querySelector(`.source[data-n="${c.dataset.n}"]`);
  if (!row) return;
  row.scrollIntoView({ block: "nearest", behavior: "smooth" });
  row.classList.add("flash");
  setTimeout(() => row.classList.remove("flash"), 1200);
});

// ---------------- wiring ----------------
const q = $("#question");
q.addEventListener("input", () => { q.style.height = "auto"; q.style.height = `${q.scrollHeight}px`; });
q.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); $("#composer").requestSubmit(); }
});
$("#composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const text = q.value;
  if (!text.trim() || state.asking) return;
  q.value = "";
  q.style.height = "auto";
  ask(text);
});
document.querySelectorAll(".example").forEach((b) => (b.onclick = () => ask(b.textContent)));
$("#add-folder").onclick = addFolder;
$("#skip-btn").onclick = async (e) => {
  const path = e.target.dataset.path;
  if (!path) return;
  e.target.disabled = true;
  e.target.textContent = "Skipping…";
  try { await invoke("skip_file", { path }); }
  catch (err) { toast(String(err), true); }
};
$("#stop-btn").onclick = () => { invoke("cancel_index"); $("#progress-count").textContent = "Stopping after the current request…"; };
$("#settings-btn").onclick = openSettings;
$("#settings-form").addEventListener("submit", saveSettings);
$("#settings-cancel").onclick = () => $("#settings").close();
$("#reset-btn").onclick = resetIndex;
$("#ollama-pill").onclick = checkOllama;
$("#write-answer").onchange = async (e) => {
  const on = e.target.checked;
  try {
    await invoke("set_write_answer", { on });
    await refresh();
    toast(on ? "Answers are on: the model writes a reply after finding sources."
             : "Sources only: you'll see matching files and excerpts in seconds, without a written answer.");
  } catch (err) { toast(String(err), true); e.target.checked = !on; }
};
// ---------------- check my computer ----------------
function benchRow(r, best) {
  const tr = document.createElement("tr");
  if (best) tr.className = "best";
  const gb = r.size ? `${(r.size / 1e9).toFixed(1)} GB` : "";
  tr.innerHTML = r.error
    ? `<td>${esc(r.model)}</td><td class="num">${gb}</td><td colspan="3" class="muted">Test failed: ${esc(r.error)}</td>`
    : `<td>${esc(r.model)}</td><td class="num">${gb}</td>
       <td class="num">${r.read_tps.toFixed(1)} tok/s</td><td class="num">${r.write_tps.toFixed(1)} tok/s</td>
       <td class="num"><b>${fmtDur(r.answer_secs) || "—"}</b></td>`;
  return tr;
}

function showBenchReport(rep) {
  const body = $("#bench-rows");
  body.innerHTML = "";
  for (const r of rep.rows) body.appendChild(benchRow(r, r.model === rep.recommended));
  $("#bench-when").textContent = rep.ran_at ? `Tested ${ago(rep.ran_at)}${rep.cpu_only ? " (CPU only)" : ""}` : "";
  const adv = $("#bench-advice");
  const parts = [];
  const acts = [];
  const current = state.overview?.settings.chat_model;
  const best = rep.rows.find((r) => r.model === rep.recommended);
  if (best) {
    parts.push(`<b>Recommended: ${esc(best.model)}</b>. A typical answer should take about ${fmtDur(best.answer_secs)} on this computer.`);
    if (!sameModel(best.model, current || "")) acts.push(`<button class="btn primary" data-use="${esc(best.model)}">Use ${esc(best.model)}</button>`);
    if (best.answer_secs > 120) {
      parts.push("Every installed model takes over 2 minutes. A smaller model would be much quicker: in PowerShell run <code>ollama pull qwen3:4b</code> or <code>ollama pull llama3.2:3b</code>, then test again.");
    }
  }
  const ct = rep.cpu_test;
  if (ct && isFinite(ct.cpu_secs) && isFinite(ct.current_secs)) {
    if (ct.cpu_secs < ct.current_secs * 0.8) {
      parts.push(`CPU only is faster here: ${fmtDur(ct.cpu_secs)} instead of ${fmtDur(ct.current_secs)} per answer with ${esc(ct.model)}.`);
      acts.push(`<button class="btn" data-cpu="1">Run on CPU only</button>`);
    } else {
      parts.push(`CPU only made no real difference (${fmtDur(ct.cpu_secs)} vs ${fmtDur(ct.current_secs)}), so the current setup is fine.`);
    }
  }
  adv.hidden = parts.length === 0;
  adv.innerHTML = `${parts.map((p) => `<div>${p}</div>`).join("")}${acts.length ? `<div class="acts">${acts.join("")}</div>` : ""}`;
  adv.querySelectorAll("[data-use]").forEach((b) => (b.onclick = async () => {
    try {
      await invoke("switch_chat_model", { model: b.dataset.use });
      await refresh();
      showBenchReport(rep);
      toast(`Answers now come from ${b.dataset.use}.`);
    } catch (e) { toast(String(e), true); }
  }));
  adv.querySelectorAll("[data-cpu]").forEach((b) => (b.onclick = async () => {
    try {
      await invoke("set_cpu_only", { on: true });
      await refresh();
      b.remove();
      toast("DocSpace now asks Ollama to run models on the CPU only. You can change this in Settings.");
    } catch (e) { toast(String(e), true); }
  }));
}

async function runBench() {
  const btn = $("#bench-run");
  btn.disabled = true;
  btn.textContent = "Testing…";
  $("#bench-rows").innerHTML = "";
  $("#bench-advice").hidden = true;
  $("#bench-fill").classList.add("indeterminate");
  $("#bench-label").textContent = "Starting…";
  try {
    const rep = await invoke("benchmark_models");
    await refresh();
    showBenchReport(rep);
    $("#bench-label").textContent = "Done.";
  } catch (e) {
    $("#bench-label").textContent = String(e);
  } finally {
    $("#bench-fill").classList.remove("indeterminate");
    $("#bench-fill").style.width = "100%";
    btn.disabled = false;
    btn.textContent = "Test again";
  }
}

listen("bench-progress", ({ payload: p }) => {
  if (p.label) $("#bench-label").textContent = p.label;
  if (p.steps) {
    $("#bench-fill").classList.remove("indeterminate");
    $("#bench-fill").style.width = `${Math.round((p.step / p.steps) * 100)}%`;
  }
  if (p.row) $("#bench-rows").appendChild(benchRow(p.row, false));
});

$("#bench-btn").onclick = () => {
  $("#settings").close();
  $("#benchdlg").showModal();
  const last = state.overview?.benchmark;
  if (last) {
    showBenchReport(last);
    $("#bench-run").textContent = "Test again";
    $("#bench-label").textContent = "";
    $("#bench-fill").style.width = "0";
  } else {
    $("#bench-rows").innerHTML = "";
    $("#bench-advice").hidden = true;
    $("#bench-when").textContent = "";
    $("#bench-run").textContent = "Start test";
  }
};
$("#bench-run").onclick = runBench;
$("#bench-close").onclick = () => $("#benchdlg").close();

$("#log-btn").onclick = () => { $("#logdlg").showModal(); showLog(); };
$("#log-close").onclick = () => $("#logdlg").close();
$("#log-refresh").onclick = showLog;
$("#log-open").onclick = async () => {
  const path = await invoke("log_file_path");
  if (path) invoke("open_path", { path }).catch((e) => toast(String(e), true));
};
$("#log-clear").onclick = async () => {
  if (!(await askYesNo("Clear the log?"))) return;
  await invoke("clear_log");
  showLog();
};
$("#log-errors").onchange = showLog;
$("#log-copy").onclick = async () => {
  const text = $("#log-text").value;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = $("#log-text");
    ta.select();
    document.execCommand("copy");
  }
  toast("Copied. Paste it into the chat with Claude.");
};
$("#skipped-btn").onclick = () => { $("#skipped").showModal(); showSkipped(); };
$("#skipped-close").onclick = () => $("#skipped").close();
$("#skip-reason").onchange = renderSkipped;
$("#skip-search").oninput = renderSkipped;
$("#include-all").onclick = async () => {
  // Pictures can't be included without a picture model, so they're left out of "Include all".
  const paths = shownSkipped().filter((it) => it.reason !== "needs_vision").map((it) => it.path);
  if (!paths.length) { toast("Pictures need a picture model: choose one in Settings, then Re-index."); return; }
  if (paths.length > 1 && !(await askYesNo(`Include all ${paths.length.toLocaleString()} files shown? They'll be indexed the next time you Re-index their folders.`))) return;
  includeFiles(paths);
};
$("#tags-btn").onclick = () => { $("#tagsdlg").showModal(); showTags(""); };
$("#tags-close").onclick = () => $("#tagsdlg").close();
$("#model-select").onchange = async (e) => {
  const model = e.target.value;
  if (modelKind(model) === "vision" &&
      !(await askYesNo(`${model} is a picture model. It can answer questions, but more slowly and less well than a text model like qwen3:4b. Use it for answers anyway?\n\n(To classify pictures, choose it as the Picture model in Settings instead.)`))) {
    fillModelPicker(state.models || [], state.overview?.settings.chat_model);
    return;
  }
  e.target.disabled = true;
  try {
    await invoke("switch_chat_model", { model });
    await refresh();
    toast(`Answers now come from ${model}. The previous model was unloaded to free memory.`);
  } catch (err) { toast(String(err), true); }
  finally { e.target.disabled = false; }
};
invoke("app_version").then((v) => ($("#app-version").textContent = `v${v}`)).catch(() => {});

refresh().then(checkOllama).then(loadTagNames).catch((e) => toast(String(e), true, 15000));
q.focus();
