//! Phase 24b — OpenAI-compatible remote LLM client.
//!
//! Speaks non-streaming `POST /v1/chat/completions` (+ `GET /v1/models` for the
//! settings picker) against a user-provided server: LM Studio, `ollama serve`,
//! llama-server, vLLM, Jan, KoboldCpp, … — they all expose this API. Requests
//! go through zord-net's OS-cert-store + proxy-aware agent. Errors are mapped
//! to actionable messages ("is the server running?", "check the API key") and
//! never fall back to the local model (decided: explicit failure over silent
//! model switching).

use anyhow::{anyhow, Result};
use std::time::Duration;

use crate::opts::{truncate_chars, ChatRole, GenOpts};

/// Connection details for an OpenAI-compatible server (user-provided, from
/// Settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    /// Server root, e.g. `http://localhost:1234` (LM Studio's default). A
    /// trailing `/` or `/v1` is tolerated.
    pub base_url: String,
    /// Bearer token; empty = no `Authorization` header (typical on LAN).
    pub api_key: String,
    /// Model id as the server reports it (see [`list_models`]).
    pub model: String,
    /// Per-request timeout in seconds. Generations on big models take a while.
    pub timeout_secs: u64,
}

impl RemoteConfig {
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.clamp(10, 3600))
    }

    fn bearer(&self) -> Option<&str> {
        let k = self.api_key.trim();
        (!k.is_empty()).then_some(k)
    }

    /// `…/v1/<path>` from the configured base URL.
    fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        format!("{base}/v1/{path}")
    }
}

/// List the model ids the server offers (`GET /v1/models`). Used by the
/// settings picker and as a cheap "test connection".
pub fn list_models(cfg: &RemoteConfig) -> Result<Vec<String>> {
    let url = cfg.endpoint("models");
    let resp = zord_net::get_json(&url, cfg.bearer(), Duration::from_secs(15))
        .map_err(|e| friendly(e, cfg))?;
    let ids: Vec<String> = resp["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        anyhow::bail!(
            "the server at {} answered but listed no models — load one in the server first",
            cfg.base_url.trim()
        );
    }
    Ok(ids)
}

/// A configured remote endpoint, exposing the same generation surface as the
/// local [`crate::Summarizer`] (via [`crate::LlmBackend`]).
pub struct RemoteLlm {
    cfg: RemoteConfig,
}

impl RemoteLlm {
    pub fn new(cfg: RemoteConfig) -> Self {
        Self { cfg }
    }

    /// The config this client was built with (used for cache invalidation).
    pub fn config(&self) -> &RemoteConfig {
        &self.cfg
    }

    /// One chat completion over `user_content` with `system_prompt`. Mirrors the
    /// local backend: the same coarse input truncation, and `temperature: 0` to
    /// match its greedy decode.
    pub fn generate(&self, user_content: &str, system_prompt: &str, opts: GenOpts) -> Result<String> {
        let user = truncate_chars(user_content, opts.max_transcript_chars);
        self.complete(
            vec![message("system", system_prompt), message("user", &user)],
            opts,
        )
    }

    /// Multi-turn grounded chat (Phase 23d shape).
    pub fn chat(&self, system_prompt: &str, turns: &[(ChatRole, String)], n_ctx: u32) -> Result<String> {
        self.complete(self.chat_messages(system_prompt, turns), GenOpts::chat(n_ctx))
    }

    /// Like [`chat`], but streams (`stream: true` + SSE), calling `on_delta`
    /// with each content piece as the server emits it (Phase 24d). Returns the
    /// accumulated reply at the end.
    pub fn chat_stream(
        &self,
        system_prompt: &str,
        turns: &[(ChatRole, String)],
        n_ctx: u32,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let opts = GenOpts::chat(n_ctx);
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": self.chat_messages(system_prompt, turns),
            "max_tokens": opts.max_new_tokens,
            "temperature": 0,
            "stream": true,
        });
        let url = self.cfg.endpoint("chat/completions");
        tracing::info!(%url, model = %self.cfg.model, "remote LLM request (streaming)");
        let mut full = String::new();
        zord_net::post_sse(&url, self.cfg.bearer(), &body, self.cfg.timeout(), &mut |data| {
            // Each SSE payload is a chunk object; content lives in
            // choices[0].delta.content (absent on role/finish chunks).
            if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(piece) = chunk["choices"][0]["delta"]["content"].as_str() {
                    if !piece.is_empty() {
                        full.push_str(piece);
                        on_delta(piece);
                    }
                }
            }
        })
        .map_err(|e| friendly(e, &self.cfg))?;
        let full = full.trim().to_string();
        if full.is_empty() {
            anyhow::bail!("the server streamed no completion text");
        }
        Ok(full)
    }

    fn chat_messages(&self, system_prompt: &str, turns: &[(ChatRole, String)]) -> Vec<serde_json::Value> {
        let mut messages = vec![message("system", system_prompt)];
        messages.extend(turns.iter().map(|(role, content)| message(role.as_str(), content)));
        messages
    }

    fn complete(&self, messages: Vec<serde_json::Value>, opts: GenOpts) -> Result<String> {
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": messages,
            "max_tokens": opts.max_new_tokens,
            "temperature": 0,
            "stream": false,
        });
        let url = self.cfg.endpoint("chat/completions");
        tracing::info!(%url, model = %self.cfg.model, "remote LLM request");
        let resp = zord_net::post_json(&url, self.cfg.bearer(), &body, self.cfg.timeout())
            .map_err(|e| friendly(e, &self.cfg))?;
        let text = resp["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        text.ok_or_else(|| {
            anyhow!(
                "the server returned no completion text (response: {})",
                excerpt(&resp.to_string())
            )
        })
    }
}

