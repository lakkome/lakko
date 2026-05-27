// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Tamper-evident chain across backup versions (G-11
//! mitigation).
//!
//! ## Threat
//!
//! `services/storage` (or any cache between client and
//! storage) sees an opaque sequence of backup envelopes
//! the client uploaded over time. A passive observer
//! learns nothing — the envelope is AEAD-sealed under
//! Argon2id(phrase). But an **active** attacker who
//! controls the storage layer can:
//!
//! 1. Roll a user's backup back: instead of returning the
//!    latest version `N`, the server hands the client an
//!    older version `N-5`. The AEAD opens cleanly (same
//!    phrase), the payload is structurally valid, and
//!    the client silently downgrades its state to a
//!    week-old baseline (resurrects already-revoked
//!    devices, forgets recently-accepted contacts, etc.).
//!
//! The v1 envelope (`crate::envelope`) does not defend
//! against this — each envelope is standalone, with no
//! way to tell "this is the current version" from "this
//! is six months old".
//!
//! ## Mitigation
//!
//! Each v2 envelope carries a plaintext **chain header**
//! — `(version: u64, prev_hash: [u8; 32])` — bound into
//! the AEAD AAD. The version is monotone (every fresh
//! seal bumps it by 1); `prev_hash` is BLAKE3 of the
//! previous envelope's full on-wire bytes. The first
//! envelope a user ever uploads has
//! `prev_hash == GENESIS_PREV_HASH` (all-zero 32-byte
//! marker).
//!
//! The header is plaintext on wire so [`verify_chain`]
//! can audit a sequence without holding the recovery
//! phrase (the version is a public count anyway — no
//! privacy leak — and `prev_hash` is a public
//! commitment). The header is AAD-bound so a server that
//! tries to edit either field gets a clean AEAD-open
//! failure on the next `open_v2`.
//!
//! ## What this defends against
//!
//! - **Client with local state.** The device stores
//!   `(last_known_version, last_known_envelope_hash)` in
//!   its device-key-encrypted local storage (separate
//!   from the backup blob). On every restore the client
//!   checks the returned envelope's `version` >=
//!   `last_known_version` and, if the chain history is
//!   available, walks [`verify_chain`] from the last
//!   known anchor to the latest envelope. Server-side
//!   rollback is caught.
//! - **Multi-version restore.** During a recovery flow
//!   where the storage layer hands back a *sequence* of
//!   envelopes (e.g., for forensics or audit), the chain
//!   commits each version to the previous. Any single
//!   broken link is fatal.
//!
//! ## What this does NOT defend against
//!
//! - **Fresh-install rollback.** A user who lost all
//!   local state and is restoring from scratch with only
//!   the recovery phrase has no way to tell whether the
//!   server returned the latest envelope or an older
//!   one — both verify cleanly against the same phrase.
//!   This requires a separate mechanism (e.g., a KT-log
//!   binding "latest backup envelope hash for account
//!   X = h" via an out-of-band signed timestamp) and is
//!   tracked separately.
//!
//! ## Wire format (v2)
//!
//! ```text
//! 1B   schema_version = 2
//! 16B  salt
//! 12B  nonce
//! 8B   version (u64 big-endian)        // chain header begins
//! 32B  prev_hash                       // chain header ends
//! 4B   ciphertext_len (u32 big-endian)
//! NB   ciphertext + 16B AEAD tag
//! ```
//!
//! AAD =
//! `ENVELOPE_DOMAIN_V2 || schema_version || salt || nonce || version_u64_be || prev_hash`.

use lakko_crypto::aead::{self, Algorithm, NONCE_LEN};
use lakko_crypto::hash;
use lakko_crypto::kdf;

use crate::envelope::SALT_LEN;
use crate::error::{BackupError, Result};

/// Schema version for the chained envelope.
pub const SCHEMA_VERSION_V2: u8 = 2;

/// Domain separator bound into the AEAD AAD for v2.
/// Distinct from v1 so a v1 ciphertext cannot be
/// replayed as a v2 ciphertext.
pub const ENVELOPE_DOMAIN_V2: &[u8] = b"lakko-backup/v2\x00";

/// Sentinel `prev_hash` value for the very first
/// envelope a user ever uploads. Any well-formed chain
/// starts with this.
pub const GENESIS_PREV_HASH: [u8; 32] = [0u8; 32];

/// 8 (version u64 BE) + 32 (prev_hash) = 40 bytes.
const CHAIN_HEADER_LEN: usize = 8 + 32;

