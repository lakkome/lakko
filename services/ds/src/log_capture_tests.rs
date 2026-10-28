// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0078 section 11, decision 5 half b: drive the Delivery Service's
//! journal and mailbox routes through the real router under a capture
//! subscriber, and fail if any line at info level or above carries a value
//! shaped like a 16- or 32-byte identifier (a journal id, a mailbox address,
//! a key).
//!
//! Gated on `LAKKO_DS_TEST_DATABASE_URL` like the other route tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::http::StatusCode;
use lakko_anon_token::TokenContext;
use lakko_log_hygiene::capture::Capture;

use crate::test_support::{
    app, claim, fresh_name, fresh_signer, key, knock, knock_token, mailbox_row, messages_uri, send,
    send_with, token_at, Req,
};

/// Values allowed in the captured output although they have an identifier's
/// shape. Reviewed like code; empty today.
const ALLOW: &[&str] = &[];

#[tokio::test]
async fn no_identifier_reaches_the_log_on_journal_and_mailbox_routes() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (capture, _guard) = Capture::install();

    // A claimed group: publish under the epoch key, read back, and the two
    // refusals (no proof, another key).
    let gid = fresh_name(0x61);
    let root = fresh_signer(0x62);
    let epoch_key = key(0x63);
    assert!(claim(&app, &gid, &root, 1, &epoch_key).await.is_success());
    let uri = messages_uri("group", &gid);
    let (status, _) = send(
        &app,
        Req {
            method: "POST",
            uri: &uri,
            body: b"ciphertext".to_vec(),
            proof: Some((&epoch_key, 1)),
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let get = format!("{uri}?after=0");
    let (status, _) = send(
        &app,
        Req {
            method: "GET",
            uri: &get,
            body: vec![],
            proof: Some((&epoch_key, 1)),
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for proof in [None, Some((&key(0x64), 1))] {
        let (status, _) = send(
            &app,
            Req {
                method: "POST",
                uri: &uri,
                body: b"ciphertext".to_vec(),
                proof,
            },
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // A knock on the claimed group.
    let (status, _) = knock(
        &app,
        &format!("/ds/group/{}/knock", hex::encode(gid)),
        &[b"proposal"],
        Some(&knock_token()),
    )
    .await;
    assert!(status.is_success(), "{status}");

    // A proofless write to an unclaimed mailbox, spending a token.
    let mailbox = fresh_name(0x65);
    let token = token_at(TokenContext::MailboxWrite, 0);
    let (status, _) = send_with(
        &app,
        Req {
            method: "POST",
            uri: &messages_uri("mailbox", &mailbox),
            body: mailbox_row(b"sealed"),
            proof: None,
        },
        &[(lakko_anon_token::TOKEN_HEADER, &token)],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    capture.assert_no_identifiers(ALLOW);
}
