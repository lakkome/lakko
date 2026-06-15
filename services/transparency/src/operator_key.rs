// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Operator signing-key persistence — the TRUST ANCHOR of the log.
//!
//! Mirrors `services/auth`'s `load_or_init_call_signing_key`: a
//! single-row table, generate a fresh 32-byte Ed25519 seed +
//! `INSERT IGNORE` on first boot, read it back, rebuild the
//! [`signing::SigningKey`]. The seed NEVER leaves the server; the
//! derived [`signing::VerifyingKey`] is what clients pin out-of-band
//! and verify every [`SignedRoot`](lakko_transparency::SignedRoot)
//! against.

use anyhow::Context as _;
use lakko_crypto::signing;
use rand::RngCore as _;
use sqlx::MySqlPool;

/// Load (or generate + persist on first run) the Ed25519 operator
/// signing key.
///
/// Same single-row `INSERT IGNORE` pattern as the auth service's
/// call-JWT key. A racing second startup loses the insert race
/// gracefully — both read back the same seed.
///
/// # Errors
///
/// Propagates sqlx errors, and fails loud if a persisted seed is not
/// exactly 32 bytes (DB corruption). Returns `anyhow::Error` since
/// this runs at startup; the caller surfaces it as a fatal bootstrap
/// error.
pub(crate) async fn load_or_init_operator_key(
    pool: &MySqlPool,
) -> anyhow::Result<signing::SigningKey> {
    // Try to read an existing row.
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT seed FROM transparency_operator_key WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("read transparency_operator_key")?;
    if let Some((seed_bytes,)) = row {
        return signing_key_from_seed_bytes(&seed_bytes);
    }

    // No row — generate a fresh seed + insert. `INSERT IGNORE` so two
    // racing startups don't both insert.
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    sqlx::query("INSERT IGNORE INTO transparency_operator_key (id, seed) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert transparency_operator_key")?;

    // Re-read so a racing insert wins gracefully.
    let (seed_bytes,): (Vec<u8>,) =
        sqlx::query_as("SELECT seed FROM transparency_operator_key WHERE id = 1")
            .fetch_one(pool)
            .await
            .context("re-read transparency_operator_key after insert")?;
    signing_key_from_seed_bytes(&seed_bytes)
}

/// Dev / e2e override: when `LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX` is set (64
/// hex chars = 32 bytes), use that FIXED operator seed instead of the
/// DB-generated key. This makes the operator verifying key DETERMINISTIC so a
/// test / e2e can compute it ahead of time (e.g. for the witness to pin, or for
/// a committed fixture, mirroring the sfu-e2e `LAKKO_SFU_E2E_SEED_HEX` pattern).
///
/// Returns `None` when the env var is unset/empty (production: the random
/// DB-generated key via [`load_or_init_operator_key`]).
///
/// # Errors
///
/// Fails loud when the value is set but not 64 valid hex chars.
pub(crate) fn operator_key_from_env() -> anyhow::Result<Option<signing::SigningKey>> {
    let Ok(hex) = std::env::var("LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX") else {
        return Ok(None);
    };
    let hex = hex.trim();
    if hex.is_empty() {
        return Ok(None);
    }
    let bytes = decode_hex(hex)
        .context("LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX is not valid hex")?;
    if bytes.len() != 32 {
        anyhow::bail!(
            "LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX decodes to {} bytes; need 32 (64 hex chars)",
            bytes.len()
        );
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(Some(signing::SigningKey::from_seed(&seed)))
}

/// Decode a hex string to bytes (no `hex` crate dep for one call site).
fn decode_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        anyhow::bail!("hex string has odd length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow::anyhow!("bad hex: {e}"))
        })
        .collect()
}

/// Rebuild a [`signing::SigningKey`] from a persisted 32-byte seed,
/// failing loud on a wrong length.
fn signing_key_from_seed_bytes(seed_bytes: &[u8]) -> anyhow::Result<signing::SigningKey> {
    if seed_bytes.len() != 32 {
        anyhow::bail!(
            "transparency_operator_key.seed length {} != 32 — DB corruption?",
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
        // The verifying key is deterministic in the seed — a re-load of
        // the same bytes pins the same public key, which is exactly what
        // a client out-of-band pin relies on.
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

    #[test]
    fn fixed_seed_env_override_derives_deterministic_key() {
        // The e2e pins the operator key via a fixed seed; assert the derived
        // verifying key is the committed fixture (ci/recovery-e2e wires the
        // witness's LAKKO_WITNESS_OPERATOR_PUBKEY to this).
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine as _;
        let key = signing::SigningKey::from_seed(&[0xAB; 32]);
        assert_eq!(
            B64.encode(key.verifying_key().to_bytes()),
            "JIrL26+eBQGW3nBL6i1odw5RkVDRA7WH2uLZytU92TA="
        );
    }
}