/// Length of the four-byte big-endian ciphertext-length
/// prefix.
const CT_LEN_FIELD: usize = 4;

/// Fixed-length prefix on wire before the ciphertext —
/// version-byte + salt + nonce + chain-header +
/// length-prefix.
const FIXED_PREFIX_LEN: usize = 1 + SALT_LEN + NONCE_LEN + CHAIN_HEADER_LEN + CT_LEN_FIELD;

/// Chain header. Plaintext on wire, AAD-bound under the
/// AEAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainHeader {
    /// Monotone backup version. Starts at 1; the first
    /// envelope a user uploads has `version == 1`,
    /// `prev_hash == GENESIS_PREV_HASH`. Each subsequent
    /// seal bumps the version by exactly 1.
    pub version: u64,
    /// BLAKE3 of the previous envelope's full on-wire
    /// bytes. The genesis envelope sets this to
    /// [`GENESIS_PREV_HASH`].
    pub prev_hash: [u8; 32],
}

impl ChainHeader {
    /// Build the genesis header — the first envelope a
    /// user ever uploads. `version = 1`, `prev_hash =
    /// GENESIS_PREV_HASH`.
    #[must_use]
    pub fn genesis() -> Self {
        Self {
            version: 1,
            prev_hash: GENESIS_PREV_HASH,
        }
    }

    /// Build the next header following an existing
    /// envelope. Hashes `prev_envelope_wire` and
    /// increments the version.
    #[must_use]
    pub fn after(prev_envelope_wire: &[u8], prev_version: u64) -> Self {
        Self {
            version: prev_version.saturating_add(1),
            prev_hash: envelope_hash(prev_envelope_wire),
        }
    }

    fn encode(&self) -> [u8; CHAIN_HEADER_LEN] {
        let mut out = [0u8; CHAIN_HEADER_LEN];
        out[0..8].copy_from_slice(&self.version.to_be_bytes());
        out[8..40].copy_from_slice(&self.prev_hash);
        out
    }

    fn decode(bytes: &[u8]) -> Self {
        debug_assert_eq!(bytes.len(), CHAIN_HEADER_LEN);
        let mut v = [0u8; 8];
        v.copy_from_slice(&bytes[0..8]);
        let version = u64::from_be_bytes(v);
        let mut prev_hash = [0u8; 32];
        prev_hash.copy_from_slice(&bytes[8..40]);
        Self { version, prev_hash }
    }
}

/// BLAKE3 hash of an entire v2 envelope wire form. This
/// is what the *next* envelope's `prev_hash` must equal.
/// Computed over the full bytes including the version
/// byte and the ciphertext tag — every byte the server
/// could plausibly tamper with.
#[must_use]
pub fn envelope_hash(wire: &[u8]) -> [u8; 32] {
    hash::blake3(wire)
}

/// Encrypt `payload_bytes` with chain-aware AAD. The
/// caller controls the chain header so the next-version
/// computation can be done outside this crate (e.g., by
/// the storage client that owns the persisted "last
/// known" record).
///
/// `phrase` is the BIP39 recovery phrase bytes — same
/// shape as [`crate::envelope::seal`].
///
/// # Errors
///
/// * [`BackupError::Crypto`] on Argon2id failure.
/// * [`BackupError::InvalidEncoding`] if `payload_bytes`
///   overflows the u32 length prefix.
pub fn seal_v2(
    phrase: &[u8],
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    header: &ChainHeader,
    payload_bytes: &[u8],
) -> Result<Vec<u8>> {
    let kek = kdf::argon2id_kek(phrase, salt)?;
    let aad = build_aad(SCHEMA_VERSION_V2, salt, nonce, header);
    let ciphertext = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &kek,
        nonce,
        &aad,
        payload_bytes,
    )?;

    let ct_len = u32::try_from(ciphertext.len()).map_err(|_| {
        BackupError::InvalidEncoding(format!(
            "ciphertext too large for u32be length prefix ({} bytes)",
            ciphertext.len(),
        ))
    })?;

    let header_bytes = header.encode();
    let mut out = Vec::with_capacity(FIXED_PREFIX_LEN + ciphertext.len());
    out.push(SCHEMA_VERSION_V2);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&ct_len.to_be_bytes());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a v2 envelope back to `(header, payload)`.
