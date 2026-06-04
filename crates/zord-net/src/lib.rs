//! Shared HTTP download helper that works on managed / corporate networks.
//!
//! Model downloads previously used `ureq`'s default agent (rustls + bundled
//! Mozilla roots, no proxy), which fails behind corporate HTTPS-inspection
//! (untrusted MITM root CA) or an enterprise proxy — even though the user's
//! browser works. This helper:
//! - uses the **OS certificate store** via native-tls (Windows schannel / macOS
//!   Secure Transport), so an IT-installed inspection CA is trusted, and
//! - honors an explicit **proxy** from the usual environment variables, and
//! - streams to disk with progress + a few retries.
//!
//! Note: this covers TLS-inspection and env-var proxies. A PAC/WPAD or
//! Windows-registry (WinINET) system proxy with no env var set is not auto-
//! detected — the manual browser-download fallback still covers that case.

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Build an agent that trusts the OS cert store and uses an env proxy if set.
fn agent() -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .user_agent(concat!("zord/", env!("CARGO_PKG_VERSION")));

    match native_tls::TlsConnector::new() {
        Ok(connector) => builder = builder.tls_connector(Arc::new(connector)),
        Err(e) => tracing::warn!("native-tls connector unavailable: {e}"),
    }

    if let Some(p) = proxy_from_env() {
        match ureq::Proxy::new(&p) {
            Ok(proxy) => {
                tracing::info!("using proxy from environment");
                builder = builder.proxy(proxy);
            }
            Err(e) => tracing::warn!("ignoring invalid proxy '{p}': {e}"),
        }
    }
    builder.build()
}

/// First non-empty proxy URL from the standard environment variables.
fn proxy_from_env() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// Why a JSON API call failed — split so callers can give targeted hints
/// (e.g. "is the server running?" vs "check the API key").
#[derive(Debug)]
pub enum ApiError {
    /// The server couldn't be reached at all (refused / DNS / TLS / timeout).
    Connect(String),
    /// The server responded with a non-2xx status.
    Status { code: u16, body: String },
    /// The 2xx response body wasn't valid JSON.
    BadJson(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Connect(e) => write!(f, "connection failed: {e}"),
            ApiError::Status { code, body } => write!(f, "HTTP {code}: {body}"),
            ApiError::BadJson(e) => write!(f, "invalid JSON response: {e}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// POST a JSON body to `url` and parse the JSON response. Uses the same
/// OS-cert-store + proxy agent as downloads. `bearer` (when non-empty) is sent
/// as an `Authorization: Bearer` header. `timeout` bounds the whole request —
/// LLM generations can take minutes, so callers pick it.
pub fn post_json(
    url: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, ApiError> {
    let mut req = agent()
        .post(url)
        .set("Content-Type", "application/json")
        .timeout(timeout);
    if let Some(key) = bearer.filter(|k| !k.trim().is_empty()) {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    json_response(req.send_string(&body.to_string()))
}

/// GET `url` and parse the JSON response (see [`post_json`] for agent/auth).
pub fn get_json(
    url: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<serde_json::Value, ApiError> {
    let mut req = agent().get(url).timeout(timeout);
    if let Some(key) = bearer.filter(|k| !k.trim().is_empty()) {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    json_response(req.call())
}

fn json_response(
    result: Result<ureq::Response, ureq::Error>,
) -> Result<serde_json::Value, ApiError> {
    match result {
        Ok(resp) => {
            let text = resp
                .into_string()
                .map_err(|e| ApiError::Connect(format!("reading response: {e}")))?;
            serde_json::from_str(&text).map_err(|e| ApiError::BadJson(e.to_string()))
        }
        Err(ureq::Error::Status(code, resp)) => Err(ApiError::Status {
            code,
            body: resp.into_string().unwrap_or_default(),
        }),
        Err(ureq::Error::Transport(t)) => Err(ApiError::Connect(t.to_string())),
    }
}

/// Stream `url` to `dest` (atomic via a `.partial` temp), reporting
/// `(downloaded, total)` progress. Retries transient failures a few times.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let agent = agent();
    let mut last_err = None;
    for attempt in 1..=3 {
        match try_download(&agent, url, dest, progress) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(%url, attempt, "download failed: {e}");
                last_err = Some(e);
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("download failed")))
}

/// Download a GGUF model from the **Ollama registry** (used purely as a model
/// CDN — no Ollama install/daemon/engine). Resolves the model manifest, picks
/// the `application/vnd.ollama.image.model` layer, and downloads that blob (a
/// standard GGUF) to `dest`. Reaches the registry via the same OS-cert-store +
/// proxy agent as everything else.
pub fn download_ollama_model(
    repo: &str,
    tag: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let blob_url = ollama_blob_url(&agent(), repo, tag)?;
    tracing::info!(%blob_url, "downloading GGUF from Ollama registry");
    download_to_file(&blob_url, dest, progress)
}

/// Resolve an Ollama `repo:tag` to the direct blob URL of its GGUF model layer.
fn ollama_blob_url(agent: &ureq::Agent, repo: &str, tag: &str) -> Result<String> {
    let base = format!("https://registry.ollama.ai/v2/library/{repo}");
    let manifest_url = format!("{base}/manifests/{tag}");
    let body = agent
        .get(&manifest_url)
        .set("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .call()
        .with_context(|| format!("fetching Ollama manifest {manifest_url}"))?
        .into_string()
        .context("reading Ollama manifest")?;
    let manifest: serde_json::Value =
        serde_json::from_str(&body).context("parsing Ollama manifest")?;
    let digest = manifest["layers"]
        .as_array()
        .and_then(|layers| {
            layers
                .iter()
                .find(|l| l["mediaType"] == "application/vnd.ollama.image.model")
        })
        .and_then(|l| l["digest"].as_str())
        .ok_or_else(|| anyhow!("no model layer in Ollama manifest for {repo}:{tag}"))?;
    Ok(format!("{base}/blobs/{digest}"))
}

fn try_download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let resp = agent.get(url).call().with_context(|| format!("requesting {url}"))?;
    let total = resp.header("Content-Length").and_then(|h| h.parse::<u64>().ok());
    let tmp = dest.with_extension("partial");
    let mut file = std::fs::File::create(&tmp)?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut downloaded = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        progress(downloaded, total);
    }
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Hits the network — run manually: `cargo test -p zord-net -- --ignored`.
    /// Confirms the native-tls (OS cert store) agent actually connects + downloads.
    #[test]
    #[ignore]
    fn downloads_over_native_tls() {
        let dest = std::env::temp_dir().join("zord-net-test.txt");
        let _ = std::fs::remove_file(&dest);
        download_to_file(
            "https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/master/README.md",
            &dest,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(std::fs::metadata(&dest).unwrap().len() > 0);
    }

    /// Hits the network — `cargo test -p zord-net -- --ignored`. Validates the
    /// Ollama manifest fetch + JSON parse + model-layer digest extraction
    /// (without downloading the ~1 GB blob).
    #[test]
    #[ignore]
    fn resolves_ollama_blob_url() {
        let url = ollama_blob_url(&agent(), "qwen2.5", "1.5b").unwrap();
        assert!(url.contains("/blobs/sha256:"), "unexpected blob url: {url}");
    }
}
