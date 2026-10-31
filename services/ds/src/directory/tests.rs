// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The directory routes against a real database, with the real writer on one
//! side and the real reader on the other. Gated on
//! `LAKKO_DS_TEST_DATABASE_URL`, like the other route tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use axum::http::StatusCode;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_anon_token::{TokenContext, TOKEN_HEADER};
use lakko_circle::{
    directory_path, member_epoch_key, open_filter, open_row, path_nodes, row_epoch, DirectoryHead,
    DirectoryWrite, DirectoryWriter, EpochKey, FilterEntry, MemberGrant, Wrap, HEAD_LEN, ROW_LEN,
};
use lakko_ds_proof::{SigningKey, PROOF_HEADER_NAME};

use crate::test_support::{app, fresh_name, send, send_with, token_at, Req};

fn writer(tag: u8) -> DirectoryWriter {
    DirectoryWriter::create(&fresh_name(tag)).unwrap()
}

fn owner_key(writer: &DirectoryWriter) -> SigningKey {
    SigningKey::from_seed(writer.owner_signing_seed().as_bytes())
}

fn read_key(key: &EpochKey) -> SigningKey {
    SigningKey::from_seed(key.read_seed().as_bytes())
}

fn claim_token() -> String {
    token_at(TokenContext::CircleClaim, 0)
}

async fn send_write(
    app: &Router,
    write: &DirectoryWrite,
    owner: &SigningKey,
    token: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let uri = write.path();
    let headers: Vec<(&str, &str)> = token.map(|t| vec![(TOKEN_HEADER, t)]).unwrap_or_default();
    send_with(
        app,
        Req {
            method: write.method(),
            uri: &uri,
            body: write.body_json().into_bytes(),
            proof: Some((owner, lakko_circle::OWNER_PROOF_EPOCH)),
        },
        &headers,
    )
    .await
}