///
/// # Errors
///
/// * [`BackupError::InvalidEncoding`] if the wire bytes
///   are malformed (wrong schema version, truncated,
///   length-prefix overflow, trailing junk).
/// * [`BackupError::DecryptFailed`] if AEAD-open fails
///   (wrong phrase OR tampered envelope OR tampered
///   plaintext-but-AAD-bound chain header).
/// * [`BackupError::Crypto`] on Argon2id failure
///   (unreachable for fixed-params API).
pub fn open_v2(phrase: &[u8], wire: &[u8]) -> Result<(ChainHeader, Vec<u8>)> {
    if wire.len() < FIXED_PREFIX_LEN {
        return Err(BackupError::InvalidEncoding(format!(
            "v2 envelope too short: {} bytes, need >= {FIXED_PREFIX_LEN}",
            wire.len(),
        )));
    }
    let version_byte = wire[0];
    if version_byte != SCHEMA_VERSION_V2 {
        return Err(BackupError::InvalidEncoding(format!(
            "unsupported v2 backup schema version {version_byte} (expected {SCHEMA_VERSION_V2})",
        )));
    }
    let mut off = 1;
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&wire[off..off + SALT_LEN]);
    off += SALT_LEN;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&wire[off..off + NONCE_LEN]);
    off += NONCE_LEN;
    let header = ChainHeader::decode(&wire[off..off + CHAIN_HEADER_LEN]);
    off += CHAIN_HEADER_LEN;
    let mut ct_len_bytes = [0u8; CT_LEN_FIELD];
    ct_len_bytes.copy_from_slice(&wire[off..off + CT_LEN_FIELD]);
    off += CT_LEN_FIELD;
    let ct_len = u32::from_be_bytes(ct_len_bytes) as usize;

    let ct_end = off
        .checked_add(ct_len)
        .ok_or_else(|| BackupError::InvalidEncoding("ciphertext length overflow".into()))?;
    if wire.len() < ct_end {
        return Err(BackupError::InvalidEncoding(format!(
            "v2 envelope claims {ct_len} ciphertext bytes, only {} remain",
            wire.len() - off,
        )));
    }
    if wire.len() != ct_end {
        return Err(BackupError::InvalidEncoding(format!(
            "{} trailing bytes after v2 ciphertext",
            wire.len() - ct_end,
        )));
    }
    let ciphertext = &wire[off..ct_end];

    let kek = kdf::argon2id_kek(phrase, &salt)?;
    let aad = build_aad(SCHEMA_VERSION_V2, &salt, &nonce, &header);
    let payload = aead::open(Algorithm::ChaCha20Poly1305, &kek, &nonce, &aad, ciphertext)
        .map_err(|_| BackupError::DecryptFailed)?;
    Ok((header, payload))
}

/// Parse the chain header from a v2 envelope wire form
/// without running AEAD-open. The verifier uses this to
/// audit a sequence of envelopes without holding the
/// recovery phrase. The header is AAD-bound so any later
/// `open_v2` would still catch a tampered field — this
/// function just exposes what the wire claims.
///
/// # Errors
///
/// [`BackupError::InvalidEncoding`] if the wire is too
/// short or the schema-version byte is wrong.
pub fn header_of(wire: &[u8]) -> Result<ChainHeader> {
    if wire.len() < FIXED_PREFIX_LEN {
        return Err(BackupError::InvalidEncoding(format!(
            "v2 envelope too short: {} bytes, need >= {FIXED_PREFIX_LEN}",
            wire.len(),
        )));
    }
    if wire[0] != SCHEMA_VERSION_V2 {
        return Err(BackupError::InvalidEncoding(format!(
            "not a v2 envelope: schema byte {} (expected {SCHEMA_VERSION_V2})",
            wire[0],
        )));
    }
    let header_off = 1 + SALT_LEN + NONCE_LEN;
    Ok(ChainHeader::decode(
        &wire[header_off..header_off + CHAIN_HEADER_LEN],
    ))
}

