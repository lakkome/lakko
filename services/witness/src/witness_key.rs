// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness signing-key persistence — the witness's OWN trust anchor (ADR-0020).
//!
//! Mirrors the transparency operator key: a single-row table, generate a fresh
//! 32-byte Ed25519 seed + `INSERT IGNORE` on first boot, read it back, rebuild
//! the [`signing::SigningKey`]. The seed NEVER leaves the server; the derived
//! verifying key is published at `GET /witness/key` and pinned by clients
//! out-of-band, independent of the operator key.

use anyhow::Context as _;
use lakko_crypto::signing;
use rand::RngCore as _;
use sqlx::MySqlPool;

/// Load (or generate + persist on first run) the Ed25519 witness signing key.
///
/// # Errors
///
/// Propagates sqlx errors; fails loud if a persisted seed is not exactly 32
/// bytes (DB corruption).
pub(crate) async fn load_or_init_witness_key(
    pool: &MySqlPool,
) -> anyhow::Result<signing::SigningKey> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT seed FROM witness_key WHERE id = 1")
        .fetch_optional(pool)
        .await
        .context("read witness_key")?;
    if let Some((seed_bytes,)) = row {
        return signing_key_from_seed_bytes(&seed_bytes);
    }

    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    sqlx::query("INSERT IGNORE INTO witness_key (id, seed) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert witness_key")?;

    // Re-read so a racing insert wins gracefully.
    let (seed_bytes,): (Vec<u8>,) = sqlx::query_as("SELECT seed FROM witness_key WHERE id = 1")
        .fetch_one(pool)
        .await
        .context("re-read witness_key after insert")?;
    signing_key_from_seed_bytes(&seed_bytes)
}

/// Rebuild a [`signing::SigningKey`] from a persisted 32-byte seed.
fn signing_key_from_seed_bytes(seed_bytes: &[u8]) -> anyhow::Result<signing::SigningKey> {
    if seed_bytes.len() != 32 {
        anyhow::bail!(
            "witness_key.seed length {} != 32 — DB corruption?",
            seed_bytes.len()
        );
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(seed_bytes);
    Ok(signing::SigningKey::from_seed(&seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_round_trips_to_same_verifying_key() {
        let seed = [0x42u8; 32];
        let key = signing_key_from_seed_bytes(&seed).expect("32-byte seed");
        let again = signing_key_from_seed_bytes(&seed).expect("32-byte seed");
        assert_eq!(key.verifying_key(), again.verifying_key());
        assert_eq!(
            key.verifying_key(),
            signing::SigningKey::from_seed(&seed).verifying_key()
        );
    }

    #[test]
    fn wrong_length_seed_fails_loud() {
        assert!(signing_key_from_seed_bytes(&[0u8; 16]).is_err());
        assert!(signing_key_from_seed_bytes(&[0u8; 33]).is_err());
        assert!(signing_key_from_seed_bytes(&[]).is_err());
    }
}
