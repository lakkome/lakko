// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0065 section 5 - the attested arm of `PUT /ds/group/{hex}/reset`.
//!
//! A journal's root key hangs on the owner's account seed (ADR-0057), so
//! after an account resets its keys the device that holds the NEW seed is a
//! stranger to the DeviceGroup journal the OLD seed claimed, and a stranger
//! is heard nowhere. The reset route therefore takes an optional second body
//! field, `key_reset_attest_b64`: the 177-byte [`KeyResetAttestation`]
//! `services/auth` minted inside the one transaction that moved the account
//! to its new key generation. When it is present and
//!
//! 1. verifies under the attest key pinned from [`ATTEST_KEY_ENV`] (unset:
//!    `503 key-reset-disabled`, on this arm only),
//! 2. names the journal in the path (else the uniform `401`: an attestation
//!    for one journal is useless on any other, a group journal the account
//!    owns included - those are not re-rooted here, ADR-0065 phase 2),
//! 3. carries a generation strictly greater than the journal's stored
//!    `ds_group_owner.key_generation` (else `409`),
//!
//! the Reset statement is heard from ANY signer: the signer becomes the root,
//! the epoch chain, the registrar set and the push binding are dropped, the
//! statement's epoch key and optional registrars are written, and the
//! generation is recorded. The nonce is spent only after 1-3 pass, so a
//! stranger without an attestation still learns the uniform `401` at no cost
//! to the server. Without the field the route behaves exactly as ADR-0057
//! wrote it.
//!
//! # The root endorsement (ADR-0066 section 4.6, decision D11)
//!
//! "From ANY signer" was a race. The attestation is not a secret: the
//! resetting device hands it to discovery before it comes here, and ADR-0065
//! R.3 has discovery serve it to anyone who fetches the bundle. Between
//! those two steps, and for as long as an owed retry lasts, anyone holding a
//! copy could present it and become the root of that account's DeviceGroup
//! journal. So the body must also carry `root_endorsement_b64`: a 64-byte
//! Ed25519 signature by the attestation's `new_signature_pubkey` (the
//! account's NEW identity key, which only the resetting device holds) over
//! `"lakko/ds/root-endorsement/v1" || 0x00 || journal_id || key_generation ||
//! root_key`, where `root_key` is the key that becomes the root. The DS
//! verifies it with the key inside the attestation it has just verified
//! (`lakko_attest::verify_root_endorsement`); a missing or failing
//! endorsement is the uniform `401`, before the statement's signature is
//! checked and before any nonce is spent. A copied attestation is then
//! useless to everyone but the new identity.
//!
//! # The attested root transfer (a key rotation)
//!
//! `PUT /ds/group/{hex}/root` takes the same two fields. A key ROTATION
//! (ADR-0066 section 3.5) does not reset the journal: the old root, which the
//! rotating device still holds, signs a `RootTransfer` naming the root the
//! new seed derives, so the epoch chain stays and a kept device that lags an
//! epoch is not cut off. Carried with the attestation and an endorsement of
//! the NEW root, the transfer also records the generation, under the same
//! strictly-greater rule, so the attestation cannot be presented afterwards
//! on either route. It is heard from the current root only: the attestation
//! adds the generation, it does not widen who may transfer.
//!
//! What the DS checks and does not learn: the attestation names the account
//! by a one-way `account_ref` the DS only ever carries, never relates to an
//! account (invariant #5); the DS compares the journal id and the generation
//! and nothing else about it. A `409` on a retry after a crash means the
//! reset already landed at this generation; the client confirms with the
//! `PUT .../root` probe naming itself (`204`).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attest::{KeyResetAttestation, KEY_RESET_ATTESTATION_LEN, ROOT_ENDORSEMENT_LEN};
use lakko_ds_proof::statement::KEY_LEN;
use lakko_ds_proof::{RegistrarEntry, VerifyingKey, ROOT_RANK_BYTE};
use sqlx::mysql::MySqlConnection;

use crate::access::{refused, registration_db_error, HandlerError};
use crate::journal::PSEUDONYM_LEN;