/// Walk an ordered sequence of v2 envelopes and verify
/// the chain integrity:
///
/// 1. Every envelope is well-formed v2.
/// 2. Versions are sequential — `header[i].version ==
///    header[i-1].version + 1`.
/// 3. `header[i].prev_hash == envelope_hash(envelope[i-1])`
///    for every `i > 0`.
///
/// The first envelope's `prev_hash` is NOT checked
/// against `GENESIS_PREV_HASH` — the caller decides
/// whether they're walking from genesis (then they check
/// `result[0].prev_hash == GENESIS_PREV_HASH` themselves)
/// or from a known anchor (then they check it equals
/// their local `last_known_envelope_hash`).
///
/// Returns the parsed headers in input order so the
/// caller can hand them to the storage-side reconciler.
///
/// # Errors
///
/// * [`BackupError::InvalidEncoding`] if any envelope is
///   malformed or any chain invariant fails.
pub fn verify_chain(envelopes: &[&[u8]]) -> Result<Vec<ChainHeader>> {
    if envelopes.is_empty() {
        return Ok(Vec::new());
    }
    let mut headers = Vec::with_capacity(envelopes.len());
    headers.push(header_of(envelopes[0])?);
    for i in 1..envelopes.len() {
        let next = header_of(envelopes[i])?;
        if next.version != headers[i - 1].version.saturating_add(1) {
            return Err(BackupError::InvalidEncoding(format!(
                "non-sequential chain at index {i}: got version {}, expected {}",
                next.version,
                headers[i - 1].version + 1,
            )));
        }
        let actual_prev = envelope_hash(envelopes[i - 1]);
        if next.prev_hash != actual_prev {
            return Err(BackupError::InvalidEncoding(format!(
                "broken chain link at index {i}: prev_hash mismatch",
            )));
        }
        headers.push(next);
    }
    Ok(headers)
}

