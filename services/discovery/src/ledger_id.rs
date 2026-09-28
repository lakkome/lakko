// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Random ledger ids for the three invite ledgers (`/i/`, `/c/`, `/g/`).
//!
//! Since the short-link amendment of ADR-0028 (2026-09-27) a ledger row is
//! addressed by a random [`BLOB_ID_LEN`]-byte id the server mints at upload
//! ([`lakko_contacts::invite::generate_blob_id`]), not by a hash of the
//! capability. The id is a lookup handle only: the joiner still verifies the
//! capability's Ed25519 signature, and uploading the same capability twice
//! gives two rows under two ids.
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
                    (blob_id, capability_bytes, issuer_pubkey, group_id, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, ?, 0, 0)",
            )
            .bind(id.to_vec())
            .bind(cap.to_vec())
            .bind(vec![0x11u8; 32])
            .bind(vec![0x22u8; 16])
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
            sqlx::query_scalar("SELECT capability_bytes FROM group_invites WHERE blob_id = ?")
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
                    (blob_id, capability_bytes, issuer_pubkey, group_id, expiry_ms, max_uses) \
                 VALUES (?, ?, ?, ?, 0, 0)",
            )
            .bind(vec![0x33u8; 32])
            .bind(b"x".to_vec())
            .bind(vec![0x11u8; 32])
            .bind(vec![0x22u8; 16])
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
