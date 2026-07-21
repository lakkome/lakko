// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Backup-head commitment (G-11, [ADR-0037]).
//!
//! A *backup head* is a compact, domain-separated, Ed25519-signed commitment
//! that a given encrypted backup envelope is the account's **latest** — the one
//! carrying the highest auth-assigned monotonic `backup_version`. It closes the
//! fresh-install rollback residual: an attacker who controls the storage tier
//! can hand a restoring client an *older* backup blob, which opens cleanly under
//! the same recovery phrase, so nothing a client runs today authenticates "this
//! blob is the current head". The head does.
//!
//! Read the semantics plainly (ADR-0037 § "why a signature alone proves
//! nothing"): the signature proves the head is **authentic**, not that it is
//! **current**. Two mechanisms turn authenticity into a rollback defence:
//!
//! - a **monotonic `backup_version`** (auth-assigned, never client-supplied, so
//!   a regression is structurally impossible) that a returning client compares
//!   against a persisted floor — a hard floor, no window; and
//! - a **`signed_at`** mint timestamp against which a zero-anchor fresh install
//!   applies a client-side staleness window (the within-window replay residual
//!   is confined to that one case).
//!
//! The head is minted by `services/auth` (`POST /auth/backup/head`, stateful —
//! a per-account counter), verified by the restoring client, and never touches
//! the KT log — so, unlike [`GenesisAttestation`](crate::GenesisAttestation), it
//! binds **`account_id`** directly (the client, storage, and auth all speak
//! `account_id` natively at restore time). There is **no** operator-side /
//! transparency-gate verify here (the genesis-attestation split does not apply):
//! this primitive is auth-mint + client-verify only.
//!
//! The format deliberately mirrors [`GenesisAttestation`](crate::GenesisAttestation)
//! and [`ActionAttestation`](crate::ActionAttestation): a small fixed-layout
//! binary blob (not a JWT), so the verifier stays trivial to read, bounded, and
//! fuzzable, and pulls no JWS / JWKS-fetch dependency into the core. The trusted
//! verifying key is supplied by the caller (the client pins it as its
//! `DeviceAttestKey`, the same key it already pins for device / genesis
//! attestations — ADR-0037 §3 reuses the auth attest key across all three roles,
//! made safe by the distinct domain separator below).
//!
//! ```text
//! wire           = version(1) ‖ account_id(16) ‖ backup_version(8, big-endian u64)
//!                  ‖ envelope_hash(32) ‖ signed_at_unix_millis(8, big-endian i64)
//!                  ‖ Ed25519_sig(64)                                   = 129 bytes
//! signing input  = DOMAIN ‖ version ‖ account_id ‖ backup_version
//!                  ‖ envelope_hash ‖ signed_at
//! DOMAIN         = b"lakko/backup-head/v1"   (NEW — distinct from
//!                                             genesis-attest/v1 and device-attest/v1)
//! ```
//!
//! `version` is the blob-layout version (`= 1`), distinct from `backup_version`
//! (the auth-monotonic counter) — the two must not be conflated. `envelope_hash`
//! is `BLAKE3` over the envelope portion only (no self-reference over the head
//! prefix); the caller computes it (`lakko_backup::chain::envelope_hash`) and
//! passes it in. `signed_at` is the **mint timestamp** (not an expiry): the
//! staleness window is a client policy, not a server-fixed TTL, so the head
//! carries the raw mint time and the client applies its own window — a
//! deliberate deviation from the genesis template's `expires_at`.
//!
//! **Units: Unix milliseconds.** `signed_at`, `now`, and the staleness / skew
//! windows are all milliseconds (the clients — web / iOS / Android — work in
//! milliseconds natively). This differs from
//! [`GenesisAttestation`](crate::GenesisAttestation)'s `_unix_secs` fields; the
//! field and parameter names carry `_millis` so the unit is unambiguous and the
//! auth mint (S2) and clients (S5) cannot introduce a 1000× drift.
//!
//! Two verify entry points, mirroring the genesis freshness split:
//!
//! - [`verify_backup_head`] — signature + `account_id` binding + `envelope_hash`
//!   binding + **freshness**, returning the parsed [`BackupHead`] so the caller
//!   reads `backup_version` for its own monotonic-floor check (the floor needs
//!   client-persisted state, so it stays with the caller). This is the client's
//!   restore-path entry point.
//! - [`verify_backup_head_binding`] — signature + `account_id` binding only,
//!   **no** hash / freshness check, for a caller that applies its own freshness
//!   and floor policy (e.g. against the served envelope it has not yet hashed).
//!
//! [ADR-0037]: ../../../docs/adr/0037-backup-head-rollback-commitment.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from
/// [`ActionAttestation`](crate::ActionAttestation)'s `lakko/device-attest/v1`
/// and [`GenesisAttestation`](crate::GenesisAttestation)'s
/// `lakko/genesis-attest/v1`, so all three attestation kinds are
/// cryptographically non-interchangeable even under a shared signing key
/// (ADR-0037 §3). Bumping this string (and [`VERSION`]) invalidates every
/// previously-issued backup head.
const DOMAIN: &[u8] = b"lakko/backup-head/v1";

