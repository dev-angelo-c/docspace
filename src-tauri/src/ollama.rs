//! Minimal blocking client for the Ollama HTTP API:
//! /api/embed, /api/chat (streaming), /api/tags, /api/show, /api/ps, unload via keep_alive=0.

use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

/// Context window for the embedding model. Chunks are ~1.2k characters, far below this,
/// and a small window keeps the embedder's memory footprint small.
const EMBED_NUM_CTX: u32 = 2048;
/// An embedding request that gets no answer in this long means Ollama is stuck (or was
/// closed mid-request). Indexing stops with a clear message instead of waiting forever.
/// Generous on purpose: 64 chunks on a slow CPU can take a couple of minutes.
const EMBED_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub struct Ollama {
    base: String,
    http: Client,
    /// Ask Ollama to keep every model on the CPU (num_gpu = 0). On weak integrated
    /// graphics this is often faster, and it avoids restarting Ollama without the GPU setting.
    cpu_only: bool,
}

/// An installed model and what it can do, from /api/tags and /api/show.
#[derive(Serialize, Clone, Debug)]
pub struct ModelInfo {
    pub name: String,
    /// Download size in bytes.
    pub size: u64,
    /// e.g. ["completion"], ["completion", "vision"], ["embedding"]
    pub capabilities: Vec<String>,
}

impl ModelInfo {
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// One speed test of a model on this computer.
#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct Bench {
    /// Reading speed: how fast the model takes in a prompt (tokens per second).
    pub read_tps: f64,
    /// Writing speed (tokens per second).
    pub write_tps: f64,
    /// Seconds to load the model into memory.
    pub load_secs: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Message { role: "user".into(), content: content.into() }
    }
}

/// Ollama's own figures for one answer (durations in seconds).
#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct ChatStats {
    pub prompt_tokens: Option<u32>,
    pub prompt_secs: f64,
    pub answer_tokens: Option<u32>,
    pub answer_secs: f64,
    pub load_secs: f64,
}

pub enum Token<'a> {
    /// The model is reasoning before it answers (the reasoning text itself isn't shown).
    Thinking,
    Answer(&'a str),
}

/// Task prefixes some embedding models were trained with; using them noticeably improves retrieval.
pub fn embed_prefixes(model: &str) -> (&'static str, &'static str) {
    let m = model.to_ascii_lowercase();
    if m.contains("nomic-embed") {
        ("search_document: ", "search_query: ")
    } else if m.contains("mxbai-embed") || m.contains("snowflake-arctic-embed") {
        ("", "Represent this sentence for searching relevant passages: ")
    } else {
        ("", "")
    }
}

