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
}