/// Wire-format (blob-layout) version. The verifier refuses any other value.
/// Distinct from the `backup_version` monotonic counter carried in the body.
const VERSION: u8 = 1;

/// Length of the account id (raw UUIDv7 bytes).
const ACCOUNT_LEN: usize = 16;

/// Length of the big-endian `u64` auth-monotonic backup version.
const BACKUP_VERSION_LEN: usize = 8;

/// Length of the `BLAKE3` envelope hash.
const ENVELOPE_HASH_LEN: usize = 32;

/// Length of the big-endian `i64` `signed_at` mint timestamp.
const SIGNED_AT_LEN: usize = 8;

/// Forward clock-skew tolerance (milliseconds) the freshness check allows: a
/// `signed_at` up to this far in the future of `now` is still accepted, so a
/// mildly fast auth-server clock relative to a restoring client does not flag a
/// genuinely-current head as stale. A `signed_at` beyond this future bound is
/// treated as stale (a nonsensical future timestamp is not a fresh head).
const MAX_CLOCK_SKEW_MILLIS: i64 = 5 * 60 * 1_000;

/// Total serialized backup-head length:
/// `version + account_id + backup_version + envelope_hash + signed_at + signature`.
pub const BACKUP_HEAD_LEN: usize =
    1 + ACCOUNT_LEN + BACKUP_VERSION_LEN + ENVELOPE_HASH_LEN + SIGNED_AT_LEN + SIGNATURE_LEN;

/// Why a backup head was rejected. Returned by [`BackupHead::open`],
/// [`verify_backup_head`], and [`verify_backup_head_binding`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupHeadError {
    /// The byte slice is not exactly [`BACKUP_HEAD_LEN`] long.
    #[error("backup head has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not [`VERSION`].
    #[error("unsupported backup head version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("backup head signature is invalid")]
    BadSignature,
    /// The committed account id does not match the expected one.
    #[error("backup head account id does not match the expected account")]
    AccountMismatch,
    /// The committed envelope hash does not match the served envelope's hash.
    #[error("backup head envelope hash does not match the served envelope")]
    EnvelopeHashMismatch,
    /// The head's `signed_at` falls outside the freshness window (older than
    /// `now - max_staleness`, or further in the future than the clock-skew
    /// tolerance).
    #[error("backup head is stale (signed_at outside the freshness window)")]
    Stale,
}