/// The environment variable the auth attest verifying key is pinned from.
pub(crate) const ATTEST_KEY_ENV: &str = "LAKKO_DS_ATTEST_KEY";

/// Read `LAKKO_DS_ATTEST_KEY`: base64 of a 32-byte Ed25519 verifying key, or
/// unset / blank.
///
/// # Errors
///
/// A message naming the variable when it is set but not a valid key: a
/// malformed pin refuses startup rather than silently refusing every reset.
pub(crate) fn attest_key_from_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<VerifyingKey>, String> {
    let Some(raw) = lookup(ATTEST_KEY_ENV).map(|v| v.trim().to_owned()) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|_| format!("{ATTEST_KEY_ENV} is not valid base64"))?;
    VerifyingKey::from_bytes(&bytes)
        .map(Some)
        .map_err(|_| format!("{ATTEST_KEY_ENV} is not a valid 32-byte Ed25519 public key"))
}

/// The `503` the attested arm answers when no attest key is pinned.
pub(crate) fn disabled() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "key-reset-disabled" })),
    )
        .into_response()
}

/// Decode and verify the `key_reset_attest_b64` body field against the
/// pinned key and the journal in the path: shape first (`400`), then the
/// signature and the journal binding (the uniform `401`).
///
/// # Errors
///
/// `400` for bad base64 or a wrong length; `401` for anything the pinned
/// key does not vouch for or that names another journal.
pub(crate) fn verify_for_journal(
    field: &str,
    attest_key: &VerifyingKey,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<KeyResetAttestation, HandlerError> {
    let bytes = B64.decode(field.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "key_reset_attest_b64: bad base64".to_string(),
        )
    })?;
    if bytes.len() != KEY_RESET_ATTESTATION_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("key_reset_attest_b64 must decode to {KEY_RESET_ATTESTATION_LEN} bytes"),
        ));
    }
    let attestation = KeyResetAttestation::open(&bytes, attest_key).map_err(|_| refused())?;
    if attestation.device_group_journal_id != *group_id {
        return Err(refused());
    }
    Ok(attestation)
}

/// Check the root endorsement that must accompany an attestation
/// (ADR-0066 D11): the attested identity key signing `becomes_root`, the key
/// this statement makes the journal's root. Shape first (`400`), then the
/// signature (the uniform `401`); an absent field is the uniform `401`.
///
/// # Errors
///
/// `400` for bad base64 or a wrong length; `401` for an absent endorsement
/// or one that does not verify.
pub(crate) fn verify_endorsement(
    field: Option<&str>,
    attestation: &KeyResetAttestation,
    becomes_root: &[u8; KEY_LEN],
) -> Result<(), HandlerError> {
    let Some(field) = field else {
        return Err(refused());
    };
    let bytes = B64.decode(field.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "root_endorsement_b64: bad base64".to_string(),
        )
    })?;
    if bytes.len() != ROOT_ENDORSEMENT_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("root_endorsement_b64 must decode to {ROOT_ENDORSEMENT_LEN} bytes"),
        ));
    }
    lakko_attest::verify_root_endorsement(attestation, becomes_root, &bytes).map_err(|_| refused())
}

/// The journal's stored generation: `0` for an unclaimed journal and for
/// one never re-rooted by an attested reset.
pub(crate) async fn stored_generation(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
) -> Result<u64, HandlerError> {
    let row: Option<(u64,)> =
        sqlx::query_as("SELECT key_generation FROM ds_group_owner WHERE group_id = ?")
            .bind(&group_id[..])
            .fetch_optional(&mut *conn)
            .await
            .map_err(registration_db_error("load journal key generation"))?;
    Ok(row.map_or(0, |(g,)| g))
}

/// Pure half of the generation rule: an attested reset is heard only above
/// the stored generation.
pub(crate) const fn generation_is_newer(attested: u64, stored: u64) -> bool {
    attested > stored
}

