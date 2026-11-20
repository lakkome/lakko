// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ciphertext blob storage for Lakko.
//!
//! Clients `PUT` ciphertext keyed by `SHA-256(ciphertext)` and fetch it back
//! by the same hash. The server is blind: it cannot decrypt a blob and does
//! not learn who uploaded or who reads one. The hash is over the ciphertext,
//! so the server cannot deduplicate across people who encrypt the same
//! plaintext under different keys.
//!
//! Backend ([`store`]): the local filesystem rooted at `LAKKO_BLOB_DIR`, laid
//! out `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`, or an S3-compatible bucket when
//! the `LAKKO_STORAGE_S3_*` variables are set (ADR-0065 amendment of
//! 2026-11-19); one storage node today. Upload temporaries and the account
//! backups stay under `LAKKO_BLOB_DIR` with either backend.
//!
//! `lakko-svc-storage migrate-to-s3` copies every blob file under
//! `LAKKO_BLOB_DIR` into the configured bucket (skipping objects already
//! there at the same length, checking each copy by `HEAD`), prints the
//! counts and exits non-zero on any failure. It deletes nothing locally.
//!
//! # ADR-0065: lifetime, owner delete, a gate
//!
//! - An index row per blob ([`index`]); every blob request is answered from
//!   it. MariaDB when `LAKKO_STORAGE_DATABASE_URL` is set, else an
//!   append-only index file beside the blobs (one instance only).
//! - A blob nobody downloads, keeps or re-uploads for
//!   `LAKKO_STORAGE_BLOB_LIFETIME_DAYS` (365) is deleted by a daily sweep
//!   ([`sweep`]).
//! - Upload and download need an hour's pass bought with one anonymous token
//!   ([`pass`]); `PUT` additionally carries the blob's delete public key, and
//!   `DELETE` needs a signature under it ([`blob`] lists the routes and
//!   codes).
//! - Anti-abuse: the pass budgets, the token quotas at `services/auth`, hash
//!   verification on `PUT`, the 50 MiB ceiling here and at nginx, and an
//!   nginx per-address `limit_req` on the blob locations
//!   (`lakko_storage_blob` zone) as a backstop against floods of invalid
//!   passes and tokens.
//!
//! # Configuration (environment; `/etc/lakko/lakko.env` under systemd)
//!
//! - `LAKKO_BIND` - `host:port` (default `127.0.0.1:8083`)
//! - `LAKKO_BLOB_DIR` - blob root (default `./target/dev-blobs/`); with S3,
//!   the upload temporaries and the backups only
//! - `LAKKO_STORAGE_S3_ENDPOINT`, `LAKKO_STORAGE_S3_BUCKET`,
//!   `LAKKO_STORAGE_S3_REGION`, `LAKKO_STORAGE_S3_ACCESS_KEY`,
//!   `LAKKO_STORAGE_S3_SECRET_KEY`, `LAKKO_STORAGE_S3_PATH_STYLE` - the
//!   bucket ([`store::s3`]); all unset = the local directory
//! - `LAKKO_SESSION_TOKEN_SECRET` - the session-token key (backup routes)
//! - `LAKKO_STORAGE_DATABASE_URL` - MariaDB for the index and the token
//!   spent-set; unset = the file index at `<LAKKO_BLOB_DIR>/.lakko-index/`
//!   and an in-memory spent-set
//! - `LAKKO_STORAGE_NODE` - this node's name in the index (default `local`;
//!   ASCII letters, digits, `-`, `_`, `.`, at most 64)
//! - `LAKKO_STORAGE_PASS_SECRET` - 64 hex, the pass MAC key; unset = every
//!   blob route except `DELETE` answers 503 `blob-pass-disabled`
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` / `LAKKO_ANON_TOKEN_WINDOW_SECS` - the
//!   anonymous-token verifier shared with `services/auth`
//! - `LAKKO_STORAGE_PASS_TTL_SECS` (3600), `LAKKO_STORAGE_UPLOAD_PASS_FILES`
//!   (300), `LAKKO_STORAGE_UPLOAD_PASS_BYTES` (2147483648),
//!   `LAKKO_STORAGE_READ_PASS_DOWNLOADS` (3000),
//!   `LAKKO_STORAGE_READ_PASS_KEEPS` (5000) - chosen, not measured
//! - `LAKKO_STORAGE_BLOB_LIFETIME_DAYS` (365; 0 disables the sweep),
//!   `LAKKO_STORAGE_SWEEP_INTERVAL_SECS` (86400)
//! - `LAKKO_STORAGE_VAULT_MAX_BYTES` (67108864),
//!   `LAKKO_STORAGE_VAULT_MAX_CHUNKS` (256),
//!   `LAKKO_STORAGE_VAULT_WRITES_PER_HOUR` (240) - per-vault limits, chosen,
//!   not measured; the vault index lives beside the blob index (same
//!   database, or the same index file)
//! - `RUST_LOG` - `tracing-subscriber` env-filter spec
//!
//! # Endpoints
//!
//! - `GET /health`, `GET /ready`, `GET /version`
//! - `POST /blob/pass/upload`, `POST /blob/pass/read`
//! - `PUT` / `GET` / `HEAD` / `DELETE /blob/{sha256_hex}`, `POST /blob/keep`
//! - `PUT` / `GET` / `DELETE /backup` - the per-account backup blob, session
//!   token (unchanged; not indexed, never swept)
//! - `PUT` / `DELETE /vault/{id}`, `PUT` / `GET /vault/{id}/head`,
//!   `PUT` / `GET /vault/{id}/chunk/{n}` - the conversation vaults of
//!   ADR-0064 section 1 ([`vault`] lists the credentials and codes)

#![forbid(unsafe_code)]

mod auth;
mod backup;
mod blob;
mod disk;
mod index;
mod internal;
mod metrics;
mod pass;
mod store;
mod stream;
mod sweep;
mod vault;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post, put};
use axum::Router;
use lakko_anon_token_server::{AnonTokenServerConfig, TokenVerifier};
use lakko_auth_token::TokenSecret;
use serde_json::json;
use tokio::signal;

use crate::index::{BlobIndex, VaultIndex, VaultLimits};
use crate::pass::{BudgetLedger, PassLimits, PassSecret, PASS_SECRET_VAR};
use crate::store::{BlobStore, FsStore};

/// Default bind address if `LAKKO_BIND` is not set.
const DEFAULT_BIND: &str = "127.0.0.1:8083";

/// Default blob root for local development.
const DEFAULT_BLOB_DIR: &str = "./target/dev-blobs";

/// Per-blob ceiling. Mirrored in nginx `client_max_body_size`.
pub(crate) const MAX_BLOB_BYTES: usize = 50 * 1024 * 1024;

/// Upload temporaries of the S3 backend, under the blob root. A dot
/// directory, so the import and the migration never take it for a shard.
const S3_TEMP_DIR: &str = ".lakko-upload";

/// The index file of the file-backed index, under the blob root. A dot
/// directory, so the import never takes it for a blob shard.
const INDEX_FILE: &str = ".lakko-index/blobs.log";

/// The service's migrations (`migrations/`), run at startup on the database.
pub(crate) static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Shared application state. Cheap to clone.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Local directory: upload temporaries, backups, the free-space check.
    pub(crate) blob_root: Arc<PathBuf>,
    /// Where blob bytes rest.
    pub(crate) store: Arc<dyn BlobStore>,
    /// Session-token key for the per-account backup routes.
    pub(crate) session_token_secret: Arc<TokenSecret>,
    /// The blob index.
    pub(crate) index: Arc<dyn BlobIndex>,
    /// This node's name in the index.
    pub(crate) node: Arc<str>,
    /// The pass MAC key; `None` = the pass routes and the gated blob routes
    /// answer 503.
    pub(crate) pass_secret: Option<Arc<PassSecret>>,
    /// Pass lifetime and budgets.
    pub(crate) limits: PassLimits,
    /// What each live pass has spent.
    pub(crate) ledger: Arc<BudgetLedger>,
    /// The anonymous-token spend gate for the two pass routes and the vault
    /// claim.
    pub(crate) tokens: TokenVerifier,
    /// The vault index (the same store as `index`).
    pub(crate) vaults: Arc<dyn VaultIndex>,
    /// Per-vault limits.
    pub(crate) vault_limits: VaultLimits,
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .with_context(|| format!("{name} must be a non-negative integer")),
        _ => Ok(default),
    }
}

fn valid_node(node: &str) -> bool {
    !node.is_empty()
        && node.len() <= 64
        && node
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn pass_limits_from_env() -> Result<PassLimits> {
    let d = PassLimits::default();
    let limits = PassLimits {
        ttl_secs: env_u64("LAKKO_STORAGE_PASS_TTL_SECS", d.ttl_secs)?,
        upload_files: env_u64("LAKKO_STORAGE_UPLOAD_PASS_FILES", d.upload_files)?,
        upload_bytes: env_u64("LAKKO_STORAGE_UPLOAD_PASS_BYTES", d.upload_bytes)?,
        read_downloads: env_u64("LAKKO_STORAGE_READ_PASS_DOWNLOADS", d.read_downloads)?,
        read_keeps: env_u64("LAKKO_STORAGE_READ_PASS_KEEPS", d.read_keeps)?,
        min_free_bytes: env_u64(crate::disk::MIN_FREE_VAR, d.min_free_bytes)?,
    };
    if limits.ttl_secs == 0 {
        bail!("LAKKO_STORAGE_PASS_TTL_SECS must be positive");
    }
    Ok(limits)
}

fn vault_limits_from_env() -> Result<VaultLimits> {
    let d = vault::DEFAULT_LIMITS;
    let limits = VaultLimits {
        max_bytes: env_u64("LAKKO_STORAGE_VAULT_MAX_BYTES", d.max_bytes)?,
        max_chunks: u32::try_from(env_u64(
            "LAKKO_STORAGE_VAULT_MAX_CHUNKS",
            u64::from(d.max_chunks),
        )?)
        .context("LAKKO_STORAGE_VAULT_MAX_CHUNKS out of range")?,
        max_writes_per_hour: u32::try_from(env_u64(
            "LAKKO_STORAGE_VAULT_WRITES_PER_HOUR",
            u64::from(d.max_writes_per_hour),
        )?)
        .context("LAKKO_STORAGE_VAULT_WRITES_PER_HOUR out of range")?,
        reservation_ms: d.reservation_ms,
    };
    if limits.max_bytes == 0 || limits.max_writes_per_hour == 0 {
        bail!("LAKKO_STORAGE_VAULT_MAX_BYTES and LAKKO_STORAGE_VAULT_WRITES_PER_HOUR must be positive");
    }
    Ok(limits)
}

fn pass_secret_from_env() -> Result<Option<Arc<PassSecret>>> {
    match std::env::var(PASS_SECRET_VAR) {
        Ok(v) if !v.trim().is_empty() => Ok(Some(Arc::new(
            PassSecret::from_hex(&v).context("LAKKO_STORAGE_PASS_SECRET must be 64 hex")?,
        ))),
        _ => {
            tracing::warn!(
                "blob passes DISABLED: LAKKO_STORAGE_PASS_SECRET is not set; every blob route \
                 except DELETE answers 503 blob-pass-disabled"
            );
            Ok(None)
        },
    }
}

/// The blob index, the vault index over the same store, the token verifier
/// over the same store, and the file index when that is the one in use (it
/// is compacted after a sweep).
type OpenedIndex = (
    Arc<dyn BlobIndex>,
    Arc<dyn VaultIndex>,
    TokenVerifier,
    Option<Arc<index::file::FileIndex>>,
);

/// MariaDB when `LAKKO_STORAGE_DATABASE_URL` is set (migrated here), else
/// the index file under the blob root and an in-memory spent-set.
async fn open_index(
    blob_root: &std::path::Path,
    token_config: Option<AnonTokenServerConfig>,
) -> Result<OpenedIndex> {
    let database_url = std::env::var("LAKKO_STORAGE_DATABASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    if let Some(url) = database_url {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(10)
            .connect(&url)
            .await
            .context("connect LAKKO_STORAGE_DATABASE_URL")?;
        MIGRATIONS
            .run(&pool)
            .await
            .context("run storage migrations")?;
        // Connections in use and idle (ADR-0096); only with a database.
        let sampled = pool.clone();
        let _pool_gauge =
            lakko_metrics::spawn_pool_gauge(move || (sampled.size(), sampled.num_idle()));
        tracing::info!("blob and vault index: MariaDB");
        let sql = Arc::new(index::sql::SqlIndex::new(pool.clone()));
        return Ok((
            sql.clone(),
            sql,
            TokenVerifier::new(pool, token_config),
            None,
        ));
    }
    let f = Arc::new(
        index::file::FileIndex::open(blob_root.join(INDEX_FILE))
            .context("open the blob index file")?,
    );
    tracing::info!(
        "blob and vault index: file (single instance; set LAKKO_STORAGE_DATABASE_URL for MariaDB)"
    );
    Ok((
        f.clone(),
        f.clone(),
        TokenVerifier::in_memory(token_config),
        Some(f),
    ))
}

/// The S3 store, checked, or `None` when no bucket is configured.
async fn open_s3(blob_root: &std::path::Path) -> Result<Option<Arc<store::s3::S3Store>>> {
    let Some(config) = store::s3::S3Config::from_env().context("S3 storage config")? else {
        return Ok(None);
    };
    let s3 = store::s3::S3Store::new(&config, blob_root.join(S3_TEMP_DIR))
        .context("S3 storage config")?;
    s3.check()
        .await
        .context("S3 bucket check (HEAD bucket must be 200, HEAD of an absent key 404)")?;
    Ok(Some(Arc::new(s3)))
}

/// Whether the command line asks for `migrate-to-s3` (the only argument
/// accepted).
fn wants_migration() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(false),
        [one] if one == "migrate-to-s3" => Ok(true),
        _ => bail!("usage: lakko-svc-storage [migrate-to-s3]"),
    }
}

/// The blob store: the bucket when configured, else the local directory.
async fn open_store(blob_root: &std::path::Path) -> Result<Arc<dyn BlobStore>> {
    if let Some(s3) = open_s3(blob_root).await? {
        tracing::info!(bucket = s3.bucket_name(), "blob store: S3");
        return Ok(s3);
    }
    tracing::info!("blob store: local directory");
    Ok(Arc::new(FsStore::new(blob_root.to_path_buf())))
}

/// `migrate-to-s3`: copy the local blob files into the bucket.
async fn migrate_to_s3(blob_root: &std::path::Path) -> Result<()> {
    let s3 = open_s3(blob_root)
        .await?
        .context("migrate-to-s3 needs the LAKKO_STORAGE_S3_* variables")?;
    let report = store::s3::migrate_files(&s3, blob_root).await?;
    tracing::info!(
        copied = report.copied,
        already_there = report.present,
        failed = report.failed,
        "migrate-to-s3 done"
    );
    if report.failed > 0 {
        bail!("migrate-to-s3: {} file(s) failed", report.failed);
    }
    Ok(())
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // flat startup wiring reads best whole
async fn main() -> Result<()> {
    // Load `.env` from the workspace root in debug builds. Production
    // picks up env vars from the systemd `EnvironmentFile=` directive.
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = dotenvy::dotenv() {
            eprintln!("dotenv loaded from {}", path.display());
        }
    }

    init_tracing();
    // /metrics on its own localhost port, off when unset (ADR-0096).
    lakko_metrics::start_from_env(
        "storage",
        env!("CARGO_PKG_VERSION"),
        "LAKKO_STORAGE_METRICS_BIND",
    )
    .await?;

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_string())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    let blob_root = PathBuf::from(
        std::env::var("LAKKO_BLOB_DIR").unwrap_or_else(|_| DEFAULT_BLOB_DIR.to_string()),
    );
    tokio::fs::create_dir_all(&blob_root)
        .await
        .with_context(|| format!("create LAKKO_BLOB_DIR at {}", blob_root.display()))?;
    store::probe_writable(&blob_root)
        .await
        .with_context(|| format!("LAKKO_BLOB_DIR not writable: {}", blob_root.display()))?;

    if wants_migration()? {
        return migrate_to_s3(&blob_root).await;
    }

    let store = open_store(&blob_root).await?;

    let session_token_secret_raw = std::env::var("LAKKO_SESSION_TOKEN_SECRET").context(
        "LAKKO_SESSION_TOKEN_SECRET env var must be set (base64-encoded HMAC-SHA256 key, \
         at least 32 bytes after decode)",
    )?;
    let session_token_secret = TokenSecret::from_base64(&session_token_secret_raw)
        .context("parse LAKKO_SESSION_TOKEN_SECRET as base64")?;

    let node: Arc<str> = std::env::var("LAKKO_STORAGE_NODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
        .into();
    if !valid_node(&node) {
        bail!("LAKKO_STORAGE_NODE must be 1..=64 of ASCII letters, digits, '-', '_', '.'");
    }

    let pass_secret = pass_secret_from_env()?;
    let limits = pass_limits_from_env()?;
    let vault_limits = vault_limits_from_env()?;
    let lifetime_days = u32::try_from(env_u64("LAKKO_STORAGE_BLOB_LIFETIME_DAYS", 365)?)
        .context("LAKKO_STORAGE_BLOB_LIFETIME_DAYS out of range")?;
    let sweep_interval = env_u64("LAKKO_STORAGE_SWEEP_INTERVAL_SECS", 86_400)?;
    if sweep_interval == 0 {
        bail!("LAKKO_STORAGE_SWEEP_INTERVAL_SECS must be positive");
    }

    let token_config = AnonTokenServerConfig::from_env().context("anonymous-token config")?;
    let (index, vaults, tokens, file_index) = open_index(&blob_root, token_config).await?;
    tokens.log_startup("storage");
    let _spent_pruner = tokens.spawn_pruner();

    match sweep::import_once(index.as_ref(), &blob_root, &node, index::today()).await {
        Ok(Some(n)) => tracing::info!(rows = n, "blob index: one-time import done"),
        Ok(None) => {},
        Err(e) => return Err(e.context("blob index import")),
    }

    let blob_root = Arc::new(blob_root);
    let _sweeper = if lifetime_days == 0 {
        tracing::warn!("blob lifetime sweep DISABLED: LAKKO_STORAGE_BLOB_LIFETIME_DAYS=0");
        None
    } else {
        let after: Option<Arc<dyn Fn() + Send + Sync>> = file_index.map(|f| {
            let f: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Err(e) = f.compact_now() {
                    tracing::warn!(error = %e, "compact the blob index file");
                }
            });
            f
        });
        Some(sweep::spawn_sweeper(
            index.clone(),
            store.clone(),
            node.clone(),
            lifetime_days,
            Duration::from_secs(sweep_interval),
            after,
        ))
    };

    let state = AppState {
        blob_root: blob_root.clone(),
        store,
        session_token_secret: Arc::new(session_token_secret),
        index,
        node,
        pass_secret,
        limits,
        ledger: Arc::new(BudgetLedger::default()),
        tokens,
        vaults,
        vault_limits,
    };

    // The internal listener auth removes a deleted account's backup through.
    // Both variables or neither; one alone refuses startup.
    let internal_config = internal::load_config()
        .map_err(anyhow::Error::msg)
        .context("internal listener")?;
    let _internal = internal::spawn(blob_root.clone(), internal_config).await?;

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, blob_root = %blob_root.display(), "storage service listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    tracing::info!("storage service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    // Per-account backup routes: the session-token middleware proves "the
    // caller holds a valid token for SOME account" and the handler takes the
    // account id from it; the URL carries none.
    let backup_routes = Router::new()
        .route(
            "/backup",
            put(backup::put_backup)
                .get(backup::get_backup)
                .route_layer(DefaultBodyLimit::max(backup::MAX_BACKUP_BYTES)),
        )
        .route("/backup", delete(backup::delete_backup))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/blob/pass/upload", post(blob::pass_upload))
        .route("/blob/pass/read", post(blob::pass_read))
        .route(
            "/blob/keep",
            post(blob::keep_blobs).route_layer(DefaultBodyLimit::max(blob::MAX_KEEP_BODY)),
        )
        .route(
            "/blob/{hex}",
            put(blob::put_blob)
                .get(blob::get_blob)
                .head(blob::head_blob)
                .delete(blob::delete_blob),
        )
        .layer(DefaultBodyLimit::max(MAX_BLOB_BYTES))
        .merge(vault_routes())
        .merge(backup_routes)
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("storage"))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(lakko_log_hygiene::request_span),
        )
        .with_state(state)
}

/// The vault routes (ADR-0064 section 1), each with its own body ceiling.
fn vault_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/vault/{hex}",
            put(vault::claim_vault)
                .delete(vault::delete_vault)
                .route_layer(DefaultBodyLimit::max(vault::MAX_CLAIM_BODY)),
        )
        .route(
            "/vault/{hex}/head",
            put(vault::put_head)
                .get(vault::get_head)
                .route_layer(DefaultBodyLimit::max(vault::MAX_OBJECT_BODY)),
        )
        .route(
            "/vault/{hex}/chunk/{n}",
            put(vault::put_chunk)
                .get(vault::get_chunk)
                .route_layer(DefaultBodyLimit::max(vault::MAX_OBJECT_BODY)),
        )
}

/// Liveness probe - process is alive, no I/O.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness probe - the local blob root is writable and the blob store is
/// usable (with S3: the bucket answers `HEAD`).
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if let Err(e) = store::probe_writable(&state.blob_root).await {
        tracing::warn!(error = %e, "readiness probe: blob root not writable");
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response();
    }
    match state.store.ready().await {
        Ok(()) => (StatusCode::OK, "ready\n").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, store = state.store.name(), "readiness probe: blob store not usable");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
        },
    }
}

/// Service version metadata.
async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-storage",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// Initialize tracing-subscriber from `RUST_LOG`. Falls back to `info`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .compact()
        .init();
}

/// Wait for SIGTERM (systemd stop) or SIGINT (Ctrl-C).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! One state builder for every test module.

    use super::*;

    /// The anonymous-token master secret of the tests.
    pub(crate) fn master_hex() -> String {
        "11".repeat(32)
    }

    /// A state over a fresh temporary blob root with a file index, a pass
    /// secret, the given limits and an in-memory token verifier.
    pub(crate) fn state_with(limits: PassLimits, passes: bool) -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret = TokenSecret::new(vec![0x42u8; 32]).expect("32 bytes is over MIN_SECRET_LEN");
        let index =
            Arc::new(index::file::FileIndex::open(tmp.path().join(INDEX_FILE)).expect("index"));
        let token_config =
            AnonTokenServerConfig::from_values(Some(&master_hex()), None).expect("config");
        let state = AppState {
            blob_root: Arc::new(tmp.path().to_path_buf()),
            store: Arc::new(FsStore::new(tmp.path().to_path_buf())),
            session_token_secret: Arc::new(secret),
            index: index.clone(),
            node: Arc::from("local"),
            pass_secret: passes.then(|| Arc::new(PassSecret::from_bytes([0x33; 32]))),
            limits,
            ledger: Arc::new(BudgetLedger::default()),
            tokens: TokenVerifier::in_memory(token_config),
            vaults: index,
            vault_limits: vault::DEFAULT_LIMITS,
        };
        (state, tmp)
    }

    /// [`state_with`] at the default limits, passes enabled.
    pub(crate) fn state() -> (AppState, tempfile::TempDir) {
        state_with(no_disk_floor(PassLimits::default()), true)
    }

    /// `limits` without the free-space floor, so a test does not depend on
    /// how full the machine running it is.
    pub(crate) const fn no_disk_floor(limits: PassLimits) -> PassLimits {
        PassLimits {
            min_free_bytes: 0,
            ..limits
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    async fn call(uri: &str) -> (StatusCode, Vec<u8>) {
        let (state, _tmp) = test_support::state();
        let response = build_app(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, body.to_vec())
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let (status, body) = call("/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"ok\n");
    }

    #[tokio::test]
    async fn ready_returns_200_when_blob_root_writable() {
        assert_eq!(call("/ready").await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn version_returns_json() {
        let (status, body) = call("/version").await;
        assert_eq!(status, StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["service"], "lakko-svc-storage");
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        assert_eq!(call("/does-not-exist").await.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn node_names_are_restricted() {
        assert!(valid_node("local"));
        assert!(valid_node("node-2.eu_1"));
        assert!(!valid_node(""));
        assert!(!valid_node("a b"));
        assert!(!valid_node(&"x".repeat(65)));
    }
}