/// The authenticated commitment carried by a backup head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupHead {
    /// The account the head commits to (raw 16-byte id). Checked against the
    /// client's authenticated `account_id` as defence-in-depth against a head
    /// served for the wrong account.
    pub account_id: [u8; ACCOUNT_LEN],
    /// The auth-assigned monotonic backup version. Strictly increasing per
    /// account; a returning client refuses any head below its persisted floor.
    pub backup_version: u64,
    /// `BLAKE3` of the envelope portion of the backup blob. Binds the head to
    /// exactly the served envelope (a mismatch is a hard tamper signal).
    pub envelope_hash: [u8; ENVELOPE_HASH_LEN],
    /// The mint timestamp, Unix milliseconds. The client applies its own
    /// staleness window against this (it is not an expiry).
    pub signed_at_unix_millis: i64,
}

impl BackupHead {
    /// Construct a commitment to be signed.
    #[must_use]
    pub fn new(
        account_id: [u8; ACCOUNT_LEN],
        backup_version: u64,
        envelope_hash: [u8; ENVELOPE_HASH_LEN],
        signed_at_unix_millis: i64,
    ) -> Self {
        Self {
            account_id,
            backup_version,
            envelope_hash,
            signed_at_unix_millis,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + ACCOUNT_LEN + BACKUP_VERSION_LEN + ENVELOPE_HASH_LEN + SIGNED_AT_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.account_id);
        buf.extend_from_slice(&self.backup_version.to_be_bytes());
        buf.extend_from_slice(&self.envelope_hash);
        buf.extend_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        buf
    }

