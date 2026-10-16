// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shared helpers for the DB-backed route tests of `access` and `ownership`.
//! Gated on `LAKKO_DS_TEST_DATABASE_URL`; [`app`] returns `None` (and every
//! test returns early) without it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token::TokenContext;
use lakko_ds_proof::statement::{RegisterMode, StatementBody, StatementSigner};
use lakko_ds_proof::{sign, RequestParts, SigningKey, PROOF_HEADER_NAME};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use tower::ServiceExt as _;

use crate::access::now_ms;
use crate::journal::{self, PSEUDONYM_LEN};
use crate::AppState;

/// Parallel tests share one database; sqlx's migrator does not tolerate
/// two concurrent runs on it, so migrate once per process.
pub(crate) static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

pub(crate) async fn app() -> Option<(Router, MySqlPool)> {
    app_with_waker().await.map(|(app, pool, _)| (app, pool))
}

/// [`app`], plus the (dormant) waker so a test can see which wakes fired.
pub(crate) async fn app_with_waker(
) -> Option<(Router, MySqlPool, std::sync::Arc<crate::wake::Waker>)> {
    app_configured().await
}

/// The test app with its waker.
pub(crate) async fn app_configured(
) -> Option<(Router, MySqlPool, std::sync::Arc<crate::wake::Waker>)> {
    let Ok(url) = std::env::var("LAKKO_DS_TEST_DATABASE_URL") else {
        eprintln!("skipping: LAKKO_DS_TEST_DATABASE_URL unset");
        return None;
    };
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test db");
    MIGRATED
        .get_or_init(|| async {
            crate::MIGRATIONS.run(&pool).await.expect("migrate");
        })
        .await;
    let waker = std::sync::Arc::new(crate::wake::Waker::dormant());
    let state = AppState {
        db: pool.clone(),
        notifiers: journal::Notifiers::default(),
        waker: std::sync::Arc::clone(&waker),
        anon_tokens: lakko_anon_token_server::TokenVerifier::new(
            pool.clone(),
            Some(
                lakko_anon_token_server::AnonTokenServerConfig::from_values(
                    Some(TOKEN_MASTER_HEX),
                    None,
                )
                .unwrap()
                .unwrap(),
            ),
        ),
    };
    Some((crate::build_app(state), pool, waker))
}

pub(crate) const TOKEN_MASTER_HEX: &str =
    "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b";

/// A spendable anonymous token for `context` at `epochs_ago` epochs
/// before the current one, as the `x-lakko-anon-token` header value.
pub(crate) fn token_at(context: TokenContext, epochs_ago: u64) -> String {
    let now = lakko_anon_token_server::current_epoch(lakko_anon_token::DEFAULT_WINDOW_SECS);
    lakko_anon_token_server::testing::mint_header(
        &lakko_anon_token::MasterSecret::from_hex(TOKEN_MASTER_HEX).unwrap(),
        context,
        now - epochs_ago,
    )
}

pub(crate) fn knock_token() -> String {
    token_at(TokenContext::DsKnock, 0)
}

pub(crate) fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) async fn knock(
    app: &Router,
    uri: &str,
    payloads: &[&[u8]],
    token: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let payloads: Vec<String> = payloads.iter().map(|p| B64.encode(p)).collect();
    let body = serde_json::json!({ "payloads_b64": payloads })
        .to_string()
        .into_bytes();
    let headers: Vec<(&str, &str)> = token
        .map(|t| vec![(lakko_anon_token::TOKEN_HEADER, t)])
        .unwrap_or_default();
    send_with(
        app,
        Req {
            method: "POST",
            uri,
            body,
            proof: None,
        },
        &headers,
    )
    .await
}

pub(crate) static COUNTER: AtomicU64 = AtomicU64::new(1);

/// A fresh, unique 32-byte name per call so parallel tests never share
/// a journal.
pub(crate) fn fresh_name(tag: u8) -> [u8; PSEUDONYM_LEN] {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut out = [0u8; PSEUDONYM_LEN];
    out[0] = tag;
    out[1..9].copy_from_slice(&n.to_be_bytes());
    out[9..17].copy_from_slice(&now_ms().to_be_bytes());
    out
}

pub(crate) fn fresh_nonce() -> [u8; lakko_ds_proof::NONCE_LEN] {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut out = [0u8; lakko_ds_proof::NONCE_LEN];
    out[..8].copy_from_slice(&n.to_be_bytes());
    out[8..].copy_from_slice(&now_ms().to_be_bytes());
    out
}

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

pub(crate) fn pk_b64(key: &SigningKey) -> String {
    B64.encode(key.verifying_key().to_bytes())
}

pub(crate) struct Req<'a> {
    pub(crate) method: &'a str,
    pub(crate) uri: &'a str,
    pub(crate) body: Vec<u8>,
    pub(crate) proof: Option<(&'a SigningKey, u64)>,
}

