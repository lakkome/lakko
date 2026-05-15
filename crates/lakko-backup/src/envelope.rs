// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Backup envelope wire codec — Argon2id KEK + ChaCha20-Poly1305
//! AEAD (Phase 2a1).
//!
//! ## Wire format (v1)
//!
//! ```text
//! u8           schema version (= 1)
//! [16 bytes]   argon2id salt
//! [12 bytes]   ChaCha20-Poly1305 nonce
//! u32be        ciphertext length (includes the 16-byte AEAD tag)
//! [..]         ciphertext
//! ```
//!
//! Total fixed overhead before the ciphertext: 1 + 16 + 12 + 4 =
//! **33 bytes**. The AEAD tag is appended to the ciphertext by
//! the underlying `lakko-crypto::aead::seal` so the
//! `ciphertext length` field captures `payload + 16` bytes.
//!
//! ## Argon2id parameters
//!
//! Pinned to the OWASP-2024 interactive defaults:
//! `(m_cost = 64 MiB, t_cost = 3, p_cost = 4, out = 32 bytes)`.
//! Defined by `SCHEMA_VERSION` — a future params rotation
//! (e.g. `SCHEMA_VERSION = 2` with `m = 128 MiB`) gets a fresh
//! version byte so old blobs keep decoding while new ones move
//! to the harder parameters.
//!
//! The recovery-phrase entropy (BIP39 24-word = 256 bits) makes
//! the work factor an honest soak rather than a brute-force
//! barrier — but we run Argon2id anyway so that a misuser who
//! manages to lose part of the phrase still buys time before an
//! offline attacker brute-forces the residual entropy.
//!
//! ## AAD
//!
//! The AEAD's associated data covers
//! `(ENVELOPE_DOMAIN_V1 || version_byte || salt || nonce)`. Tamper
//! with any of those header bytes and the tag check fails. This
//! prevents:
//!   * Salt-substitution attacks: an attacker who has a captured
//!     blob cannot replace the salt with one whose Argon2id
//!     output they know.
//!   * Cross-protocol replay: the domain tag binds these bytes
//!     to the backup-v1 use case; the same Argon2id+AEAD
//!     primitives used elsewhere in Lakko produce envelopes that
//!     do not interchange.

use lakko_crypto::aead::{self, Algorithm, NONCE_LEN};
use lakko_crypto::kdf;

use crate::error::{BackupError, Result};

/// Current schema version. Bumping this forces a fresh
/// Argon2id-params set + AAD-binding update.
pub const SCHEMA_VERSION: u8 = 1;

/// Length of the Argon2id salt in bytes. 16 bytes of CSPRNG —
/// matches the `lakko-crypto::kdf::argon2id_kek` recommendation
/// and is what the host crypto-stack tests already use.
pub const SALT_LEN: usize = 16;

/// Domain-separator prefix bound into the AEAD's associated data.
/// Distinct from every other AEAD use in Lakko so a captured
/// ciphertext cannot be replayed against a different decoder.
/// Trailing NUL keeps the prefix length distinct from any
/// incidental ASCII suffix the `lakko-backup/v…` namespace might
/// grow.
pub const ENVELOPE_DOMAIN_V1: &[u8] = b"lakko-backup/v1\x00";

/// Length of the four-byte big-endian ciphertext-length prefix.
const CT_LEN_FIELD: usize = 4;

/// Encrypt `payload_bytes` under a KEK derived from `phrase` and
/// a fresh CSPRNG salt + nonce. Returns the on-the-wire envelope.
///
/// `phrase` is the raw BIP39 recovery phrase bytes (UTF-8). The
/// caller normalises whitespace / case before passing it in;
/// this function treats the slice as opaque IKM into Argon2id.
///
/// # Errors
///
/// * [`BackupError::Crypto`] if Argon2id parameter validation
///   fails (cannot happen for the fixed-params API but the
///   wrapper surfaces it).
/// * [`BackupError::InvalidEncoding`] if `payload_bytes` is
///   larger than `u32::MAX - 16` (overflow on the length
///   prefix). In practice unreachable; the wire field tops out
///   at ~4 GiB and real backup payloads are kilobytes.
pub fn seal(phrase: &[u8], payload_bytes: &[u8]) -> Result<Vec<u8>> {
    use rand::RngCore as _;
    let mut rng = rand::rngs::OsRng;

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);

    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    seal_with(phrase, &salt, &nonce, payload_bytes)
}