    /// Serialize + sign this commitment into the [`BACKUP_HEAD_LEN`]-byte wire
    /// form. Called by `services/auth` on `POST /auth/backup/head`.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; BACKUP_HEAD_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; BACKUP_HEAD_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + ACCOUNT_LEN].copy_from_slice(&self.account_id);
        off += ACCOUNT_LEN;
        out[off..off + BACKUP_VERSION_LEN].copy_from_slice(&self.backup_version.to_be_bytes());
        off += BACKUP_VERSION_LEN;
        out[off..off + ENVELOPE_HASH_LEN].copy_from_slice(&self.envelope_hash);
        off += ENVELOPE_HASH_LEN;
        out[off..off + SIGNED_AT_LEN].copy_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        off += SIGNED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against `verifying`.
    /// Returns the authenticated commitment.
    ///
    /// This does **not** check the account / envelope-hash binding or freshness
    /// — callers should use [`verify_backup_head`] (client restore path, full
    /// check) or [`verify_backup_head_binding`] (binding only).
    ///
    /// # Errors
    ///
    /// [`BackupHeadError::BadLength`] / [`BackupHeadError::BadVersion`] on a
    /// malformed envelope; [`BackupHeadError::BadSignature`] if the signature is
    /// malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, BackupHeadError> {
        if bytes.len() != BACKUP_HEAD_LEN {
            return Err(BackupHeadError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(BackupHeadError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut account_id = [0u8; ACCOUNT_LEN];
        account_id.copy_from_slice(&bytes[off..off + ACCOUNT_LEN]);
        off += ACCOUNT_LEN;
        let mut ver = [0u8; BACKUP_VERSION_LEN];
        ver.copy_from_slice(&bytes[off..off + BACKUP_VERSION_LEN]);
        off += BACKUP_VERSION_LEN;
        let backup_version = u64::from_be_bytes(ver);
        let mut envelope_hash = [0u8; ENVELOPE_HASH_LEN];
        envelope_hash.copy_from_slice(&bytes[off..off + ENVELOPE_HASH_LEN]);
        off += ENVELOPE_HASH_LEN;
        let mut signed_at = [0u8; SIGNED_AT_LEN];
        signed_at.copy_from_slice(&bytes[off..off + SIGNED_AT_LEN]);
        off += SIGNED_AT_LEN;
        let signed_at_unix_millis = i64::from_be_bytes(signed_at);

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| BackupHeadError::BadSignature)?;
        let head = Self {
            account_id,
            backup_version,
            envelope_hash,
            signed_at_unix_millis,
        };
        verifying
            .verify(&head.signing_input(), &sig)
            .map_err(|_| BackupHeadError::BadSignature)?;
        Ok(head)
    }

    /// `true` when `signed_at` lies within the freshness window: at or after
    /// `now - max_staleness`, and no further in the future than the clock-skew
    /// tolerance ([`MAX_CLOCK_SKEW_MILLIS`]). All arguments are Unix
    /// milliseconds. Uses saturating arithmetic so extreme `now` /
    /// `max_staleness` values cannot overflow.
    #[must_use]
    pub fn is_fresh(&self, now_unix_millis: i64, max_staleness_millis: i64) -> bool {
        let lower = now_unix_millis.saturating_sub(max_staleness_millis);
        let upper = now_unix_millis.saturating_add(MAX_CLOCK_SKEW_MILLIS);
        self.signed_at_unix_millis >= lower && self.signed_at_unix_millis <= upper
    }
}

/// Verify a backup head's signature, `account_id` binding, `envelope_hash`
/// binding, and freshness — the full check the **client** applies on restore.
/// Returns the parsed [`BackupHead`] so the caller reads `backup_version` for
/// its own monotonic-floor comparison against its persisted `last_known_version`
/// (the floor needs client-persisted state, so it stays with the caller;
/// ADR-0037 §6).
///
/// `now_unix_millis` and `max_staleness_millis` are Unix milliseconds. The
/// staleness window is a client policy tracking the auto-backup cadence.
///
/// # Errors
///
/// Propagates [`BackupHead::open`] errors, then
/// [`BackupHeadError::AccountMismatch`] /
/// [`BackupHeadError::EnvelopeHashMismatch`] if the (now-authenticated)
/// commitment does not match the served account / envelope, or
/// [`BackupHeadError::Stale`] if `signed_at` is outside the freshness window.
pub fn verify_backup_head(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account: &[u8; ACCOUNT_LEN],
    expected_envelope_hash: &[u8; ENVELOPE_HASH_LEN],
    now_unix_millis: i64,
    max_staleness_millis: i64,
) -> Result<BackupHead, BackupHeadError> {
    let head = BackupHead::open(bytes, verifying)?;
    if &head.account_id != expected_account {
        return Err(BackupHeadError::AccountMismatch);
    }
    if &head.envelope_hash != expected_envelope_hash {
        return Err(BackupHeadError::EnvelopeHashMismatch);
    }
    if !head.is_fresh(now_unix_millis, max_staleness_millis) {
        return Err(BackupHeadError::Stale);
    }
    Ok(head)
}

/// Verify a backup head's signature and `account_id` binding only — **no**
/// envelope-hash or freshness check. For a caller that applies its own
/// freshness and monotonic-floor policy (e.g. one that wants the parsed
/// `backup_version` before it has hashed the served envelope). Returns the
/// parsed [`BackupHead`].
///
/// # Errors
///
/// Propagates [`BackupHead::open`] errors, then
/// [`BackupHeadError::AccountMismatch`] if the committed account does not match
/// the expected one.
pub fn verify_backup_head_binding(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account: &[u8; ACCOUNT_LEN],
) -> Result<BackupHead, BackupHeadError> {
    let head = BackupHead::open(bytes, verifying)?;
    if &head.account_id != expected_account {
        return Err(BackupHeadError::AccountMismatch);
    }
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionAttestation, GenesisAttestation};

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample() -> BackupHead {
        BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000)
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let sk = key();
        let head = sample();
        let wire = head.sign(&sk);
        assert_eq!(wire.len(), BACKUP_HEAD_LEN);
        assert_eq!(BACKUP_HEAD_LEN, 129);
        let got = verify_backup_head(
            &wire,
            &sk.verifying_key(),
            &[0xAA; 16],
            &[0xBB; 32],
            1_000_000, // now == signed_at → fresh
            10_000,
        )
        .expect("valid backup head verifies");
        assert_eq!(got, head);
        assert_eq!(got.backup_version, 7);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // Flip one byte in each region (account, backup_version, envelope_hash,
        // signed_at, sig) and confirm the signature no longer verifies.
        for idx in [1usize, 18, 30, 60, BACKUP_HEAD_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert_eq!(
                BackupHead::open(&bad, &vk),
                Err(BackupHeadError::BadSignature),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            BackupHead::open(&wire, &other),
            Err(BackupHeadError::BadSignature)
        );
    }