/// Build the AEAD associated-data bytes for a v2 envelope.
/// Same shape on both seal + open sides so any tampered
/// salt / nonce / chain-header bit invalidates the AEAD
/// tag on open.
fn build_aad(
    version: u8,
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    header: &ChainHeader,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_DOMAIN_V2.len() + 1 + SALT_LEN + NONCE_LEN + CHAIN_HEADER_LEN);
    out.extend_from_slice(ENVELOPE_DOMAIN_V2);
    out.push(version);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&header.encode());
    out
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &[u8] = b"abandon abandon abandon abandon abandon abandon \
                            abandon abandon abandon abandon abandon about";

    fn salt() -> [u8; SALT_LEN] {
        [0x11; SALT_LEN]
    }

    fn nonce(byte: u8) -> [u8; NONCE_LEN] {
        [byte; NONCE_LEN]
    }

    fn payload(seed: u8) -> Vec<u8> {
        vec![seed; 64]
    }

    /// Roll a small chain of `n` envelopes. Returns the
    /// envelopes in order so tests can call
    /// `verify_chain` directly.
    fn build_chain(n: u64) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(usize::try_from(n).unwrap());
        let mut header = ChainHeader::genesis();
        for i in 0..n {
            let envelope = seal_v2(
                PHRASE,
                &salt(),
                &nonce(u8::try_from(i).unwrap_or(0xAA)),
                &header,
                &payload(u8::try_from(i).unwrap_or(0xBB)),
            )
            .expect("seal");
            // Next header references this envelope.
            header = ChainHeader::after(&envelope, header.version);
            out.push(envelope);
        }
        out
    }

    #[test]
    fn seal_open_round_trip() {
        let header = ChainHeader::genesis();
        let pt = payload(0xC0);
        let wire = seal_v2(PHRASE, &salt(), &nonce(0xE1), &header, &pt).unwrap();
        let (parsed_header, recovered) = open_v2(PHRASE, &wire).unwrap();
        assert_eq!(parsed_header, header);
        assert_eq!(recovered, pt);
    }

    #[test]
    fn header_of_round_trips() {
        let header = ChainHeader {
            version: 42,
            prev_hash: [0xAB; 32],
        };
        let wire = seal_v2(PHRASE, &salt(), &nonce(0x55), &header, &payload(7)).unwrap();
        let parsed = header_of(&wire).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn verify_chain_walks_three_envelopes() {
        let envelopes = build_chain(3);
        let refs: Vec<&[u8]> = envelopes.iter().map(Vec::as_slice).collect();
        let headers = verify_chain(&refs).unwrap();
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].version, 1);
        assert_eq!(headers[0].prev_hash, GENESIS_PREV_HASH);
        assert_eq!(headers[1].version, 2);
        assert_eq!(headers[1].prev_hash, envelope_hash(&envelopes[0]));
        assert_eq!(headers[2].version, 3);
        assert_eq!(headers[2].prev_hash, envelope_hash(&envelopes[1]));
    }

    #[test]
    fn verify_chain_rejects_version_skip() {
        // Build {v1, v2, v3}, drop v2 — server pretends
        // v2 never happened. The chain walk catches the
        // gap.
        let envelopes = build_chain(3);
        let skipped: Vec<&[u8]> = vec![&envelopes[0], &envelopes[2]];
        match verify_chain(&skipped) {
            Err(BackupError::InvalidEncoding(msg)) => {
                assert!(msg.contains("non-sequential"), "wrong message: {msg}");
            }
            other => panic!("expected non-sequential rejection, got {other:?}"),
        }
    }

    #[test]
    fn verify_chain_rejects_broken_link() {
        // Build {v1, v2}, then re-seal v2 with the WRONG
        // prev_hash — pretend a server crafted a v2 that
        // claims to follow a v1' that does not exist.
        let envelopes = build_chain(1);
        let bad_header = ChainHeader {
            version: 2,
            prev_hash: [0xFF; 32], // not the real BLAKE3 of envelopes[0]
        };
        let bad_v2 = seal_v2(
            PHRASE,
            &salt(),
            &nonce(0x02),
            &bad_header,
            &payload(0x02),
        )
        .unwrap();
        let chain: Vec<&[u8]> = vec![&envelopes[0], &bad_v2];
        match verify_chain(&chain) {
            Err(BackupError::InvalidEncoding(msg)) => {
                assert!(msg.contains("broken chain link"), "wrong message: {msg}");
            }
            other => panic!("expected broken-link rejection, got {other:?}"),
        }
    }

    #[test]
    fn verify_chain_empty_input_is_empty_output() {
        let out = verify_chain(&[]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn verify_chain_single_envelope_no_link_to_check() {
        let envelopes = build_chain(1);
        let refs: Vec<&[u8]> = envelopes.iter().map(Vec::as_slice).collect();
        let headers = verify_chain(&refs).unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].version, 1);
        // For a single envelope, the caller decides
        // whether prev_hash == GENESIS or matches their
        // last-known anchor; verify_chain does NOT
        // enforce.
    }

    #[test]
    fn open_v2_rejects_wrong_schema_byte() {
        let header = ChainHeader::genesis();
        let mut wire = seal_v2(PHRASE, &salt(), &nonce(0x77), &header, &payload(9)).unwrap();
        wire[0] = 0xFF; // not SCHEMA_VERSION_V2
        match open_v2(PHRASE, &wire) {
            Err(BackupError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version"), "wrong message: {msg}");
            }
            other => panic!("expected schema-version rejection, got {other:?}"),
        }
    }

    #[test]
    fn open_v2_rejects_tampered_chain_header() {
        // Server tries to lie about the version: flip the
        // first byte of the plaintext chain header. The
        // AAD includes the header, so AEAD-open MUST
        // fail.
        let header = ChainHeader {
            version: 5,
            prev_hash: [0xAA; 32],
        };
        let mut wire = seal_v2(PHRASE, &salt(), &nonce(0x33), &header, &payload(3)).unwrap();
        // Header begins after 1 (schema) + SALT_LEN + NONCE_LEN bytes.
        let header_off = 1 + SALT_LEN + NONCE_LEN;
        wire[header_off] ^= 0xFF;
        match open_v2(PHRASE, &wire) {
            Err(BackupError::DecryptFailed) => {}
            other => panic!("expected DecryptFailed, got {other:?}"),
        }
    }

    #[test]
    fn open_v2_rejects_truncated_wire() {
        let header = ChainHeader::genesis();
        let wire = seal_v2(PHRASE, &salt(), &nonce(0x44), &header, &payload(4)).unwrap();
        let short = &wire[..wire.len() - 1];
        match open_v2(PHRASE, short) {
            Err(BackupError::InvalidEncoding(_)) => {}
            other => panic!("expected InvalidEncoding on truncated wire, got {other:?}"),
        }
    }

    #[test]
    fn open_v2_rejects_trailing_bytes() {
        let header = ChainHeader::genesis();
        let mut wire = seal_v2(PHRASE, &salt(), &nonce(0x66), &header, &payload(6)).unwrap();
        wire.push(0x00);
        match open_v2(PHRASE, &wire) {
            Err(BackupError::InvalidEncoding(msg)) => {
                assert!(msg.contains("trailing"), "wrong message: {msg}");
            }
            other => panic!("expected trailing-bytes rejection, got {other:?}"),
        }
    }

    #[test]
    fn header_after_increments_version_and_binds_hash() {
        let envelopes = build_chain(1);
        let h = ChainHeader::after(&envelopes[0], 1);
        assert_eq!(h.version, 2);
        assert_eq!(h.prev_hash, envelope_hash(&envelopes[0]));
    }
}
