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
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Hard ceiling for any single download (bytes). Generous enough for the
/// largest GGUF/ONNX assets (a few GB) while bounding a malicious/compromised
/// mirror that streams forever → disk-exhaustion DoS.
const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// Build an agent that trusts the OS cert store and uses an env proxy if set.
fn agent() -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .user_agent(concat!("zord/", env!("CARGO_PKG_VERSION")));

    match native_tls::TlsConnector::new() {
        Ok(connector) => builder = builder.tls_connector(Arc::new(connector)),
        Err(e) => tracing::warn!("native-tls connector unavailable: {e}"),
    }

    builder = apply_env_proxy(builder);
    builder.build()
}

/// Apply an env-var proxy (if one is set) to an in-progress agent builder.
fn apply_env_proxy(mut builder: ureq::AgentBuilder) -> ureq::AgentBuilder {
    if let Some(p) = proxy_from_env() {
        match ureq::Proxy::new(&p) {
            Ok(proxy) => {
                tracing::info!("using proxy from environment");
                builder = builder.proxy(proxy);
            }
            Err(e) => tracing::warn!("ignoring invalid proxy '{p}': {e}"),
        }
    }
    builder
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

/// POST a JSON body and stream the Server-Sent-Events response, calling
/// `on_data` with each `data:` payload (the `[DONE]` sentinel is filtered
/// out). Used for streaming LLM completions.
pub fn post_sse(
    url: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
    timeout: Duration,
    on_data: &mut dyn FnMut(&str),
) -> Result<(), ApiError> {
    use std::io::BufRead;
    let mut req = agent()
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream")
        .timeout(timeout);
    if let Some(key) = bearer.filter(|k| !k.trim().is_empty()) {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    match req.send_string(&body.to_string()) {
        Ok(resp) => {
            // A streaming completion has no Content-Length, so ureq returns an
            // unbounded reader; `lines()` would accumulate a single never-newline
            // body until OOM. Cap the whole stream so a malicious/MITM'd server
            // can't exhaust memory (chat replies are kilobytes; 64 MiB is ample).
            const MAX_SSE_BYTES: u64 = 64 * 1024 * 1024;
            let reader = std::io::BufReader::new(resp.into_reader().take(MAX_SSE_BYTES));
            for line in reader.lines() {
                let line =
                    line.map_err(|e| ApiError::Connect(format!("reading stream: {e}")))?;
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        break;
                    }
                    if !data.is_empty() {
                        on_data(data);
                    }
                }
            }
            Ok(())
        }
        Err(ureq::Error::Status(code, resp)) => Err(ApiError::Status {
            code,
            body: resp.into_string().unwrap_or_default(),
        }),
        Err(ureq::Error::Transport(t)) => Err(ApiError::Connect(t.to_string())),
    }
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
    download_with(&agent(), url, dest, None, progress)
}

/// Shared download driver: 3 retries around [`try_download`]. `expected_sha256`
/// (a `sha256:<hex>` or bare-hex digest) is verified against the bytes before
/// the temp→dest rename when present.
fn download_with(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let mut last_err = None;
    for attempt in 1..=3 {
        match try_download(agent, url, dest, expected_sha256, progress) {
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
/// standard GGUF) to `dest`, **verifying its bytes against the layer's
/// content-addressed digest** (the integrity guarantee that survives a trusted
/// MITM CA / compromised mirror). Reaches the registry via the same
/// OS-cert-store + proxy agent as everything else.
pub fn download_ollama_model(
    repo: &str,
    tag: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let agent = agent();
    let (blob_url, digest) = ollama_blob_url(&agent, repo, tag)?;
    tracing::info!(%blob_url, "downloading GGUF from Ollama registry");
    download_with(&agent, &blob_url, dest, Some(&digest), progress)
}

/// Resolve an Ollama `repo:tag` to `(blob URL, sha256 digest)` of its GGUF
/// model layer. The digest is content-addressed (it *is* the URL path), so
/// verifying the downloaded bytes against it detects substitution.
fn ollama_blob_url(agent: &ureq::Agent, repo: &str, tag: &str) -> Result<(String, String)> {
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
    Ok((format!("{base}/blobs/{digest}"), digest.to_string()))
}

fn try_download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<()> {
    let resp = agent.get(url).call().with_context(|| format!("requesting {url}"))?;
    let total = resp.header("Content-Length").and_then(|h| h.parse::<u64>().ok());
    // Reject an oversized advertised length up front (cheap DoS guard).
    if let Some(t) = total {
        if t > MAX_DOWNLOAD_BYTES {
            anyhow::bail!("{url}: refusing {t}-byte download (over {MAX_DOWNLOAD_BYTES}-byte cap)");
        }
    }
    let tmp = dest.with_extension("partial");
    let mut file = std::fs::File::create(&tmp)?;
    let mut reader = resp.into_reader();
    let mut hasher = expected_sha256.map(|_| Sha256::new());
    let mut buf = vec![0u8; 1 << 20];
    let mut downloaded = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        downloaded += n as u64;
        // Enforce the cap mid-stream too: a chunked/close-delimited response can
        // exceed (or omit) Content-Length. Abandon the partial file on overflow.
        if downloaded > MAX_DOWNLOAD_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("{url}: download exceeded {MAX_DOWNLOAD_BYTES}-byte cap");
        }
        file.write_all(&buf[..n])?;
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        progress(downloaded, total);
    }
    file.flush()?;
    drop(file);
    // Verify the content digest (if known) before publishing the file.
    verify_digest(hasher, expected_sha256, &tmp, url)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Verify the streamed bytes' digest (when known) against `expected_sha256`,
/// removing the partial `tmp` file and failing on mismatch.
fn verify_digest(
    hasher: Option<Sha256>,
    expected_sha256: Option<&str>,
    tmp: &Path,
    url: &str,
) -> Result<()> {
    if let (Some(h), Some(expected)) = (hasher, expected_sha256) {
        let got = h.finalize();
        let want = expected.strip_prefix("sha256:").unwrap_or(expected);
        if !got.iter().map(|b| format!("{b:02x}")).collect::<String>().eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("{url}: sha256 mismatch (expected {want}) — refusing tampered download");
        }
    }
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
        let (url, digest) = ollama_blob_url(&agent(), "qwen2.5", "1.5b").unwrap();
        assert!(url.contains("/blobs/sha256:"), "unexpected blob url: {url}");
        assert!(digest.starts_with("sha256:"), "unexpected digest: {digest}");
        assert!(url.ends_with(&digest), "url must be content-addressed by the digest");
    }

    /// Offline unit test: a download whose bytes don't match the claimed digest
    /// must be rejected and leave no file behind.
    #[test]
    fn rejects_sha256_mismatch() {
        // Serve a tiny body from a local one-shot server, claim a bogus digest.
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b);
            let body = b"not the expected bytes";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
            s.write_all(body).unwrap();
        });
        let dest = std::env::temp_dir().join(format!("zord-net-sha-{port}.bin"));
        let _ = std::fs::remove_file(&dest);
        let bogus = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let r = download_with(
            &agent(),
            &format!("http://127.0.0.1:{port}/blob"),
            &dest,
            Some(bogus),
            &mut |_, _| {},
        );
        server.join().unwrap();
        assert!(r.is_err(), "mismatched digest must fail");
        assert!(!dest.exists(), "no file should be left on digest mismatch");
        assert!(!dest.with_extension("partial").exists(), "partial must be cleaned up");
    }
}