/// Encrypt `payload_bytes` with the given `salt` and `nonce`.
/// Same shape as [`seal`] but the caller controls the random
/// inputs — used by tests for byte-identical determinism and
/// by future deterministic-encryption paths (none today).
///
/// Production callers should prefer [`seal`] which sources
/// fresh randomness from the CSPRNG.
///
/// # Errors
///
/// Same as [`seal`].
pub fn seal_with(
    phrase: &[u8],
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    payload_bytes: &[u8],
) -> Result<Vec<u8>> {
    let kek = kdf::argon2id_kek(phrase, salt)?;
    let aad = build_aad(SCHEMA_VERSION, salt, nonce);
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

    let mut out = Vec::with_capacity(1 + SALT_LEN + NONCE_LEN + CT_LEN_FIELD + ciphertext.len());
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct_len.to_be_bytes());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an on-the-wire envelope back to the payload bytes.
///
/// # Errors
///
/// * [`BackupError::InvalidEncoding`] — the wire bytes do not
///   form a valid envelope (wrong schema version, truncated,
///   length prefix overflows the buffer).
/// * [`BackupError::DecryptFailed`] — the AEAD `open` failed.
///   This folds "wrong recovery phrase" and "tampered blob" into
///   a single error: a status-code-style oracle that
///   distinguishes them leaks attacker-useful information.
/// * [`BackupError::Crypto`] — Argon2id parameter validation
///   failed (unreachable for the fixed-params API).
pub fn open(phrase: &[u8], wire: &[u8]) -> Result<Vec<u8>> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(BackupError::InvalidEncoding(format!(
            "unsupported backup schema version {version} (expected {SCHEMA_VERSION})",
        )));
    }

    let salt_bytes = take_n(&mut cur, SALT_LEN)?;
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(salt_bytes);

    let nonce_bytes = take_n(&mut cur, NONCE_LEN)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);

    let ct_len_bytes = take_n(&mut cur, CT_LEN_FIELD)?;
    let ct_len = u32::from_be_bytes(ct_len_bytes.try_into().expect("CT_LEN_FIELD == 4")) as usize;

    let ciphertext = take_n(&mut cur, ct_len)?;
    if !cur.is_empty() {
        return Err(BackupError::InvalidEncoding(format!(
            "{} trailing bytes after ciphertext",
            cur.len(),
        )));
    }

    let kek = kdf::argon2id_kek(phrase, &salt)?;
    let aad = build_aad(version, &salt, &nonce);
    aead::open(Algorithm::ChaCha20Poly1305, &kek, &nonce, &aad, ciphertext)
        .map_err(|_| BackupError::DecryptFailed)
}