/// Claim a new directory.
async fn claimed(app: &Router, tag: u8) -> (DirectoryWriter, SigningKey) {
    let mut w = writer(tag);
    let owner = owner_key(&w);
    let claim = w.take_write().unwrap().unwrap();
    let token = claim_token();
    let (status, _) = send_write(app, &claim, &owner, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    (w, owner)
}

async fn flush(app: &Router, w: &mut DirectoryWriter, owner: &SigningKey) {
    while let Some(write) = w.take_write().unwrap() {
        let (status, body) = send_write(app, &write, owner, None).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
}

struct Header {
    head: DirectoryHead,
    nodes: Vec<(u32, Wrap)>,
    chain: Vec<(u64, Wrap)>,
}

async fn read_header(app: &Router, w: &DirectoryWriter, query: &str) -> Header {
    let uri = format!("{}/header{query}", directory_path(w.directory_id()));
    let (status, body) = send(
        app,
        Req {
            method: "GET",
            uri: &uri,
            body: vec![],
            proof: None,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let head = B64.decode(json["head_b64"].as_str().unwrap()).unwrap();
    let wraps = |field: &str| -> Vec<(u64, Wrap)> {
        json[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                let wrap = B64.decode(item[1].as_str().unwrap()).unwrap();
                (item[0].as_u64().unwrap(), wrap.try_into().unwrap())
            })
            .collect()
    };
    Header {
        head: DirectoryHead::decode(&head).unwrap(),
        nodes: wraps("nodes")
            .into_iter()
            .map(|(n, w)| (u32::try_from(n).unwrap(), w))
            .collect(),
        chain: wraps("chain"),
    }
}

/// What a member does: the head to learn the capacity, then its path.
async fn member_key(
    app: &Router,
    w: &DirectoryWriter,
    grant: &MemberGrant,
) -> Result<EpochKey, lakko_circle::CircleError> {
    let first = read_header(app, w, "").await;
    let path = path_nodes(grant.leaf, first.head.capacity_log).unwrap();
    let list: Vec<String> = path.iter().map(u32::to_string).collect();
    let header = read_header(app, w, &format!("?nodes={}", list.join(","))).await;
    member_epoch_key(grant, &header.head, &header.nodes)
}

async fn read_with(
    app: &Router,
    uri: &str,
    key: Option<&SigningKey>,
    epoch: u64,
) -> (StatusCode, Vec<u8>) {
    send(
        app,
        Req {
            method: "GET",
            uri,
            body: vec![],
            proof: key.map(|k| (k, epoch)),
        },
    )
    .await
}

fn filter_uri(w: &DirectoryWriter) -> String {
    format!("{}/filter", directory_path(w.directory_id()))
}

fn row_uri(w: &DirectoryWriter, index: u32) -> String {
    format!("{}/rows/{index}", directory_path(w.directory_id()))
}

fn entry(term: u8, row: u32) -> FilterEntry {
    FilterEntry {
        tag: [term; 32],
        row,
    }
}

#[tokio::test]
async fn a_claim_needs_the_owner_and_a_token() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let mut w = writer(0x91);
    let owner = owner_key(&w);
    let claim = w.take_write().unwrap().unwrap();

    // No token: refused, nothing claimed.
    let (status, body) = send_write(&app, &claim, &owner, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(String::from_utf8_lossy(&body).contains("anon-token-required"));
    // Another key: the uniform refusal, and no token spent.
    let token = claim_token();
    let (status, _) =
        send_write(&app, &claim, &SigningKey::from_seed(&[9; 32]), Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The same token still spends: the refusal above took nothing.
    let (status, _) = send_write(&app, &claim, &owner, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The same claim again answers as the first, for free.
    let (status, _) = send_write(&app, &claim, &owner, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A claim under an id that is not the owner's name.
    let other = writer(0x92);
    let stolen_uri = format!("{}/claim", directory_path(other.directory_id()));
    let (status, _) = send(
        &app,
        Req {
            method: "PUT",
            uri: &stolen_uri,
            body: claim.body_json().into_bytes(),
            proof: Some((&owner, lakko_circle::OWNER_PROOF_EPOCH)),
        },
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reads_need_the_current_key_and_a_removal_ends_the_old_one_at_once() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x93).await;
    let bob = w.add_member().unwrap();
    let carol = w.add_member().unwrap();
    w.put_row(0, b"anna".to_vec()).unwrap();
    w.set_filter(vec![entry(1, 0)], 1).unwrap();
    flush(&app, &mut w, &owner).await;

    let key = member_key(&app, &w, &bob).await.unwrap();
    assert_eq!(key.epoch(), 1);
    let signer = read_key(&key);

    // No proof, another key: the same refusal.
    let filter = filter_uri(&w);
    let (status, none) = read_with(&app, &filter, None, 1).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, other) = read_with(&app, &filter, Some(&SigningKey::from_seed(&[5; 32])), 1).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(none, other);

    // The filter and the row under the read key.
    let (status, wire) = read_with(&app, &filter, Some(&signer), 1).await;
    assert_eq!(status, StatusCode::OK);
    let rows = open_filter(&key, &wire).unwrap().lookup(&[1; 32]);
    assert_eq!(rows, vec![0]);
    let (status, row) = read_with(&app, &row_uri(&w, 0), Some(&signer), 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row.len(), ROW_LEN);
    assert_eq!(open_row(&key, 0, &row).unwrap(), b"anna");

    // Katya removes Bob. His key is refused from that moment.
    w.remove_members(&[bob.leaf]).unwrap();
    flush(&app, &mut w, &owner).await;
    let (status, _) = read_with(&app, &filter, Some(&signer), 1).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = read_with(&app, &row_uri(&w, 0), Some(&signer), 1).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Naming the new epoch does not help him.
    let (status, _) = read_with(&app, &filter, Some(&signer), 2).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // He reaches no new key from the header.
    assert_eq!(
        member_key(&app, &w, &bob).await.map(|_| ()),
        Err(lakko_circle::CircleError::HeaderMismatch)
    );

    // Carol does, reads under it, and opens the older filter through the chain.
    let current = member_key(&app, &w, &carol).await.unwrap();
    assert_eq!(current.epoch(), 2);
    let (status, wire) = read_with(&app, &filter, Some(&read_key(&current)), 2).await;
    assert_eq!(status, StatusCode::OK);
    let header = read_header(&app, &w, "?chain_from=1").await;
    assert_eq!(header.chain.len(), 1);
    assert_eq!(header.chain[0].0, 2);
    let older = current.previous(&header.chain[0].1).unwrap();
    assert_eq!(
        open_filter(&older, &wire).unwrap().lookup(&[1; 32]),
        vec![0]
    );
}

#[tokio::test]
async fn a_repeated_proof_is_refused() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x94).await;
    let bob = w.add_member().unwrap();
    w.put_row(0, b"x".to_vec()).unwrap();
    w.set_filter(vec![], 1).unwrap();
    flush(&app, &mut w, &owner).await;
    let key = member_key(&app, &w, &bob).await.unwrap();
    let signer = read_key(&key);

    let uri = filter_uri(&w);
    let header = crate::test_support::proof_header(&Req {
        method: "GET",
        uri: &uri,
        body: vec![],
        proof: Some((&signer, 1)),
    })
    .unwrap();
    let extra = [(PROOF_HEADER_NAME, header.as_str())];
    for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
        let (status, _) = send_with(
            &app,
            Req {
                method: "GET",
                uri: &uri,
                body: vec![],
                proof: None,
            },
            &extra,
        )
        .await;
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn a_write_is_the_owners_and_names_the_next_version() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x95).await;

    w.put_row(0, b"one".to_vec()).unwrap();
    let first = w.take_write().unwrap().unwrap();

    // Not the owner: the uniform refusal.
    let (status, _) = send_write(&app, &first, &SigningKey::from_seed(&[6; 32]), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The owner, once; the same bytes again answer the same and apply nothing.
    let (status, _) = send_write(&app, &first, &owner, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send_write(&app, &first, &owner, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Other bytes at that version, as a second device would build: refused.
    let mut stale = first.clone();
    stale.rows.clear();
    let (status, _) = send_write(&app, &stale, &owner, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A version that skips ahead: refused.
    w.put_row(1, b"two".to_vec()).unwrap();
    let mut second = w.take_write().unwrap().unwrap();
    second.version += 1;
    let (status, _) = send_write(&app, &second, &owner, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_device_behind_a_removal_cannot_write_under_the_old_epoch() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let seed = fresh_name(0x96);
    let mut w = DirectoryWriter::create(&seed).unwrap();
    let owner = owner_key(&w);
    let claim = w.take_write().unwrap().unwrap();
    let token = claim_token();
    assert_eq!(
        send_write(&app, &claim, &owner, Some(&token)).await.0,
        StatusCode::NO_CONTENT
    );
    let bob = w.add_member().unwrap();
    let state = w.encode_state();

    // This device removes Bob.
    w.remove_members(&[bob.leaf]).unwrap();
    flush(&app, &mut w, &owner).await;
    assert_eq!(w.epoch(), 2);

    // The other device still thinks Bob is in, and queues a row at epoch 1.
    let mut behind = DirectoryWriter::restore(&seed, &state).unwrap();
    assert_eq!(behind.epoch(), 1);
    behind.put_row(0, b"late".to_vec()).unwrap();
    let write = behind.take_write().unwrap().unwrap();
    assert_eq!(write.epoch, 1);
    let (status, _) = send_write(&app, &write, &owner, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = read_with(&app, &row_uri(&w, 0), Some(&read_key(&w.epoch_key())), 2).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_new_epoch_needs_its_read_key_and_chain_and_arrives_whole() {
    let Some((app, pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x97).await;
    let bob = w.add_member().unwrap();
    w.remove_members(&[bob.leaf]).unwrap();
    let write = w.take_write().unwrap().unwrap();

    let mut no_chain = write.clone();
    no_chain.chain = None;
    let (status, _) = send_write(&app, &no_chain, &owner, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A write the transaction refuses leaves the epoch where it was. Row
    // bytes of the wrong length are refused before it starts, so nothing of
    // this write landed.
    let mut bad_row = write.clone();
    bad_row.rows.push((0, vec![0; 3]));
    let (status, _) = send_write(&app, &bad_row, &owner, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let header = read_header(&app, &w, "").await;
    assert_eq!(header.head.epoch, 1);

    let (status, _) = send_write(&app, &write, &owner, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let header = read_header(&app, &w, "").await;
    assert_eq!(header.head.epoch, 2);
    let id = *w.directory_id();
    let (stored,): (u64,) = sqlx::query_as("SELECT epoch FROM ds_directory WHERE directory_id = ?")
        .bind(&id[..])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 2);
}

#[tokio::test]
async fn the_header_is_public_and_bounded() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x98).await;
    let bob = w.add_member().unwrap();
    flush(&app, &mut w, &owner).await;
    let header = read_header(&app, &w, "").await;
    assert_eq!(header.head.version, w.version());
    assert!(header.nodes.is_empty() && header.chain.is_empty());
    assert_eq!(B64.encode([0u8; HEAD_LEN]).len(), 80);

    let base = directory_path(w.directory_id());
    let too_many: Vec<String> = (2..40).map(|n: u32| n.to_string()).collect();
    for bad in [
        format!("{base}/header?nodes={}", too_many.join(",")),
        format!("{base}/header?nodes=1"),
        format!("{base}/header?nodes=abc"),
        format!("{base}/header?nodes=999999999"),
    ] {
        let (status, _) = read_with(&app, &bad, None, 0).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let absent = directory_path(&fresh_name(0x99));
    let (status, _) = read_with(&app, &format!("{absent}/header"), None, 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A gated read of a directory that does not exist is the uniform refusal.
    let (status, _) = read_with(&app, &format!("{absent}/filter"), Some(&owner), 1).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = bob;
}

#[tokio::test]
async fn a_row_can_be_replaced_and_deleted() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    let (mut w, owner) = claimed(&app, 0x9A).await;
    let bob = w.add_member().unwrap();
    w.put_row(0, b"first".to_vec()).unwrap();
    w.put_row(1, b"second".to_vec()).unwrap();
    flush(&app, &mut w, &owner).await;
    w.put_row(0, b"changed".to_vec()).unwrap();
    w.delete_row(1).unwrap();
    flush(&app, &mut w, &owner).await;

    let key = member_key(&app, &w, &bob).await.unwrap();
    let signer = read_key(&key);
    let (status, row) = read_with(&app, &row_uri(&w, 0), Some(&signer), 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row_epoch(&row).unwrap(), 1);
    assert_eq!(open_row(&key, 0, &row).unwrap(), b"changed");
    let (status, _) = read_with(&app, &row_uri(&w, 1), Some(&signer), 1).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_growth_write_past_the_journal_body_limit_lands() {
    let Some((app, _pool)) = app().await else {
        return;
    };
    // Growing to 2^15 leaves writes 32,767 edges, about 3 MB of JSON: past the
    // 1 MiB the journal routes allow, inside the directory routes' limit.
    let (mut w, owner) = claimed(&app, 0x9B).await;
    let mut last = None;
    for _ in 0..=(1u32 << 14) {
        last = Some(w.add_member().unwrap());
    }
    assert_eq!(w.capacity_log(), 15);
    let write = w.take_write().unwrap().unwrap();
    assert!(write.body_json().len() > 2 * 1024 * 1024);
    let (status, _) = send_write(&app, &write, &owner, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let key = member_key(&app, &w, &last.unwrap()).await.unwrap();
    assert_eq!(key, w.epoch_key());
}

#[tokio::test]
async fn the_owner_deletes_everything_and_can_start_over() {
    let Some((app, pool)) = app().await else {
        return;
    };
    let seed = fresh_name(0x9C);
    let mut w = DirectoryWriter::create(&seed).unwrap();
    let owner = owner_key(&w);
    let claim = w.take_write().unwrap().unwrap();
    let token = claim_token();
    send_write(&app, &claim, &owner, Some(&token)).await;
    let bob = w.add_member().unwrap();
    w.put_row(0, b"x".to_vec()).unwrap();
    w.set_filter(vec![], 1).unwrap();
    w.remove_members(&[bob.leaf]).unwrap();
    flush(&app, &mut w, &owner).await;

    let base = directory_path(w.directory_id());
    let (status, _) = send(
        &app,
        Req {
            method: "DELETE",
            uri: &base,
            body: vec![],
            proof: Some((&SigningKey::from_seed(&[7; 32]), 0)),
        },
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(
        &app,
        Req {
            method: "DELETE",
            uri: &base,
            body: vec![],
            proof: Some((&owner, 0)),
        },
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let id = *w.directory_id();
    for query in [
        "SELECT COUNT(*) FROM ds_directory WHERE directory_id = ?",
        "SELECT COUNT(*) FROM ds_directory_edges WHERE directory_id = ?",
        "SELECT COUNT(*) FROM ds_directory_chain WHERE directory_id = ?",
        "SELECT COUNT(*) FROM ds_directory_filter WHERE directory_id = ?",
        "SELECT COUNT(*) FROM ds_directory_rows WHERE directory_id = ?",
    ] {
        let (left,): (i64,) = sqlx::query_as(query)
            .bind(&id[..])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "{query}");
    }

    // The same seed starts a new tree under the same id, and the old leaf key
    // opens nothing in it.
    let mut again = DirectoryWriter::create(&seed).unwrap();
    assert_eq!(again.directory_id(), w.directory_id());
    let claim = again.take_write().unwrap().unwrap();
    let token = claim_token();
    let (status, _) = send_write(&app, &claim, &owner, Some(&token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(member_key(&app, &again, &bob).await.is_err());
}
