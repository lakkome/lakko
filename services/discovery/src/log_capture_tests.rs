// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0078 section 11, decision 5 half b: drive discovery's account- and
//! link-bearing routes through the real router under a capture subscriber,
//! and fail if any line at info level or above carries a value shaped like a
//! 16- or 32-byte identifier.
//!
//! Gated on `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips when unset), like the
//! other route tests. The routes covered are the ones whose handlers named an
//! account, a key or a ledger id in a log line before this phase: identity
//! upload (first publish, an account mismatch, an unproven key change),
//! KeyPackage upload and a mismatched one, and the three invite ledgers'
//! upload, consume and revoke.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use lakko_anon_token::{MasterSecret, TokenContext, DEFAULT_WINDOW_SECS, TOKEN_HEADER};
use lakko_anon_token_server::current_epoch;
use lakko_anon_token_server::testing::mint_header;
use lakko_crypto::signing::SigningKey;
use lakko_log_hygiene::capture::Capture;
use tower::ServiceExt as _;

use crate::anon_token_route_tests::{state, test_pool};
use crate::build_app;

/// Values allowed in the captured output although they have an identifier's
/// shape. Reviewed like code; empty today.
const ALLOW: &[&str] = &[];

const MASTER_HEX: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn fresh_account(tag: u8) -> [u8; 16] {
    let mut out = [tag; 16];
    let n = u64::try_from(now_ms()).unwrap_or(0).wrapping_mul(1_000_003);
    out[8..].copy_from_slice(&n.to_be_bytes());
    out
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, String)],
    body: serde_json::Value,
) -> (StatusCode, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-real-ip", "192.0.2.78")
        .header("content-type", "application/json");
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario across every covered route
async fn no_identifier_reaches_the_log_on_account_and_link_routes() {
    let Some(db) = test_pool("log_capture").await else {
        return;
    };
    let (capture, _guard) = Capture::install();
    // ADR-0096 section 1.5: the same routes must leave no identifier in
    // /metrics either.
    let metrics = lakko_metrics::install("discovery", env!("CARGO_PKG_VERSION")).unwrap();
    let st = state(db, true);
    let app = build_app(st.clone());
    let master = MasterSecret::from_hex(MASTER_HEX).unwrap();
    let epoch = current_epoch(DEFAULT_WINDOW_SECS);

    let account = fresh_account(0x78);
    let other = fresh_account(0x79);
    let token = |acct: &[u8; 16], device: u8| {
        st.session_token_secret
            .issue(acct, 1, &[device; 32], now_ms(), now_ms() + 60_000)
            .unwrap()
    };
    let bearer = |t: String| vec![("authorization", format!("Bearer {t}"))];

    // Identity: a first publish, a body naming another account, and an
    // unproven key change.
    let key = SigningKey::generate();
    let publish = |acct: &[u8; 16], key: &SigningKey| {
        serde_json::json!({
            "account_id": B64.encode(acct),
            "signature_pubkey": B64.encode(key.verifying_key().to_bytes()),
            "x25519_pubkey": B64.encode([0x33u8; 32]),
            "x25519_binding": B64.encode(lakko_contacts::sign_x25519_binding(key, &[0x33u8; 32])),
        })
    };
    let (status, body) = call(
        &app,
        "POST",
        "/discovery/identity",
        &bearer(token(&account, 0)),
        publish(&account, &key),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, _) = call(
        &app,
        "POST",
        "/discovery/identity",
        &bearer(token(&account, 0)),
        publish(&other, &key),
    )
    .await;
    assert!(status.is_client_error());
    let (status, _) = call(
        &app,
        "POST",
        "/discovery/identity",
        &bearer(token(&account, 0)),
        publish(&account, &SigningKey::generate()),
    )
    .await;
    assert!(status.is_client_error());

    // KeyPackage: one accepted, one naming another device than the token.
    let kp = |acct: &[u8; 16], device: u8| {
        serde_json::json!({
            "account_id": B64.encode(acct),
            "device_id": B64.encode([device; 32]),
            "keypackage": B64.encode([device; 64]),
        })
    };
    let (status, body) = call(
        &app,
        "POST",
        "/discovery/keypackage",
        &bearer(token(&account, 9)),
        kp(&account, 9),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
    let _ = call(
        &app,
        "POST",
        "/discovery/keypackage",
        &bearer(token(&account, 9)),
        kp(&account, 10),
    )
    .await;

    // The three invite ledgers: upload, consume, revoke.
    for (path, kind) in [
        ("invite", lakko_contacts::link_seal::LinkKind::Contact),
        ("call-invite", lakko_contacts::link_seal::LinkKind::Call),
        ("group-invite", lakko_contacts::link_seal::LinkKind::Group),
    ] {
        let revocation = SigningKey::generate();
        let sealed = lakko_contacts::link_seal::seal(
            kind,
            &lakko_contacts::link_seal::generate_link_secret(),
            b"capability",
        )
        .unwrap();
        let (status, body) = call(
            &app,
            "POST",
            &format!("/discovery/{path}"),
            &bearer(token(&account, 9)),
            serde_json::json!({
                "sealed": B64.encode(&sealed),
                "revocation_pubkey": B64.encode(revocation.verifying_key().to_bytes()),
                "expiry_ms": u64::try_from(now_ms()).unwrap() + 3_600_000,
                "max_uses": 5,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let blob_id: String = serde_json::from_str::<serde_json::Value>(&body).unwrap()["blob_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, body) = call(
            &app,
            "POST",
            &format!("/discovery/{path}/{blob_id}/consume"),
            &[(
                TOKEN_HEADER,
                mint_header(&master, TokenContext::InviteConsume, epoch),
            )],
            serde_json::json!({ "joiner_tag": B64.encode([0x42u8; 32]) }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path} consume: {body}");
        let id: [u8; 6] = B64URL.decode(&blob_id).unwrap().try_into().unwrap();
        let signed_at = u64::try_from(now_ms()).unwrap();
        let signature = match path {
            "invite" => lakko_contacts::invite::sign_revocation(&revocation, &id, signed_at),
            "call-invite" => {
                lakko_contacts::call_invite::call_sign_revocation(&revocation, &id, signed_at)
            },
            _ => lakko_contacts::group_invite::group_sign_revocation(&revocation, &id, signed_at),
        };
        let (status, body) = call(
            &app,
            "DELETE",
            &format!("/discovery/{path}/{blob_id}"),
            &bearer(token(&account, 9)),
            serde_json::json!({ "signed_at_ms": signed_at, "signature": B64.encode(signature) }),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{path} revoke: {body}");
    }

    assert!(
        capture.output().contains("invite stored"),
        "the capture saw the handlers' log lines:\n{}",
        capture.output()
    );
    capture.assert_no_identifiers(ALLOW);
    let text = metrics.render();
    lakko_metrics::exposition::assert_clean(&text, &["kind", "context"], ALLOW);
    assert!(
        text.contains(r#"lakko_http_requests_total{service="discovery",route="/"#),
        "the routes driven above are counted:\n{text}"
    );
    // The three identity uploads above, by how the change rule decided, and
    // the refusal's code on the RED counter (ADR-0096 P2).
    for outcome in ["changed", "invalid", "identity_change_needs_attestation"] {
        let line = format!(r#"lakko_discovery_identity_uploads_total{{outcome="{outcome}"}}"#);
        assert!(text.contains(&line), "missing {line}:\n{text}");
    }
    assert!(
        text.contains(r#"code="identity-change-needs-attestation""#),
        "the refusal carries its code:\n{text}"
    );
}