pub(crate) fn proof_header(req: &Req<'_>) -> Option<String> {
    let (key, epoch) = req.proof?;
    let (path, query) = req.uri.split_once('?').unwrap_or((req.uri, ""));
    let parts = RequestParts {
        method: req.method,
        canonical_path: path,
        query,
        body: &req.body,
    };
    Some(sign(key, &parts, epoch, now_ms(), fresh_nonce()).encode())
}

pub(crate) async fn send(app: &Router, req: Req<'_>) -> (StatusCode, Vec<u8>) {
    send_with(app, req, &[]).await
}

pub(crate) async fn send_with(
    app: &Router,
    req: Req<'_>,
    extra: &[(&str, &str)],
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().method(req.method).uri(req.uri);
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    if !req.body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(h) = proof_header(&req) {
        builder = builder.header(PROOF_HEADER_NAME, h);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(req.body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, body)
}

/// The smallest well-formed mailbox row (ADR-0078): version byte, then a
/// bucket-padded sealed envelope - 1 + 32 + 1024 + 16 bytes. The DS checks
/// only that shape, so a test row carries `body` in the clear behind a
/// one-byte length, which [`mailbox_body`] reads back.
pub(crate) fn mailbox_row(body: &[u8]) -> Vec<u8> {
    let len = u8::try_from(body.len()).expect("a test body under 256 bytes");
    let mut row = vec![0u8; 1 + 32 + 1024 + 16];
    row[0] = 1;
    row[1] = len;
    row[2..2 + body.len()].copy_from_slice(body);
    assert!(lakko_ds_proof::is_well_formed_mailbox_row(&row));
    row
}

/// The body a [`mailbox_row`] carries.
pub(crate) fn mailbox_body(row: &[u8]) -> Vec<u8> {
    let len = usize::from(row[1]);
    row[2..2 + len].to_vec()
}

pub(crate) fn messages_uri(kind: &str, name: &[u8; PSEUDONYM_LEN]) -> String {
    format!("/ds/{kind}/{}/messages", hex::encode(name))
}

/// A fresh root or registrar signer (fresh keys per call: parallel tests and
/// reruns against one database never share a signer).
pub(crate) fn fresh_signer(tag: u8) -> StatementSigner {
    StatementSigner::from_seed(&fresh_name(tag))
}

/// `PUT /ds/group/{hex}/<segment>` with `body` signed by `signer` now.
pub(crate) async fn send_statement(
    app: &Router,
    gid: &[u8; PSEUDONYM_LEN],
    signer: &StatementSigner,
    body: StatementBody,
) -> StatusCode {
    let statement = signer
        .sign_at(gid, body, now_ms(), fresh_nonce())
        .expect("sign statement");
    send(
        app,
        Req {
            method: "PUT",
            uri: &format!(
                "/ds/group/{}/{}",
                hex::encode(gid),
                statement.path_segment()
            ),
            body: statement.body_json().into_bytes(),
            proof: None,
        },
    )
    .await
    .0
}

/// Claim `gid` with `root`, `epoch_key` at `epoch`.
pub(crate) async fn claim(
    app: &Router,
    gid: &[u8; PSEUDONYM_LEN],
    root: &StatementSigner,
    epoch: u64,
    epoch_key: &SigningKey,
) -> StatusCode {
    send_statement(
        app,
        gid,
        root,
        StatementBody::Claim {
            epoch,
            epoch_key: epoch_key.verifying_key().to_bytes(),
        },
    )
    .await
}

/// Append `epoch_key` at `epoch` as `signer`, revoking `revoke`.
pub(crate) async fn register(
    app: &Router,
    gid: &[u8; PSEUDONYM_LEN],
    signer: &StatementSigner,
    epoch: u64,
    epoch_key: &SigningKey,
    revoke: &[&StatementSigner],
) -> StatusCode {
    register_as(
        app,
        gid,
        signer,
        RegisterMode::Append,
        epoch,
        epoch_key,
        revoke,
    )
    .await
}

/// Replace at `epoch` with `epoch_key` as `signer`, revoking `revoke`.
pub(crate) async fn replace(
    app: &Router,
    gid: &[u8; PSEUDONYM_LEN],
    signer: &StatementSigner,
    epoch: u64,
    epoch_key: &SigningKey,
    revoke: &[&StatementSigner],
) -> StatusCode {
    register_as(
        app,
        gid,
        signer,
        RegisterMode::Replace,
        epoch,
        epoch_key,
        revoke,
    )
    .await
}

/// A register-epoch statement in `mode`.
pub(crate) async fn register_as(
    app: &Router,
    gid: &[u8; PSEUDONYM_LEN],
    signer: &StatementSigner,
    mode: RegisterMode,
    epoch: u64,
    epoch_key: &SigningKey,
    revoke: &[&StatementSigner],
) -> StatusCode {
    send_statement(
        app,
        gid,
        signer,
        StatementBody::RegisterEpoch {
            epoch,
            epoch_key: epoch_key.verifying_key().to_bytes(),
            mode,
            revoke: revoke.iter().map(|s| s.public_key()).collect(),
        },
    )
    .await
}
