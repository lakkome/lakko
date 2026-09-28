// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Random ledger ids for the three invite ledgers (`/i/`, `/c/`, `/g/`).
//!
//! Since the short-link amendment of ADR-0028 (2026-09-27) a ledger row is
//! addressed by a random [`BLOB_ID_LEN`]-byte id the server mints at upload
//! ([`lakko_contacts::invite::generate_blob_id`]), not by a hash of the
//! capability. The id is a lookup handle only, and uploading the same blob
//! twice gives two rows under two ids. Since the 2026-09-28 security pass the
//! row holds the capability SEALED under a secret in the link fragment
//! (`lakko_contacts::link_seal`); the joiner opens it and verifies the
//! Ed25519 signature inside, and the server can neither read nor substitute
//! it. The upload helpers that check the sealed blob's outer frame and the
//! clear issuer key live here too, shared by the three ledgers.
//!
//! A fresh id can collide with a live row (the space is 2^48). The insert is a
//! plain `INSERT` - never an upsert, so a collision can never overwrite another
//! link's capability - retried under a new id on a primary-key violation, a
//! bounded number of times, then a 500.

use std::future::Future;

use axum::http::StatusCode;
use lakko_contacts::invite::{generate_blob_id, BLOB_ID_LEN};

/// How many fresh ids an upload tries before giving up. A collision needs a
/// live row under the same 48-bit id; several in a row means something other
/// than chance (a broken RNG or a table the size of the id space).
pub(crate) const MAX_ID_ATTEMPTS: usize = 4;

/// Run `insert` under a fresh random id until it succeeds, retrying only on a
/// unique-key violation. `table` names the ledger in logs.
///
/// # Errors
///
/// A 500 on any other database error, or when [`MAX_ID_ATTEMPTS`] ids all
/// collided.
pub(crate) async fn insert_with_fresh_id<F, Fut, T>(
    table: &'static str,
    mut insert: F,
) -> Result<[u8; BLOB_ID_LEN], (StatusCode, String)>
where
    F: FnMut([u8; BLOB_ID_LEN]) -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    for attempt in 1..=MAX_ID_ATTEMPTS {
        let id = generate_blob_id();
        match insert(id).await {
            Ok(_) => return Ok(id),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                tracing::warn!(
                    table,
                    attempt,
                    "ledger id collision, retrying with a fresh id"
                );
            },
            Err(e) => {
                tracing::error!(table, error = %e, "insert ledger row");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                ));
            },
        }
    }
    tracing::error!(
        table,
        attempts = MAX_ID_ATTEMPTS,
        "every fresh ledger id collided"
    );
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        "could not allocate a link id".to_string(),
    ))
}

/// Decode a ledger id from its URL form (base64url, no padding). Only the
/// current width is accepted, so a pre-amendment 43-character id is refused
/// with a 400 rather than looked up.
///
/// # Errors
///
/// A 400 when the string is not base64url or does not decode to exactly
/// [`BLOB_ID_LEN`] bytes.
pub(crate) fn parse_blob_id(slug: &str) -> Result<[u8; BLOB_ID_LEN], (StatusCode, String)> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    use base64::Engine as _;
    let bytes = B64URL.decode(slug).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid blob_id base64url: {e}"),
        )
    })?;
    <[u8; BLOB_ID_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("blob_id must be {BLOB_ID_LEN} bytes after decode"),
        )
    })
}

/// Decode and bound-check the `sealed` upload field (standard base64) of a
/// link of `kind`. The server cannot open the blob; it checks only what it
/// can see: a non-empty body longer than the seal overhead, no longer than
/// `max_len` (the kind's `MAX_SEALED_LEN`), whose outer kind and version bytes
/// name this ledger, so a blob uploaded to the wrong ledger is refused here
/// rather than at the joiner.
///
/// # Errors
///
/// 400 for bad base64, a short blob or a foreign kind / version; 413 past
/// `max_len`.
pub(crate) fn parse_sealed_upload(
    b64: &str,
    kind: lakko_contacts::link_seal::LinkKind,
    max_len: usize,
) -> Result<Vec<u8>, (StatusCode, String)> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use lakko_contacts::link_seal::{LINK_SEAL_OVERHEAD, LINK_SEAL_VERSION};
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid sealed base64: {e}"),
        )
    })?;
    if bytes.len() > max_len {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "sealed link exceeds the {max_len}-byte ceiling (got {} bytes)",
                bytes.len()
            ),
        ));
    }
    if bytes.len() <= LINK_SEAL_OVERHEAD {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("sealed link must be longer than {LINK_SEAL_OVERHEAD} bytes"),
        ));
    }
    if bytes[0] != kind.byte() || bytes[1] != LINK_SEAL_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            "sealed link is not of this ledger's kind or version".to_string(),
        ));
    }
    Ok(bytes)
}

