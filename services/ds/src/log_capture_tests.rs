// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0078 section 11, decision 5 half b: drive the Delivery Service's
//! journal and mailbox routes through the real router under a capture
//! subscriber, and fail if any line at info level or above carries a value
//! shaped like a 16- or 32-byte identifier (a journal id, a mailbox address,
//! a key).
//!
//! Gated on `LAKKO_DS_TEST_DATABASE_URL` like the other route tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

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

    // A circle directory: the claim, a read under the current key, and the
    // refusal of a read under no key.
    let mut directory = lakko_circle::DirectoryWriter::create(&fresh_name(0x65)).unwrap();
    let owner = lakko_ds_proof::SigningKey::from_seed(directory.owner_signing_seed().as_bytes());
    directory
        .put_row(0, lakko_circle::seal_card(b"card", &[[0x4c; 64]]).unwrap())
        .unwrap();
    directory.set_filter(vec![], 1).unwrap();
    let first_write = directory.take_write().unwrap().unwrap();
    let claim_uri = first_write.path();
    let claim_token = token_at(TokenContext::CircleClaim, 0);
    let (status, _) = send_with(
        &app,
        Req {
            method: first_write.method(),
            uri: &claim_uri,
            body: first_write.body_json().into_bytes(),
            proof: Some((&owner, lakko_circle::OWNER_PROOF_EPOCH)),
        },
        &[(lakko_anon_token::TOKEN_HEADER, claim_token.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let read = lakko_ds_proof::SigningKey::from_seed(directory.epoch_key().read_seed().as_bytes());
    let row_uri = format!(
        "{}/rows/0",
        lakko_circle::directory_path(directory.directory_id())
    );
    for (proof, expected) in [
        (Some((&read, 1)), StatusCode::OK),
        (None, StatusCode::UNAUTHORIZED),
    ] {
        let (status, _) = send(
            &app,
            Req {
                method: "GET",
                uri: &row_uri,
                body: vec![],
                proof,
            },
        )
        .await;
        assert_eq!(status, expected);
    }

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
