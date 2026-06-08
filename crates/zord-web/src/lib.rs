//! Read-only review dashboard served on `127.0.0.1` only. All processing stays
//! local; this is just a convenient browser surface over the same SQLite data.
//!
//! Routes:
//! - `GET /`                  → the dashboard HTML/JS page
//! - `GET /api/sessions`      → `[Session]`
//! - `GET /api/session/{id}`   → `[Segment]`
//! - `GET /api/search?q=...`  → `[{ session_id, segment }]`

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use zord_core::Segment;
use zord_store::Store;

const INDEX_HTML: &str = include_str!("dashboard.html");

#[derive(Clone)]
struct AppState {
    db_path: Arc<PathBuf>,
}

/// Build the router (exposed for testing / embedding).
pub fn app(db_path: PathBuf) -> Router {
    let state = AppState {
        db_path: Arc::new(db_path),
    };
    Router::new()
        .route("/", get(index))
        .route("/api/sessions", get(sessions))
        .route("/api/session/{id}", get(session))
        .route("/api/search", get(search))
        .with_state(state)
}

/// Run the dashboard, blocking the caller (creates its own Tokio runtime).
/// Convenient for the CLI, which is otherwise synchronous.
pub fn serve_blocking(db_path: PathBuf, port: u16) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    rt.block_on(serve(db_path, port))
}

/// Async entry point.
pub async fn serve(db_path: PathBuf, port: u16) -> Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("Zord dashboard on http://{addr}");
    println!("Zord dashboard running at http://{addr}  (Ctrl-C to stop)");
    axum::serve(listener, app(db_path)).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    // Defense-in-depth headers. `default-src 'self'` keeps any injected content
    // from exfiltrating off-origin or loading remote code; `object-src`/`base-uri`
    // 'none' close those vectors. The page ships one inline <script>, so script
    // needs 'unsafe-inline' — the actual stored-XSS vector is already closed by
    // escaping every dynamic field in the page, so this is hardening, not the fix.
    let headers = [
        (
            axum::http::header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'",
        ),
        (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (axum::http::header::REFERRER_POLICY, "no-referrer"),
    ];
    (headers, Html(INDEX_HTML))
}

/// Run a blocking SQLite query off the async worker threads.
async fn with_store<T, F>(state: &AppState, f: F) -> Result<T, StatusCode>
where
    F: FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || {
        let store = Store::open(&*db_path)?;
        f(&store)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn sessions(State(state): State<AppState>) -> impl IntoResponse {
    match with_store(&state, |s| s.list_sessions()).await {
        Ok(v) => Json(v).into_response(),
        Err(c) => c.into_response(),
    }
}

async fn session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match with_store(&state, move |s| s.segments(&id)).await {
        Ok(v) => Json(v).into_response(),
        Err(c) => c.into_response(),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
struct SearchHit {
    session_id: String,
    segment: Segment,
}

async fn search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    let query = sanitize_fts(&q.q);
    if query.is_empty() {
        return Json(Vec::<SearchHit>::new()).into_response();
    }
    match with_store(&state, move |s| s.search(&query)).await {
        Ok(v) => {
            let hits: Vec<SearchHit> = v
                .into_iter()
                .map(|(session_id, segment)| SearchHit { session_id, segment })
                .collect();
            Json(hits).into_response()
        }
        Err(c) => c.into_response(),
    }
}

/// Mirror the GUI's FTS sanitization: quoted prefix terms, AND-ed.
fn sanitize_fts(q: &str) -> String {
    q.split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}
