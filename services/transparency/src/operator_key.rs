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
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
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
    let bytes = decode_hex(hex).context("LAKKO_TRANSPARENCY_OPERATOR_SEED_HEX is not valid hex")?;
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

/// Pin the auth genesis-attestation VERIFYING key from
/// `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY` (ADR-0036 S4).
///
/// The value is a base64-standard 32-byte Ed25519 public key — the auth
/// service's attest public key (the same one published at
/// `GET /.well-known/device-attest-jwks.json`). The operator pins it
/// build/deploy-time, exactly as the clients pin the device-attest key and as
/// the witness pins the operator key; when set, the transparency append gate
/// verifies a genesis attestation against it (`verify_genesis_fresh`) and never
/// consults a served endpoint.
///
/// Returns `None` when the env var is unset/empty. With
/// `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` OFF (the default), that leaves
/// the gate inert; with the flag ON but no key, every genesis/reset append
/// fails closed (a loud misconfiguration, surfaced by `main`).
///
/// # Errors
///
/// Fails loud when the value is set but not valid base64 or not a valid 32-byte
/// Ed25519 public key.
pub(crate) fn genesis_attest_key_from_env() -> anyhow::Result<Option<signing::VerifyingKey>> {
    match std::env::var("LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY") {
        Ok(raw) => parse_genesis_attest_key(&raw),
        Err(_) => Ok(None),
    }
}

/// Pure parse half of [`genesis_attest_key_from_env`] — kept separate so it can
/// be unit-tested without touching the process environment.
fn parse_genesis_attest_key(raw: &str) -> anyhow::Result<Option<signing::VerifyingKey>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw)
        .context("LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY is not valid base64")?;
    let key = signing::VerifyingKey::from_bytes(&bytes).context(
        "LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY is not a valid 32-byte Ed25519 public key",
    )?;
    Ok(Some(key))
}

/// Decode a hex string to bytes (no `hex` crate dep for one call site).
fn decode_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        anyhow::bail!("hex string has odd length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow::anyhow!("bad hex: {e}")))
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
    fn parse_genesis_attest_key_round_trips_a_pinned_pubkey() {
        // A valid base64 Ed25519 public key parses back to the same key an auth
        // service published (ADR-0036 S4 pins exactly this).
        let signer = signing::SigningKey::from_seed(&[0x77; 32]);
        let vk = signer.verifying_key();
        let b64 = B64.encode(vk.to_bytes());
        let parsed = parse_genesis_attest_key(&b64)
            .expect("valid pinned key parses")
            .expect("some key");
        assert_eq!(parsed.to_bytes(), vk.to_bytes());
        // Surrounding whitespace is tolerated (env files are whitespace-prone).
        let parsed_ws = parse_genesis_attest_key(&format!("  {b64}\n"))
            .expect("whitespace-padded key parses")
            .expect("some key");
        assert_eq!(parsed_ws.to_bytes(), vk.to_bytes());
    }

    #[test]
    fn parse_genesis_attest_key_empty_is_none_and_garbage_fails_loud() {
        // Unset/empty → None (the gate is inert unless the flag is on).
        assert!(parse_genesis_attest_key("").expect("empty is ok").is_none());
        assert!(parse_genesis_attest_key("   ")
            .expect("blank is ok")
            .is_none());
        // Not base64 → loud error.
        assert!(parse_genesis_attest_key("not base64!!!").is_err());
        // Valid base64 but the wrong length for an Ed25519 public key → loud.
        assert!(parse_genesis_attest_key(&B64.encode([0u8; 16])).is_err());
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