    #[test]
    fn account_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        assert_eq!(
            verify_backup_head(&wire, &vk, &[0x00; 16], &[0xBB; 32], 1_000_000, 10_000),
            Err(BackupHeadError::AccountMismatch)
        );
        // The binding-only entry point enforces the same account check.
        assert_eq!(
            verify_backup_head_binding(&wire, &vk, &[0x00; 16]),
            Err(BackupHeadError::AccountMismatch)
        );
    }

    #[test]
    fn envelope_hash_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        assert_eq!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0x00; 32], 1_000_000, 10_000),
            Err(BackupHeadError::EnvelopeHashMismatch)
        );
    }

    #[test]
    fn distinct_envelope_hashes_do_not_cross_verify() {
        // A head minted committing envelope-hash X must not satisfy a check for
        // envelope-hash Y — the core property that binds a head to its blob.
        let sk = key();
        let vk = sk.verifying_key();
        let wire_x = BackupHead::new([0xAA; 16], 7, [0x01; 32], 1_000_000).sign(&sk);
        assert_eq!(
            verify_backup_head(&wire_x, &vk, &[0xAA; 16], &[0x02; 32], 1_000_000, 10_000),
            Err(BackupHeadError::EnvelopeHashMismatch)
        );
    }

    #[test]
    fn stale_head_is_rejected() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        // now far past signed_at + window → below the lower bound → Stale.
        assert_eq!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 2_000_000, 10_000),
            Err(BackupHeadError::Stale)
        );
    }

    #[test]
    fn fresh_head_within_window_is_accepted() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        // now = signed_at + 5_000, window = 10_000 → lower = now - 10_000 =
        // 995_000 <= 1_000_000 → fresh.
        assert!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 1_005_000, 10_000).is_ok(),
            "a head within the staleness window must verify"
        );
        // Exactly at the lower bound (signed_at == now - max_staleness) is fresh.
        assert!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 1_010_000, 10_000).is_ok(),
            "the lower window bound is inclusive"
        );
    }

    #[test]
    fn future_skew_is_handled() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        // signed_at slightly in the future of now, within MAX_CLOCK_SKEW_MILLIS
        // (5 min = 300_000 ms) → accepted (tolerates a fast auth clock).
        assert!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 999_000, 10_000).is_ok(),
            "a head within the forward clock-skew tolerance must verify"
        );
        // signed_at far in the future of now, beyond the skew tolerance →
        // upper = now + 300_000 = 900_000 < 1_000_000 → Stale.
        assert_eq!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 600_000, 10_000),
            Err(BackupHeadError::Stale),
            "a head beyond the forward skew tolerance is stale"
        );
    }

    #[test]
    fn binding_entry_point_ignores_freshness() {
        // `verify_backup_head_binding` performs no freshness check — a head that
        // `verify_backup_head` would reject as Stale still passes the binding
        // check (the caller applies its own freshness/floor policy).
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        assert_eq!(
            verify_backup_head(&wire, &vk, &[0xAA; 16], &[0xBB; 32], 9_000_000, 10_000),
            Err(BackupHeadError::Stale)
        );
        let got = verify_backup_head_binding(&wire, &vk, &[0xAA; 16])
            .expect("binding check ignores freshness");
        assert_eq!(got.backup_version, 7);
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            BackupHead::open(&[0u8; 10], &vk),
            Err(BackupHeadError::BadLength { got: 10 })
        );
        // Truncated by one byte.
        assert_eq!(
            BackupHead::open(&[0u8; BACKUP_HEAD_LEN - 1], &vk),
            Err(BackupHeadError::BadLength {
                got: BACKUP_HEAD_LEN - 1
            })
        );
        // One byte too long.
        assert_eq!(
            BackupHead::open(&[0u8; BACKUP_HEAD_LEN + 1], &vk),
            Err(BackupHeadError::BadLength {
                got: BACKUP_HEAD_LEN + 1
            })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 9; // bad version
        assert_eq!(
            BackupHead::open(&wire, &vk),
            Err(BackupHeadError::BadVersion(9))
        );
    }

    #[test]
    fn domains_do_not_cross_between_all_three_attestation_kinds() {
        // Domain separation is load-bearing: even under a SHARED signing key
        // (ADR-0037 §3), a `BackupHead`, a `GenesisAttestation`, and an
        // `ActionAttestation` must be cryptographically non-interchangeable. Two
        // mechanisms enforce it — (a) distinct total lengths, and (b) distinct
        // domain-prefixed signing inputs — and this proves BOTH, three-way.
        let sk = key();
        let vk = sk.verifying_key();

        let backup_wire = sample().sign(&sk); // 129 bytes
        let genesis_wire =
            GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk); // 153 bytes
        let action_wire = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk); // 121 bytes

        // (a) Length: no parser accepts another kind's wire blob.
        assert!(
            matches!(
                BackupHead::open(&genesis_wire, &vk),
                Err(BackupHeadError::BadLength { .. })
            ),
            "a genesis blob must not parse as a backup head"
        );
        assert!(
            matches!(
                BackupHead::open(&action_wire, &vk),
                Err(BackupHeadError::BadLength { .. })
            ),
            "an action blob must not parse as a backup head"
        );
        assert!(
            matches!(
                GenesisAttestation::open(&backup_wire, &vk),
                Err(crate::GenesisAttestError::BadLength { .. })
            ),
            "a backup-head blob must not parse as a genesis attestation"
        );
        assert!(
            matches!(
                ActionAttestation::open(&backup_wire, &vk),
                Err(crate::AttestError::BadLength { .. })
            ),
            "a backup-head blob must not parse as an action attestation"
        );

        // (b) Domain: splice a foreign 64-byte signature into a backup-shaped
        // buffer. Each foreign signature was produced over a different
        // domain-prefixed signing input, so it must NOT verify under the
        // backup-head (`lakko/backup-head/v1`) signing input — proving the
        // domain prefix, not merely the length, keeps the signatures apart.
        let genesis_sig = &genesis_wire[genesis_wire.len() - SIGNATURE_LEN..];
        let mut spliced_genesis = sample().sign(&sk);
        spliced_genesis[BACKUP_HEAD_LEN - SIGNATURE_LEN..].copy_from_slice(genesis_sig);
        assert_eq!(
            BackupHead::open(&spliced_genesis, &vk),
            Err(BackupHeadError::BadSignature),
            "a genesis-domain signature must not verify as a backup-head signature"
        );

        let action_sig = &action_wire[action_wire.len() - SIGNATURE_LEN..];
        let mut spliced_action = sample().sign(&sk);
        spliced_action[BACKUP_HEAD_LEN - SIGNATURE_LEN..].copy_from_slice(action_sig);
        assert_eq!(
            BackupHead::open(&spliced_action, &vk),
            Err(BackupHeadError::BadSignature),
            "an action-domain signature must not verify as a backup-head signature"
        );

        // And the reverse: a backup-head signature spliced into a genesis-shaped
        // buffer must not verify under the genesis signing input.
        let backup_sig = &backup_wire[backup_wire.len() - SIGNATURE_LEN..];
        let mut genesis_spliced =
            GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk);
        let genesis_len = genesis_spliced.len();
        genesis_spliced[genesis_len - SIGNATURE_LEN..].copy_from_slice(backup_sig);
        assert_eq!(
            GenesisAttestation::open(&genesis_spliced, &vk),
            Err(crate::GenesisAttestError::BadSignature),
            "a backup-head-domain signature must not verify as a genesis signature"
        );
    }
}