/// Build the AEAD associated-data bytes. Pure deterministic — same
/// inputs produce same output, both `seal` and `open` reference
/// the same layout.
fn build_aad(version: u8, salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_DOMAIN_V1.len() + 1 + SALT_LEN + NONCE_LEN);
    out.extend_from_slice(ENVELOPE_DOMAIN_V1);
    out.push(version);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(BackupError::InvalidEncoding("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(BackupError::InvalidEncoding(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Happy-path round-trip — same phrase encrypts + decrypts a
    /// payload, recovered bytes match the input.
    #[test]
    fn seal_open_round_trip() {
        let phrase = b"correct horse battery staple recovery phrase 24 words";
        let payload = b"the inner backup bytes - could be anything".to_vec();

        let wire = seal(phrase, &payload).unwrap();
        let opened = open(phrase, &wire).unwrap();
        assert_eq!(opened, payload);
    }

    /// Two fresh seals of the same phrase + payload produce
    /// different ciphertexts (CSPRNG salt + nonce diverge).
    /// This is the property that prevents a server from
    /// observing "this user's state did not change today" via
    /// byte-equal re-uploads.
    #[test]
    fn fresh_seals_produce_distinct_wires() {
        let phrase = b"phrase";
        let payload = b"same payload twice";

        let a = seal(phrase, payload).unwrap();
        let b = seal(phrase, payload).unwrap();
        assert_ne!(a, b);
    }

    /// `seal_with` is byte-deterministic for fixed inputs. This
    /// is the testing seam — same `(phrase, salt, nonce, plain)`
    /// always produces the same wire bytes, so reference vectors
    /// stay stable across runs.
    #[test]
    fn seal_with_is_deterministic() {
        let phrase = b"phrase";
        let salt = [0x11u8; SALT_LEN];
        let nonce = [0x22u8; NONCE_LEN];
        let payload = b"deterministic";

        let a = seal_with(phrase, &salt, &nonce, payload).unwrap();
        let b = seal_with(phrase, &salt, &nonce, payload).unwrap();
        assert_eq!(a, b);
    }

    /// Wrong phrase fails to open. Folds into
    /// `DecryptFailed` — the same variant a tampered blob would
    /// surface, deliberately indistinguishable so an attacker
    /// cannot tell "I am close" from "I am wrong about the
    /// phrase but the blob is intact".
    #[test]
    fn wrong_phrase_fails_to_open() {
        let phrase = b"the right phrase";
        let wire = seal(phrase, b"payload").unwrap();

        let err = open(b"the wrong phrase", &wire).unwrap_err();
        assert!(matches!(err, BackupError::DecryptFailed));
    }

    /// Tampered ciphertext fails to open via AEAD tag check.
    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let phrase = b"phrase";
        let mut wire = seal(phrase, b"payload").unwrap();
        // Flip the last byte (part of the AEAD tag, or right at
        // its boundary). Either way the tag fails.
        let last = wire.len() - 1;
        wire[last] ^= 0x01;

        let err = open(phrase, &wire).unwrap_err();
        assert!(matches!(err, BackupError::DecryptFailed));
    }

    /// Tampered salt fails to open. The salt feeds Argon2id so
    /// a swap produces a different KEK; AEAD tag check fails.
    /// Also the AAD binds the salt, so even if Argon2id happens
    /// to produce a matching KEK by some pathological collision,
    /// the AAD mismatch still trips the tag.
    #[test]
    fn tampered_salt_fails_to_open() {
        let phrase = b"phrase";
        let mut wire = seal(phrase, b"payload").unwrap();
        // Byte 0 is the version; bytes 1..1+SALT_LEN are the salt.
        wire[1] ^= 0xFF;
        let err = open(phrase, &wire).unwrap_err();
        assert!(matches!(err, BackupError::DecryptFailed));
    }

    /// Tampered version byte produces an `InvalidEncoding` (the
    /// codec rejects before even reaching the AEAD step) when
    /// the new value isn't a known schema version.
    #[test]
    fn unknown_version_byte_rejected() {
        let phrase = b"phrase";
        let mut wire = seal(phrase, b"payload").unwrap();
        wire[0] = 99;
        let err = open(phrase, &wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidEncoding(_)));
    }

    /// Truncated wire bytes surface as `InvalidEncoding`.
    #[test]
    fn truncated_wire_rejected() {
        let phrase = b"phrase";
        let wire = seal(phrase, b"payload").unwrap();
        let truncated = &wire[..wire.len() - 5];
        let err = open(phrase, truncated).unwrap_err();
        assert!(matches!(err, BackupError::InvalidEncoding(_)));
    }

    /// Trailing bytes after the ciphertext surface as
    /// `InvalidEncoding` — the wire format is exact-length.
    #[test]
    fn trailing_bytes_rejected() {
        let phrase = b"phrase";
        let mut wire = seal(phrase, b"payload").unwrap();
        wire.push(0x00);
        let err = open(phrase, &wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidEncoding(_)));
    }

    /// Empty payload is allowed — degenerate case for callers
    /// that want to "clear" their backup state without removing
    /// the blob entirely.
    #[test]
    fn empty_payload_supported() {
        let phrase = b"phrase";
        let wire = seal(phrase, b"").unwrap();
        let opened = open(phrase, &wire).unwrap();
        assert!(opened.is_empty());
    }

    /// Fixed-overhead layout check — pin the offsets so a
    /// future refactor that drifts the wire shape surfaces here.
    #[test]
    fn wire_layout_offsets() {
        let phrase = b"phrase";
        let salt = [0xAAu8; SALT_LEN];
        let nonce = [0xBBu8; NONCE_LEN];
        let wire = seal_with(phrase, &salt, &nonce, b"x").unwrap();

        assert_eq!(wire[0], SCHEMA_VERSION);
        assert_eq!(&wire[1..=SALT_LEN], &salt[..]);
        let nonce_off = 1 + SALT_LEN;
        assert_eq!(&wire[nonce_off..nonce_off + NONCE_LEN], &nonce[..]);
        let ct_len_off = nonce_off + NONCE_LEN;
        // Ciphertext is 1 plaintext byte + 16-byte AEAD tag = 17.
        assert_eq!(
            u32::from_be_bytes(wire[ct_len_off..ct_len_off + 4].try_into().unwrap()),
            17,
        );
    }
}