/// Decode the clear `issuer_pubkey` upload field (standard base64 of a
/// 32-byte Ed25519 verifying key) — the key a later revoke must be signed
/// by. The server cannot check it against the sealed capability; an uploader
/// who names someone else's key only gives that person, not themselves, the
/// power to revoke the row.
///
/// # Errors
///
/// 400 for bad base64 or a value that is not a valid Ed25519 key.
pub(crate) fn parse_issuer_pubkey(
    b64: &str,
) -> Result<[u8; lakko_crypto::signing::PUBLIC_KEY_LEN], (StatusCode, String)> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid issuer_pubkey base64: {e}"),
        )
    })?;
    lakko_crypto::signing::VerifyingKey::from_bytes(&bytes).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "issuer_pubkey is not an Ed25519 verifying key".to_string(),
        )
    })?;
    <[u8; lakko_crypto::signing::PUBLIC_KEY_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "issuer_pubkey must be 32 bytes".to_string(),
        )
    })
}

/// Recover the stored issuer key of a row for a revoke. A malformed stored
/// key folds into `None` (the caller answers 404, like a missing row).
pub(crate) fn stored_issuer_key(bytes: &[u8]) -> Option<lakko_crypto::signing::VerifyingKey> {
    lakko_crypto::signing::VerifyingKey::from_bytes(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    use base64::Engine as _;
    use lakko_contacts::invite::BLOB_ID_B64URL_LEN;

    #[test]
    fn a_fresh_id_round_trips_through_its_eight_character_url_form() {
        let id = generate_blob_id();
        let slug = B64URL.encode(id);
        assert_eq!(slug.len(), BLOB_ID_B64URL_LEN);
        assert_eq!(parse_blob_id(&slug).unwrap(), id);
    }

    #[test]
    fn the_pre_amendment_forty_three_character_id_is_refused() {
        let old = B64URL.encode([0xABu8; 32]);
        assert_eq!(old.len(), 43);
        assert_eq!(parse_blob_id(&old).unwrap_err().0, StatusCode::BAD_REQUEST);
        for bad in ["", "AAAAAAA", "AAAAAAAAA", "AAAA=AAA", "AAAA/AAA"] {
            assert!(parse_blob_id(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// DB-gated on `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips, does not fail,
    /// when unset). Drives the real `group_invites` table: a forced primary-key
    /// collision is retried under a fresh id, and the row that was already
    /// there keeps its bytes (a collision never overwrites).
    #[tokio::test]
    async fn a_colliding_id_is_retried_and_never_overwrites() {
        use sqlx::mysql::MySqlPoolOptions;

        async fn insert(
            pool: &sqlx::MySqlPool,
            id: [u8; BLOB_ID_LEN],
            cap: &[u8],
        ) -> Result<sqlx::mysql::MySqlQueryResult, sqlx::Error> {
            sqlx::query(
                "INSERT INTO group_invites \
                    (blob_id, sealed_capability, issuer_pubkey, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, 0, 0)",
            )
            .bind(id.to_vec())
            .bind(cap.to_vec())
            .bind(vec![0x11u8; 32])
            .execute(pool)
            .await
        }

        let Ok(url) = std::env::var("LAKKO_DISCOVERY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping a_colliding_id_is_retried_and_never_overwrites: \
                 LAKKO_DISCOVERY_TEST_DATABASE_URL unset"
            );
            return;
        };
        let pool = MySqlPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");

        // A first row under a fresh id.
        let first = insert_with_fresh_id("group_invites", |id| {
            let pool = pool.clone();
            async move { insert(&pool, id, b"first capability").await }
        })
        .await
        .expect("first insert");

        // Second upload: the first attempt is forced onto the taken id.
        let mut attempts = 0usize;
        let second = insert_with_fresh_id("group_invites", |id| {
            attempts += 1;
            let use_id = if attempts == 1 { first } else { id };
            let pool = pool.clone();
            async move { insert(&pool, use_id, b"second capability").await }
        })
        .await
        .expect("second insert lands under a fresh id");
        assert_eq!(attempts, 2, "exactly one retry after the collision");
        assert_ne!(second, first);

        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT sealed_capability FROM group_invites WHERE blob_id = ?")
                .bind(first.to_vec())
                .fetch_one(&pool)
                .await
                .expect("first row");
        assert_eq!(stored, b"first capability", "a collision never overwrites");

        // Every attempt colliding ends in a 500, not an overwrite.
        let r = insert_with_fresh_id("group_invites", |_| {
            let pool = pool.clone();
            async move { insert(&pool, first, b"third capability").await }
        })
        .await;
        assert_eq!(r.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);

        // The pre-amendment 32-byte id no longer fits the column.
        assert!(
            sqlx::query(
                "INSERT INTO group_invites \
                    (blob_id, sealed_capability, issuer_pubkey, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, 0, 0)",
            )
            .bind(vec![0x33u8; 32])
            .bind(b"x".to_vec())
            .bind(vec![0x11u8; 32])
            .execute(&pool)
            .await
            .is_err(),
            "a 32-byte id must be refused by the BINARY(6) column"
        );

        for id in [first, second] {
            sqlx::query("DELETE FROM group_invites WHERE blob_id = ?")
                .bind(id.to_vec())
                .execute(&pool)
                .await
                .expect("cleanup");
        }
    }

    #[test]
    fn a_sealed_upload_is_checked_for_kind_version_and_bounds() {
        use base64::engine::general_purpose::STANDARD as B64;
        use lakko_contacts::link_seal::{generate_link_secret, seal, LinkKind, LINK_SEAL_OVERHEAD};
        let good = seal(LinkKind::Group, &generate_link_secret(), b"cap").unwrap();
        assert_eq!(
            parse_sealed_upload(&B64.encode(&good), LinkKind::Group, 64).unwrap(),
            good
        );
        // Another ledger's blob, a wrong version, a bare frame, over the
        // ceiling, not base64.
        assert_eq!(
            parse_sealed_upload(&B64.encode(&good), LinkKind::Call, 64)
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        let mut version = good.clone();
        version[1] ^= 1;
        assert!(parse_sealed_upload(&B64.encode(version), LinkKind::Group, 64).is_err());
        let bare = vec![b'g'; LINK_SEAL_OVERHEAD];
        assert!(parse_sealed_upload(&B64.encode(bare), LinkKind::Group, 64).is_err());
        assert_eq!(
            parse_sealed_upload(&B64.encode(&good), LinkKind::Group, good.len() - 1)
                .unwrap_err()
                .0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(parse_sealed_upload("not base64!", LinkKind::Group, 64).is_err());
    }

    #[test]
    fn an_issuer_key_must_be_a_verifying_key() {
        use base64::engine::general_purpose::STANDARD as B64;
        let key = lakko_crypto::signing::SigningKey::generate()
            .verifying_key()
            .to_bytes();
        assert_eq!(parse_issuer_pubkey(&B64.encode(key)).unwrap(), key);
        assert!(parse_issuer_pubkey(&B64.encode([1u8; 31])).is_err());
        assert!(parse_issuer_pubkey(&B64.encode([1u8; 33])).is_err());
        assert!(parse_issuer_pubkey("%%").is_err());
        assert!(stored_issuer_key(&key).is_some());
        assert!(stored_issuer_key(&[0u8; 3]).is_none());
    }

    #[tokio::test]
    async fn a_collision_is_retried_and_other_errors_are_not() {
        // First attempt fails with a non-unique error: no retry, a 500.
        let mut calls = 0;
        let r = insert_with_fresh_id("t", |_| {
            calls += 1;
            async { Err::<(), _>(sqlx::Error::RowNotFound) }
        })
        .await;
        assert_eq!(r.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(calls, 1);
    }
}