/// Re-root `group_id` under `signer` at `generation`: the owner row is
/// written (or claimed, for a journal nobody claimed), the epoch chain, the
/// registrar set and the push binding are dropped, and the statement's epoch
/// key and optional registrars are written as a root reset writes them.
///
/// # Errors
///
/// `400` when the new root lists itself as a registrar; `500` on a database
/// failure.
pub(crate) async fn apply_attested_reset(
    conn: &mut MySqlConnection,
    group_id: &[u8; PSEUDONYM_LEN],
    signer: &[u8; KEY_LEN],
    generation: u64,
    epoch: u64,
    epoch_key: &[u8; KEY_LEN],
    registrars: Option<&[RegistrarEntry]>,
) -> Result<Result<(), HandlerError>, HandlerError> {
    if registrars.is_some_and(|set| set.iter().any(|e| e.key == *signer)) {
        return Ok(Err((
            StatusCode::BAD_REQUEST,
            "the root cannot be its own registrar".to_string(),
        )));
    }
    sqlx::query(
        "INSERT INTO ds_group_owner (group_id, root_pk, key_generation) VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE root_pk = VALUES(root_pk), \
            key_generation = VALUES(key_generation), updated_at = CURRENT_TIMESTAMP(3)",
    )
    .bind(&group_id[..])
    .bind(&signer[..])
    .bind(generation)
    .execute(&mut *conn)
    .await
    .map_err(registration_db_error("re-root journal"))?;
    sqlx::query("DELETE FROM ds_group_access WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("reset epoch chain"))?;
    sqlx::query("DELETE FROM ds_group_registrars WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("reset registrars"))?;
    sqlx::query("DELETE FROM ds_push_bindings WHERE group_id = ?")
        .bind(&group_id[..])
        .execute(&mut *conn)
        .await
        .map_err(registration_db_error("drop push binding"))?;
    crate::ownership::insert_epoch(conn, group_id, epoch, epoch_key, signer, ROOT_RANK_BYTE)
        .await?;
    if let Some(set) = registrars {
        for entry in set {
            crate::ownership::upsert_registrar(conn, group_id, &entry.key, entry.rank).await?;
        }
    }
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_ds_proof::SigningKey;

    fn attest_key() -> SigningKey {
        SigningKey::from_seed(&[0x41; 32])
    }

    #[test]
    fn the_attest_key_is_base64_or_unset() {
        let env = |v: Option<String>| move |_: &str| v.clone();
        assert!(attest_key_from_lookup(env(None)).unwrap().is_none());
        assert!(attest_key_from_lookup(env(Some("  ".into())))
            .unwrap()
            .is_none());
        let pinned = B64.encode(attest_key().verifying_key().to_bytes());
        assert_eq!(
            attest_key_from_lookup(env(Some(pinned))).unwrap(),
            Some(attest_key().verifying_key())
        );
        assert!(attest_key_from_lookup(env(Some("not base64!".into()))).is_err());
        assert!(attest_key_from_lookup(env(Some(B64.encode([1u8; 16])))).is_err());
    }

    #[test]
    fn generation_rule_is_strictly_greater() {
        assert!(generation_is_newer(1, 0));
        assert!(generation_is_newer(3, 2));
        assert!(!generation_is_newer(2, 2));
        assert!(!generation_is_newer(1, 2));
        assert!(!generation_is_newer(0, 0));
    }

    #[test]
    fn verify_for_journal_checks_shape_then_key_then_journal() {
        let account = [0xA7u8; 16];
        let gid = lakko_attest::device_group_journal_id(&account);
        let vk = attest_key().verifying_key();
        let wire =
            KeyResetAttestation::for_account(&account, 2, [0x5A; 32], 1_000).sign(&attest_key());
        let field = B64.encode(wire);
        let opened = verify_for_journal(&field, &vk, &gid).unwrap();
        assert_eq!(opened.key_generation, 2);
        assert_eq!(
            verify_for_journal("!!", &vk, &gid).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            verify_for_journal(&B64.encode([0u8; 176]), &vk, &gid)
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        let other_key = SigningKey::from_seed(&[0x42; 32]).verifying_key();
        assert_eq!(
            verify_for_journal(&field, &other_key, &gid).unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        let other_journal = lakko_attest::device_group_journal_id(&[0xA8u8; 16]);
        assert_eq!(
            verify_for_journal(&field, &vk, &other_journal)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

/// Route tests over the shared `LAKKO_DS_TEST_DATABASE_URL`-gated harness.
#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::test_support::*;
    use axum::Router;
    use lakko_ds_proof::statement::{StatementBody, StatementSigner};
    use lakko_ds_proof::{Rank, SignedStatement, SigningKey};
    use sqlx::MySqlPool;

    /// A fresh account, its DeviceGroup journal id, and the roots the old and
    /// the new seed derive for it.
    struct Account {
        id: [u8; 16],
        gid: [u8; PSEUDONYM_LEN],
        old_root: StatementSigner,
        new_root: StatementSigner,
    }

    fn account(tag: u8) -> Account {
        let name = fresh_name(tag);
        let mut id = [0u8; 16];
        id.copy_from_slice(&name[..16]);
        let gid = lakko_attest::device_group_journal_id(&id);
        Account {
            id,
            gid,
            old_root: StatementSigner::root(&fresh_name(tag), &gid),
            new_root: StatementSigner::root(&fresh_name(tag), &gid),
        }
    }

    /// The account's NEW identity key in these tests: the key every
    /// attestation names and every honest endorsement is signed with.
    fn identity() -> SigningKey {
        SigningKey::from_seed(&[0x5A; 32])
    }

    fn attestation_for(id: &[u8; 16], generation: u64) -> String {
        B64.encode(
            KeyResetAttestation::for_account(
                id,
                generation,
                identity().verifying_key().to_bytes(),
                1_000,
            )
            .sign(&TEST_ATTEST_KEY),
        )
    }

    /// The key a statement makes the journal's root: the `new_root` of a
    /// transfer, the signer of anything else.
    fn becomes_root(statement: &SignedStatement) -> [u8; KEY_LEN] {
        match &statement.body {
            StatementBody::RootTransfer { new_root } => *new_root,
            _ => statement.signer,
        }
    }

    /// An endorsement of `root` by `by`, for the journal and generation the
    /// attestation `attest_b64` names (read from its layout, so that a test
    /// can endorse an attestation the DS will refuse for another reason).
    /// An attestation of the wrong shape gets 64 zero bytes.
    fn endorsement(by: &SigningKey, attest_b64: &str, root: &[u8; KEY_LEN]) -> String {
        let bytes = B64.decode(attest_b64).unwrap_or_default();
        if bytes.len() != KEY_RESET_ATTESTATION_LEN {
            return B64.encode([0u8; 64]);
        }
        let journal: [u8; 32] = bytes[33..65].try_into().unwrap();
        let generation = u64::from_be_bytes(bytes[65..73].try_into().unwrap());
        B64.encode(lakko_attest::sign_root_endorsement(
            by, &journal, generation, root,
        ))
    }

    fn reset_body(
        signer: &StatementSigner,
        gid: &[u8; PSEUDONYM_LEN],
        epoch_key: &SigningKey,
    ) -> SignedStatement {
        signer
            .sign_at(
                gid,
                StatementBody::Reset {
                    epoch: 0,
                    epoch_key: epoch_key.verifying_key().to_bytes(),
                    registrars: None,
                },
                crate::access::now_ms(),
                fresh_nonce(),
            )
            .expect("sign reset")
    }

    /// `PUT /ds/group/{hex}/<segment>` carrying `{statement_b64,
    /// key_reset_attest_b64, root_endorsement_b64}`, the endorsement as an
    /// honest new identity makes it.
    async fn send_attested(
        app: &Router,
        gid: &[u8; PSEUDONYM_LEN],
        statement: &SignedStatement,
        attest_b64: &str,
    ) -> (StatusCode, Vec<u8>) {
        let endorsed = endorsement(&identity(), attest_b64, &becomes_root(statement));
        send_attested_with(app, gid, statement, Some(attest_b64), Some(&endorsed)).await
    }

    /// The same with the two attestation fields exactly as given.
    async fn send_attested_with(
        app: &Router,
        gid: &[u8; PSEUDONYM_LEN],
        statement: &SignedStatement,
        attest_b64: Option<&str>,
        endorsement_b64: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut body = serde_json::json!({ "statement_b64": B64.encode(statement.encode()) });
        if let Some(attest) = attest_b64 {
            body["key_reset_attest_b64"] = serde_json::json!(attest);
        }
        if let Some(endorsed) = endorsement_b64 {
            body["root_endorsement_b64"] = serde_json::json!(endorsed);
        }
        send(
            app,
            Req {
                method: "PUT",
                uri: &format!(
                    "/ds/group/{}/{}",
                    hex::encode(gid),
                    statement.path_segment()
                ),
                body: body.to_string().into_bytes(),
                proof: None,
            },
        )
        .await
    }

    fn transfer_body(
        signer: &StatementSigner,
        gid: &[u8; PSEUDONYM_LEN],
        new_root: [u8; KEY_LEN],
    ) -> SignedStatement {
        signer
            .sign_at(
                gid,
                StatementBody::RootTransfer { new_root },
                crate::access::now_ms(),
                fresh_nonce(),
            )
            .expect("sign transfer")
    }

    async fn attested_reset(
        app: &Router,
        a: &Account,
        signer: &StatementSigner,
        epoch_key: &SigningKey,
        attest_b64: &str,
    ) -> StatusCode {
        send_attested(
            app,
            &a.gid,
            &reset_body(signer, &a.gid, epoch_key),
            attest_b64,
        )
        .await
        .0
    }

    async fn read_as(
        app: &Router,
        gid: &[u8; PSEUDONYM_LEN],
        key: &SigningKey,
        epoch: u64,
    ) -> StatusCode {
        send(
            app,
            Req {
                method: "GET",
                uri: &format!("{}?after=0", messages_uri("group", gid)),
                body: vec![],
                proof: Some((key, epoch)),
            },
        )
        .await
        .0
    }

    async fn probe_root(
        app: &Router,
        gid: &[u8; PSEUDONYM_LEN],
        signer: &StatementSigner,
    ) -> StatusCode {
        send_statement(
            app,
            gid,
            signer,
            StatementBody::RootTransfer {
                new_root: signer.public_key(),
            },
        )
        .await
    }

    async fn count(pool: &MySqlPool, table: &str, gid: &[u8; PSEUDONYM_LEN]) -> i64 {
        let sql = match table {
            "access" => "SELECT COUNT(*) FROM ds_group_access WHERE group_id = ?",
            "registrars" => "SELECT COUNT(*) FROM ds_group_registrars WHERE group_id = ?",
            "bindings" => "SELECT COUNT(*) FROM ds_push_bindings WHERE group_id = ?",
            _ => unreachable!(),
        };
        let row: (i64,) = sqlx::query_as(sql)
            .bind(&gid[..])
            .fetch_one(pool)
            .await
            .unwrap();
        row.0
    }

    async fn generation(pool: &MySqlPool, gid: &[u8; PSEUDONYM_LEN]) -> u64 {
        sqlx::query_scalar("SELECT key_generation FROM ds_group_owner WHERE group_id = ?")
            .bind(&gid[..])
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The old seed claimed the DeviceGroup journal, appointed an admin and
    /// bound a push id; the new seed's root is a stranger until it carries
    /// the attestation, and the old root is a stranger from then on.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn an_attested_reset_re_roots_the_journal_a_stranger_could_not() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let a = account(0x4b);
        let old_epoch_key = key(0x91);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 40, &old_epoch_key).await,
            StatusCode::NO_CONTENT
        );
        let admin = fresh_signer(0x4b);
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::Registrars {
                    changes: vec![lakko_ds_proof::RegistrarChange {
                        key: admin.public_key(),
                        rank: Some(Rank::Admin),
                    }],
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::PushBinding {
                    push_gid: fresh_name(0x4b)
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(count(&pool, "bindings", &a.gid).await, 1);
        assert_eq!(generation(&pool, &a.gid).await, 0);

        // Without the attestation the new root is nobody to this journal.
        let new_epoch_key = key(0x92);
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.new_root,
                StatementBody::Reset {
                    epoch: 0,
                    epoch_key: new_epoch_key.verifying_key().to_bytes(),
                    registrars: None,
                },
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        // With it: re-rooted, chain + registrars + binding gone, generation
        // recorded, the old epoch key reads 401, the new one reads.
        let att2 = attestation_for(&a.id, 2);
        assert_eq!(
            attested_reset(&app, &a, &a.new_root, &new_epoch_key, &att2).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(generation(&pool, &a.gid).await, 2);
        assert_eq!(count(&pool, "access", &a.gid).await, 1);
        assert_eq!(count(&pool, "registrars", &a.gid).await, 0);
        assert_eq!(count(&pool, "bindings", &a.gid).await, 0);
        assert_eq!(
            read_as(&app, &a.gid, &old_epoch_key, 40).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            read_as(&app, &a.gid, &new_epoch_key, 0).await,
            StatusCode::OK
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );

        // The old root is a stranger now: no claim, no reset, no probe; the
        // old admin registers nothing.
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 40, &old_epoch_key).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::Reset {
                    epoch: 40,
                    epoch_key: old_epoch_key.verifying_key().to_bytes(),
                    registrars: None,
                },
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.old_root).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            register(&app, &a.gid, &admin, 1, &key(0x93), &[]).await,
            StatusCode::UNAUTHORIZED
        );

        // Replays: the same generation (by the new root itself, as a retry,
        // and by a third root) and a lower one are refused; the state stays.
        let third = StatementSigner::root(&fresh_name(0x4b), &a.gid);
        assert_eq!(
            attested_reset(&app, &a, &a.new_root, &key(0x94), &att2).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            attested_reset(&app, &a, &third, &key(0x94), &att2).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            attested_reset(&app, &a, &third, &key(0x94), &attestation_for(&a.id, 1)).await,
            StatusCode::CONFLICT
        );
        assert_eq!(generation(&pool, &a.gid).await, 2);
        assert_eq!(
            read_as(&app, &a.gid, &new_epoch_key, 0).await,
            StatusCode::OK
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );

        // The plain root reset still works for the current root, and leaves
        // the generation alone.
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.new_root,
                StatementBody::Reset {
                    epoch: 1,
                    epoch_key: key(0x95).verifying_key().to_bytes(),
                    registrars: None,
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(generation(&pool, &a.gid).await, 2);

        // A later generation re-roots again, and the previous new root is
        // the stranger this time.
        assert_eq!(
            attested_reset(&app, &a, &third, &key(0x96), &attestation_for(&a.id, 3)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(generation(&pool, &a.gid).await, 3);
        assert_eq!(
            probe_root(&app, &a.gid, &third).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// An attested reset on a journal nobody claimed claims it; the plain
    /// first claim by the old seed is then a stranger's.
    #[tokio::test]
    async fn an_attested_reset_claims_an_unclaimed_journal() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let a = account(0x4c);
        assert_eq!(
            attested_reset(
                &app,
                &a,
                &a.new_root,
                &key(0x97),
                &attestation_for(&a.id, 2)
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(generation(&pool, &a.gid).await, 2);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 0, &key(0x98)).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );
    }

    /// Shape (`400`), the pinned key and the journal binding (`401`), and
    /// the field on any other route (`400`).
    #[tokio::test]
    async fn an_attestation_must_be_well_formed_pinned_and_for_this_journal() {
        let Some((app, _pool)) = app().await else {
            return;
        };
        let a = account(0x4d);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 1, &key(0x99)).await,
            StatusCode::NO_CONTENT
        );
        let epoch_key = key(0x9a);
        let statement = reset_body(&a.new_root, &a.gid, &epoch_key);
        for (name, field, expected) in [
            ("bad base64", "!!".to_string(), StatusCode::BAD_REQUEST),
            (
                "wrong length",
                B64.encode([0u8; KEY_RESET_ATTESTATION_LEN - 1]),
                StatusCode::BAD_REQUEST,
            ),
            (
                "another journal",
                attestation_for(&[0xEE; 16], 2),
                StatusCode::UNAUTHORIZED,
            ),
            (
                "not the pinned key",
                B64.encode(
                    KeyResetAttestation::for_account(&a.id, 2, [0x5A; 32], 1_000)
                        .sign(&SigningKey::from_seed(&[0x4c; 32])),
                ),
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            let (status, _) = send_attested(&app, &a.gid, &statement, &field).await;
            assert_eq!(status, expected, "{name}");
        }
        assert_eq!(
            probe_root(&app, &a.gid, &a.old_root).await,
            StatusCode::NO_CONTENT
        );

        // The field belongs to the reset and root routes only.
        let claim_statement = a
            .new_root
            .sign_at(
                &a.gid,
                StatementBody::Claim {
                    epoch: 0,
                    epoch_key: epoch_key.verifying_key().to_bytes(),
                },
                crate::access::now_ms(),
                fresh_nonce(),
            )
            .unwrap();
        let (status, _) =
            send_attested(&app, &a.gid, &claim_statement, &attestation_for(&a.id, 2)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// No pinned key: the attested arm is 503, the plain root reset is not.
    #[tokio::test]
    async fn without_a_pinned_key_the_attested_arm_is_503_only() {
        let Some((app, _pool, _)) = app_configured_with(Some(TEST_PEPPER), None).await else {
            return;
        };
        let a = account(0x4e);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 1, &key(0x9b)).await,
            StatusCode::NO_CONTENT
        );
        let (status, body) = send_attested(
            &app,
            &a.gid,
            &reset_body(&a.new_root, &a.gid, &key(0x9c)),
            &attestation_for(&a.id, 2),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_code(&body), "key-reset-disabled");
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::Reset {
                    epoch: 2,
                    epoch_key: key(0x9d).verifying_key().to_bytes(),
                    registrars: None,
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.old_root).await,
            StatusCode::NO_CONTENT
        );
    }

    /// ADR-0066 D11: a copy of the attestation is not enough. Without the
    /// attested identity key's endorsement of the very key that signs the
    /// reset, the attested arm is the uniform refusal, costs the journal
    /// nothing, and leaves the honest reset possible afterwards.
    #[tokio::test]
    async fn an_attested_reset_without_the_endorsement_is_refused() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let a = account(0x51);
        let old_epoch_key = key(0xa1);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 7, &old_epoch_key).await,
            StatusCode::NO_CONTENT
        );
        let att = attestation_for(&a.id, 2);
        // Someone who fetched the attestation from the directory.
        let thief = StatementSigner::root(&fresh_name(0x51), &a.gid);
        let stranger_key = SigningKey::from_seed(&[0x5B; 32]);
        let honest_for_new_root = endorsement(&identity(), &att, &a.new_root.public_key());

        // No endorsement at all.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&thief, &a.gid, &key(0xa2)),
            Some(&att),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // An endorsement the thief signed for itself.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&thief, &a.gid, &key(0xa2)),
            Some(&att),
            Some(&endorsement(&stranger_key, &att, &thief.public_key())),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The honest endorsement, copied: it names the new identity's root,
        // not the thief's.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&thief, &a.gid, &key(0xa2)),
            Some(&att),
            Some(&honest_for_new_root),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // An endorsement for another generation of the same journal.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&a.new_root, &a.gid, &key(0xa2)),
            Some(&att),
            Some(&endorsement(
                &identity(),
                &attestation_for(&a.id, 3),
                &a.new_root.public_key(),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Malformed: bad base64, wrong length.
        for bad in ["!!".to_string(), B64.encode([0u8; 63])] {
            let (status, _) = send_attested_with(
                &app,
                &a.gid,
                &reset_body(&a.new_root, &a.gid, &key(0xa2)),
                Some(&att),
                Some(&bad),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        // An endorsement with no attestation beside it.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&a.old_root, &a.gid, &key(0xa2)),
            None,
            Some(&honest_for_new_root),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Nothing moved: the old root is the root, its epoch key reads, the
        // generation is unset.
        assert_eq!(generation(&pool, &a.gid).await, 0);
        assert_eq!(
            probe_root(&app, &a.gid, &a.old_root).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            read_as(&app, &a.gid, &old_epoch_key, 7).await,
            StatusCode::OK
        );
        // The honest reset still lands, with the same attestation.
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &reset_body(&a.new_root, &a.gid, &key(0xa3)),
            Some(&att),
            Some(&honest_for_new_root),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(generation(&pool, &a.gid).await, 2);
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );
    }

    /// A key rotation: the old root hands the DeviceGroup journal to the new
    /// seed's root with the attestation beside the transfer. The chain, the
    /// registrars and the binding stay (nobody is cut off), the generation is
    /// recorded, and the attestation is spent for both routes.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn an_attested_root_transfer_keeps_the_chain_and_records_the_generation() {
        let Some((app, pool)) = app().await else {
            return;
        };
        let a = account(0x52);
        let epoch_key = key(0xb1);
        assert_eq!(
            claim(&app, &a.gid, &a.old_root, 40, &epoch_key).await,
            StatusCode::NO_CONTENT
        );
        let admin = fresh_signer(0x52);
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::Registrars {
                    changes: vec![lakko_ds_proof::RegistrarChange {
                        key: admin.public_key(),
                        rank: Some(Rank::Admin),
                    }],
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.old_root,
                StatementBody::PushBinding {
                    push_gid: fresh_name(0x52)
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        let att2 = attestation_for(&a.id, 2);
        let new_root = a.new_root.public_key();

        // Heard from the current root only: the new root cannot transfer the
        // journal to itself, attestation and endorsement or not.
        let (status, _) = send_attested(
            &app,
            &a.gid,
            &transfer_body(&a.new_root, &a.gid, new_root),
            &att2,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The endorsement must name the NEW root, not the signer; and it
        // must be there.
        let transfer = transfer_body(&a.old_root, &a.gid, new_root);
        let (status, _) = send_attested_with(
            &app,
            &a.gid,
            &transfer,
            Some(&att2),
            Some(&endorsement(&identity(), &att2, &a.old_root.public_key())),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send_attested_with(&app, &a.gid, &transfer, Some(&att2), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(generation(&pool, &a.gid).await, 0);

        // The transfer.
        let (status, _) = send_attested(&app, &a.gid, &transfer, &att2).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(generation(&pool, &a.gid).await, 2);
        assert_eq!(count(&pool, "access", &a.gid).await, 1, "the chain is kept");
        assert_eq!(count(&pool, "registrars", &a.gid).await, 1);
        assert_eq!(count(&pool, "bindings", &a.gid).await, 1);
        assert_eq!(read_as(&app, &a.gid, &epoch_key, 40).await, StatusCode::OK);
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            probe_root(&app, &a.gid, &a.old_root).await,
            StatusCode::UNAUTHORIZED
        );
        // The admin the old root appointed still registers the next epoch.
        assert_eq!(
            register(&app, &a.gid, &admin, 41, &key(0xb2), &[]).await,
            StatusCode::NO_CONTENT
        );

        // The attestation is spent: it re-roots nothing afterwards, whoever
        // presents it and on whichever route.
        let third = StatementSigner::root(&fresh_name(0x52), &a.gid);
        assert_eq!(
            attested_reset(&app, &a, &third, &key(0xb3), &att2).await,
            StatusCode::CONFLICT
        );
        let (status, _) = send_attested(
            &app,
            &a.gid,
            &transfer_body(&a.new_root, &a.gid, third.public_key()),
            &att2,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            probe_root(&app, &a.gid, &a.new_root).await,
            StatusCode::NO_CONTENT
        );

        // A plain transfer by the new root leaves the generation alone; a
        // later attested one raises it.
        assert_eq!(
            send_statement(
                &app,
                &a.gid,
                &a.new_root,
                StatementBody::RootTransfer {
                    new_root: third.public_key()
                },
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(generation(&pool, &a.gid).await, 2);
        let att3 = attestation_for(&a.id, 3);
        let (status, _) = send_attested(
            &app,
            &a.gid,
            &transfer_body(&third, &a.gid, new_root),
            &att3,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(generation(&pool, &a.gid).await, 3);
    }
}