/// Rough token estimate (~4 chars/token) for input budgeting. The server owns
/// its real context window; this only sizes what we send (Overview packing,
/// chat-context fit), where the local path uses an exact tokenizer count.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

fn message(role: &str, content: &str) -> serde_json::Value {
    serde_json::json!({ "role": role, "content": content })
}

/// Map an HTTP-layer error to something the user can act on.
fn friendly(e: zord_net::ApiError, cfg: &RemoteConfig) -> anyhow::Error {
    let server = cfg.base_url.trim();
    match e {
        zord_net::ApiError::Connect(msg) => anyhow!(
            "couldn't reach {server} — is the inference server running? ({msg})"
        ),
        zord_net::ApiError::Status { code: 401 | 403, .. } => {
            anyhow!("{server} rejected the request — check the API key in Settings")
        }
        zord_net::ApiError::Status { code: 404, body } => anyhow!(
            "{server} has no OpenAI-compatible endpoint here (404){} — check the \
             server URL and that the model id '{}' exists",
            fmt_body(&body),
            cfg.model
        ),
        zord_net::ApiError::Status { code, body } => {
            anyhow!("{server} answered HTTP {code}{}", fmt_body(&body))
        }
        zord_net::ApiError::BadJson(msg) => anyhow!(
            "{server} answered with something that isn't JSON — is this an \
             OpenAI-compatible server? ({msg})"
        ),
    }
}

fn fmt_body(body: &str) -> String {
    let b = excerpt(body);
    if b.is_empty() {
        String::new()
    } else {
        format!(": {b}")
    }
}

fn excerpt(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 200 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(200).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base_url: &str) -> RemoteConfig {
        RemoteConfig {
            base_url: base_url.to_string(),
            api_key: String::new(),
            model: "m".to_string(),
            timeout_secs: 60,
        }
    }

    /// Users paste server roots in every shape — all must hit `…/v1/<path>`.
    #[test]
    fn endpoint_tolerates_base_url_shapes() {
        for base in [
            "http://localhost:1234",
            "http://localhost:1234/",
            "http://localhost:1234/v1",
            "http://localhost:1234/v1/",
            "  http://localhost:1234/v1 ",
        ] {
            assert_eq!(
                cfg(base).endpoint("chat/completions"),
                "http://localhost:1234/v1/chat/completions",
                "base url: {base:?}"
            );
        }
    }

    #[test]
    fn empty_api_key_sends_no_bearer() {
        assert_eq!(cfg("http://x").bearer(), None);
        let mut c = cfg("http://x");
        c.api_key = "  ".to_string();
        assert_eq!(c.bearer(), None);
        c.api_key = "sk-123".to_string();
        assert_eq!(c.bearer(), Some("sk-123"));
    }

    #[test]
    fn token_estimate_is_quarter_of_chars() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    /// End-to-end against an in-process mock server: request goes to
    /// `/v1/chat/completions` with the right shape, the canned reply parses,
    /// and no Authorization header is sent for an empty key.
    #[test]
    fn completes_against_a_mock_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read until headers + Content-Length body are fully in.
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let need: usize = text
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + need {
                        break;
                    }
                }
            }
            let body = r#"{"choices":[{"message":{"role":"assistant","content":" hello there "}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&req).to_string()
        });

        let mut c = cfg(&format!("http://127.0.0.1:{port}/v1"));
        c.model = "test-model".to_string();
        let out = RemoteLlm::new(c).generate("hi", "sys", GenOpts::summary()).unwrap();
        assert_eq!(out, "hello there");

        let req = server.join().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions"), "request line: {}", req.lines().next().unwrap_or(""));
        assert!(req.contains("\"model\":\"test-model\""));
        assert!(req.contains("\"temperature\":0"));
        assert!(!req.to_lowercase().contains("authorization:"), "empty key must send no auth header");
    }

    /// Streaming end-to-end: SSE chunks arrive as deltas, role/finish chunks
    /// and the [DONE] sentinel are skipped, and the accumulated text returns.
    #[test]
    fn streams_against_a_mock_sse_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Drain the request headers + body (single small read is enough to
            // unblock; we respond regardless).
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let chunks = [
                r#"{"choices":[{"delta":{"role":"assistant"}}]}"#, // role chunk: no content
                r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
                r#"{"choices":[{"delta":{"content":"lo!"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ];
            let mut body = String::new();
            for c in chunks {
                body.push_str(&format!("data: {c}\n\n"));
            }
            body.push_str("data: [DONE]\n\n");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            req
        });

        let mut c = cfg(&format!("http://127.0.0.1:{port}"));
        c.model = "test-model".to_string();
        let mut deltas = Vec::new();
        let out = RemoteLlm::new(c)
            .chat_stream("sys", &[(ChatRole::User, "hi".to_string())], 8192, &mut |d| {
                deltas.push(d.to_string());
            })
            .unwrap();
        assert_eq!(out, "Hello!");
        assert_eq!(deltas, vec!["Hel", "lo!"]);
        let req = server.join().unwrap();
        assert!(req.contains("\"stream\":true"), "request must ask for streaming");
    }
}