impl Ollama {
    pub fn new(host: &str) -> Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            // No overall timeout: loading an 8B model or a long answer on CPU can take minutes.
            .timeout(None::<Duration>)
            .no_proxy() // Ollama is local
            .build()?;
        Ok(Ollama { base: normalize_host(host), http, cpu_only: false })
    }

    pub fn with_cpu_only(mut self, on: bool) -> Self {
        self.cpu_only = on;
        self
    }

    /// Add num_gpu = 0 to a request's options when running CPU-only.
    fn apply_cpu(&self, body: &mut Value) {
        if self.cpu_only {
            body["options"]["num_gpu"] = json!(0);
        }
    }

    /// Every installed model with its size and capabilities.
    pub fn models_info(&self) -> Result<Vec<ModelInfo>> {
        let v: Value = self.send(self.http.get(self.url("/api/tags")))?.json()?;
        let mut out = Vec::new();
        for m in v["models"].as_array().cloned().unwrap_or_default() {
            let Some(name) = m["name"].as_str().map(str::to_owned) else { continue };
            let size = m["size"].as_u64().unwrap_or(0);
            let capabilities = self
                .post("/api/show", &json!({ "model": name }))
                .ok()
                .and_then(|r| r.json::<Value>().ok())
                .and_then(|show| show["capabilities"].as_array().cloned())
                .map(|caps| caps.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                .unwrap_or_else(|| guess_capabilities(&name));
            out.push(ModelInfo { name, size, capabilities });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Time one model on this computer: read a ~600-token prompt, write 48 tokens.
    pub fn benchmark(&self, model: &str, cpu_only: bool) -> Result<Bench> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        // The nonce makes every run's prompt different, so Ollama can't reuse a cached prompt.
        let paragraph = "DocSpace indexes local folders by splitting documents into chunks, turning each \
            chunk into a vector with an embedding model, and storing the vectors in SQLite. When you ask \
            a question, the closest chunks are found and passed to a language model, which reads them and \
            writes an answer that cites the files it used. ";
        let prompt = format!(
            "Test {nonce}.\n{}\nIn one sentence, what does DocSpace do?",
            paragraph.repeat(6)
        );
        let mut body = json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "options": { "num_predict": 48, "num_ctx": 2048, "temperature": 0.0 },
        });
        let m = model.to_ascii_lowercase();
        if m.starts_with("qwen3") || m.contains("deepseek-r1") {
            body["think"] = json!(false);
        }
        if cpu_only {
            body["options"]["num_gpu"] = json!(0);
        }
        let v: Value = self.post_with("/api/generate", &body, Some(EMBED_TIMEOUT))?.json()?;
        let rate = |count: &str, dur: &str| {
            let n = v[count].as_f64().unwrap_or(0.0);
            let secs = v[dur].as_f64().unwrap_or(0.0) / 1e9;
            if secs > 0.0 { n / secs } else { 0.0 }
        };
        Ok(Bench {
            read_tps: rate("prompt_eval_count", "prompt_eval_duration"),
            write_tps: rate("eval_count", "eval_duration"),
            load_secs: v["load_duration"].as_f64().unwrap_or(0.0) / 1e9,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        req.send().with_context(|| {
            format!("Can't reach Ollama at {}. Is it running? (Start the Ollama app, or run `ollama serve`.)", self.base)
        })
    }

    fn post(&self, path: &str, body: &Value) -> Result<Response> {
        self.post_with(path, body, None)
    }

    fn post_with(&self, path: &str, body: &Value, timeout: Option<Duration>) -> Result<Response> {
        let mut req = self.http.post(self.url(path)).json(body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = match req.send() {
            Ok(r) => r,
            Err(e) if e.is_timeout() => bail!(
                "Ollama didn't answer for {} minutes, so indexing stopped. Restart Ollama, then click Re-index: \
                 finished files are kept.",
                timeout.map_or(0, |t| t.as_secs() / 60)
            ),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Can't reach Ollama at {}. Is it running? (Start the Ollama app, or run `ollama serve`.)", self.base)
                })
            }
        };
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or(text);
        bail!("Ollama {path} returned {status}: {msg}")
    }

    pub fn version(&self) -> Result<String> {
        let v: Value = self.send(self.http.get(self.url("/api/version")))?.json()?;
        Ok(v["version"].as_str().unwrap_or("unknown").to_string())
    }

    /// Fail early, with the fix, if a model hasn't been pulled.
    pub fn ensure_model(&self, model: &str) -> Result<()> {
        let resp = self.send(self.http.post(self.url("/api/show")).json(&json!({ "model": model })))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => bail!("Ollama doesn't have `{model}` yet. In PowerShell run:  ollama pull {model}"),
            s => bail!("Ollama /api/show for `{model}` returned {s}: {}", resp.text().unwrap_or_default()),
        }
    }

    /// Embed a batch of texts in one request. `keep_alive: Some(0)` unloads the model right after.
    pub fn embed(&self, model: &str, inputs: &[String], keep_alive: Option<i64>) -> Result<Vec<Vec<f32>>> {
        #[derive(Deserialize)]
        struct EmbedResponse {
            embeddings: Vec<Vec<f32>>,
        }
        let mut body = json!({
            "model": model,
            "input": inputs,
            "truncate": true,
            "options": { "num_ctx": EMBED_NUM_CTX },
        });
        if let Some(ka) = keep_alive {
            body["keep_alive"] = json!(ka);
        }
        self.apply_cpu(&mut body);
        let resp: EmbedResponse =
            self.post_with("/api/embed", &body, Some(EMBED_TIMEOUT))?.json().context("decoding /api/embed response")?;
        if resp.embeddings.len() != inputs.len() {
            bail!("Ollama returned {} embeddings for {} inputs", resp.embeddings.len(), inputs.len());
        }
        Ok(resp.embeddings)
    }

    /// Streaming chat. Returns Ollama's timing figures from the final message.
    pub fn chat(
        &self,
        model: &str,
        messages: &[Message],
        num_ctx: u32,
        num_predict: u32,
        mut on_token: impl FnMut(Token<'_>),
    ) -> Result<ChatStats> {
        #[derive(Deserialize)]
        struct StreamMsg {
            #[serde(default)]
            content: String,
            #[serde(default)]
            thinking: Option<String>,
        }
        #[derive(Deserialize)]
        struct StreamChunk {
            #[serde(default)]
            message: Option<StreamMsg>,
            #[serde(default)]
            done: bool,
            #[serde(default)]
            error: Option<String>,
            #[serde(default)]
            prompt_eval_count: Option<u32>,
            #[serde(default)]
            prompt_eval_duration: Option<u64>,
            #[serde(default)]
            eval_count: Option<u32>,
            #[serde(default)]
            eval_duration: Option<u64>,
            #[serde(default)]
            load_duration: Option<u64>,
        }

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "options": {
                // Ollama's default context is small and it silently drops the start of
                // prompts that overflow it, so always set it explicitly.
                "num_ctx": num_ctx,
                "num_predict": num_predict,
                "temperature": 0.2,
            },
        });
        // Hybrid reasoning models: answer directly. Much faster, and thinking tokens
        // would otherwise eat the context budget. Only sent where it's understood.
        let m = model.to_ascii_lowercase();
        if m.starts_with("qwen3") || m.contains("deepseek-r1") {
            body["think"] = json!(false);
        }
        self.apply_cpu(&mut body);

        let resp = self.post("/api/chat", &body)?;
        let mut stats = ChatStats::default();
        for line in BufReader::new(resp).lines() {
            let line = line.context("reading the response stream from Ollama")?;
            if line.trim().is_empty() {
                continue;
            }
            let chunk: StreamChunk =
                serde_json::from_str(&line).with_context(|| format!("unexpected line from Ollama: {line}"))?;
            if let Some(err) = chunk.error {
                bail!("Ollama: {err}");
            }
            if let Some(msg) = chunk.message {
                if msg.thinking.as_deref().is_some_and(|t| !t.is_empty()) {
                    on_token(Token::Thinking);
                }
                if !msg.content.is_empty() {
                    on_token(Token::Answer(&msg.content));
                }
            }
            if chunk.done {
                let secs = |ns: Option<u64>| ns.map_or(0.0, |n| n as f64 / 1e9);
                stats = ChatStats {
                    prompt_tokens: chunk.prompt_eval_count,
                    prompt_secs: secs(chunk.prompt_eval_duration),
                    answer_tokens: chunk.eval_count,
                    answer_secs: secs(chunk.eval_duration),
                    load_secs: secs(chunk.load_duration),
                };
                break;
            }
        }
        Ok(stats)
    }

    /// Ask a vision model to describe a picture in a few searchable sentences.
    pub fn describe_image(&self, model: &str, image: &[u8]) -> Result<String> {
        use base64::Engine;
        let mut body = json!({
            "model": model,
            "prompt": "Describe this picture for a search index in 2-3 plain sentences: the main subject, \
                       the setting, notable objects, people or animals, and any readable text. \
                       Do not guess names. No lists, no preamble.",
            "images": [base64::engine::general_purpose::STANDARD.encode(image)],
            "stream": false,
            "options": { "num_predict": 160, "temperature": 0.1 },
        });
        self.apply_cpu(&mut body);
        let v: Value = self
            .post_with("/api/generate", &body, Some(EMBED_TIMEOUT))?
            .json()
            .context("decoding the picture description from Ollama")?;
        let text = v["response"].as_str().unwrap_or_default().trim().to_string();
        if text.is_empty() {
            bail!("the picture model returned no description");
        }
        Ok(text)
    }

    pub fn loaded_models(&self) -> Result<Vec<String>> {
        let v: Value = self.send(self.http.get(self.url("/api/ps")))?.json()?;
        Ok(v["models"]
            .as_array()
            .map(|ms| ms.iter().filter_map(|m| m["name"].as_str().map(str::to_owned)).collect())
            .unwrap_or_default())
    }

    /// Evict a model from RAM/VRAM now (same as `ollama stop <model>`), then wait briefly until it's gone.
    pub fn unload(&self, model: &str) -> Result<bool> {
        // Generative models: empty generate request with keep_alive 0.
        // Embedding-only models reject /api/generate, so fall back to an empty embed request.
        if self.post("/api/generate", &json!({ "model": model, "keep_alive": 0 })).is_err() {
            self.post("/api/embed", &json!({ "model": model, "input": [], "keep_alive": 0 }))
                .with_context(|| format!("could not unload `{model}`"))?;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if !self.loaded_models()?.iter().any(|m| same_model(m, model)) {
                return Ok(true);
            }
            if Instant::now() > deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// For old Ollama versions whose /api/show has no "capabilities": judge by the name.
fn guess_capabilities(name: &str) -> Vec<String> {
    let n = name.to_ascii_lowercase();
    let cap = if n.contains("embed") || n.contains("minilm") || n.contains("bge") {
        "embedding"
    } else if ["vl", "vision", "llava", "moondream", "minicpm-v", "bakllava"].iter().any(|k| n.contains(k)) {
        "vision"
    } else {
        "completion"
    };
    let mut caps = vec![cap.to_string()];
    if cap == "vision" {
        caps.insert(0, "completion".into());
    }
    caps
}

/// `nomic-embed-text` and `nomic-embed-text:latest` are the same model.
pub fn same_model(a: &str, b: &str) -> bool {
    let full = |s: &str| if s.contains(':') { s.to_string() } else { format!("{s}:latest") };
    full(a) == full(b)
}

/// Accepts the same forms as OLLAMA_HOST: `host:port`, `http://host:port`, `0.0.0.0`.
fn normalize_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    let host = if host.is_empty() { "127.0.0.1:11434" } else { host };
    let mut url = if host.contains("://") { host.to_string() } else { format!("http://{host}") };
    url = url.replace("://0.0.0.0", "://127.0.0.1");
    let after_scheme = &url[url.find("://").map_or(0, |i| i + 3)..];
    if !after_scheme.contains(':') {
        url.push_str(":11434");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_forms() {
        assert_eq!(normalize_host("127.0.0.1:11434"), "http://127.0.0.1:11434");
        assert_eq!(normalize_host("0.0.0.0"), "http://127.0.0.1:11434");
        assert_eq!(normalize_host("http://box.lan:8080/"), "http://box.lan:8080");
        assert_eq!(normalize_host(""), "http://127.0.0.1:11434");
    }

    #[test]
    fn model_tags() {
        assert!(same_model("nomic-embed-text", "nomic-embed-text:latest"));
        assert!(!same_model("qwen3:8b", "qwen3:4b"));
    }
}
