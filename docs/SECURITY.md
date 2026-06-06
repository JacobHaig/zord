# Security

## Review — June 2026

A comprehensive review covered six dimensions — supply chain, the network
download/decompression path, the external-LLM client, the localhost dashboard,
secret/at-rest handling, and filesystem/process safety — plus untrusted-WAV
parsing robustness. Each candidate finding was adversarially re-verified against
the threat model before being accepted.

**Threat model.** Zord is a fully-local, single-user desktop app: no server, no
multi-tenant surface, no auth except an opt-in `127.0.0.1`-only dashboard. The
real adversaries are (1) malicious/malformed *untrusted input* processed
automatically — downloaded model archives, audio read back off disk, external-LLM
responses; (2) supply-chain (known-vulnerable deps); (3) a network MITM /
compromised mirror on downloads; (4) at-rest exposure of secrets; (5) the local
dashboard and process-spawning helpers. A user misconfiguring their own machine
is **not** in scope.

**Posture.** No critical or high-severity issue survived verification. `cargo
audit` reports **0 vulnerabilities** across 681 crates (14 informational
warnings, all build-time or Linux-GUI-shell transitives). No RCE, no SQL
injection, no path traversal, no TLS bypass. Confirmed clean: TLS uses the OS
trust store with no `danger_accept_invalid_*` and no insecure fallback; all SQL
(including FTS `MATCH`) is parameterized; the dashboard is localhost-only and
read-only (no CSRF surface); the bearer token is dropped on redirect and never
logged; the SQLCipher key-application order cannot silently open a plaintext DB;
process spawning uses argv form (no shell injection) and never feeds
fetched/transcribed strings to a spawned argument; tar extraction is mitigated
against zip-slip by `tar 0.4.46`.

## Fixes applied

### Network integrity & DoS — commit `fc9e513`
- **Download digest verification.** Ollama GGUF blobs are content-addressed; the
  `sha256:` digest from the manifest (previously parsed only to build the URL,
  then discarded) is now streamed-hashed and compared before the `.partial`→dest
  rename. A mismatch deletes the file and errors. This is the only integrity
  guarantee once a corporate MITM CA or compromised mirror is trusted.
  `zord-net/src/lib.rs`.
- **Streaming-SSE memory cap.** The external-LLM SSE reader is bounded
  (`.take(64 MiB)`), with a paired 4 MiB cap on the accumulated reply — a server
  that streams a never-newline body can no longer grow `lines()` until OOM.
  `zord-net/src/lib.rs`, `zord-summarize/src/remote.rs`.
- **Download & decompression size caps.** A 16 GiB download ceiling (oversized
  `Content-Length` rejected up front; mid-stream overflow aborts and deletes the
  partial) and bzip2/tar decompression caps (2 GiB diarize / 4 GiB Parakeet)
  bound disk-exhaustion / decompression-bomb DoS from a compromised mirror.
  `zord-net/src/lib.rs`, `zord-diarize/src/diarizer.rs`,
  `zord-transcribe/src/model.rs`.

### Dashboard, at-rest secrets, input validation — commit `4f11c13`
- **Dashboard XSS.** Every dynamic field rendered into the dashboard's
  `innerHTML` (session title, model, id) is now HTML-escaped — the title is
  produced by the LLM summarizing the transcript, so call participants could
  prompt-inject markup. Defense-in-depth response headers added: a
  `Content-Security-Policy` (`default-src 'self'`, `object-src`/`base-uri 'none'`),
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.
  `zord-web/`.
- **Secrets at rest (0600).** `config.json` — which holds the external-LLM bearer
  token in plaintext — and the SQLCipher `.plaintext.bak` / `.encrypted.bak`
  backups (full transcript copies) are written owner-only (`0600`) on Unix
  instead of inheriting `0644`. `zord-config/src/lib.rs`, `zord-store/src/lib.rs`.
- **WAV header validation.** `validate_wav_spec` rejects `sample_rate == 0`
  (infinite resample ratio → huge allocation) and out-of-range
  `bits_per_sample` (scale-shift overflow) at every WAV reader and the offline
  pipeline, so a crafted/corrupt file errors cleanly instead of panicking.
  `zord-audio/src/wav.rs`, `zord-transcribe/src/offline.rs`.

## Deferred

- **Zeroizing the DB passphrase in memory** (`DB_KEY` is a plain `String`).
  Defense-in-depth only — exploitation presupposes process-memory / core-dump /
  swap access, i.e. the machine is already compromised. Tracked, not yet done.

## Reporting

Zord processes everything on-device and ships no telemetry. Security questions
or reports: open an issue on the repository.
