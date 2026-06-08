// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Identity supersession proof (G-4, security invariant #6, ADR-0007 §6).
//!
//! When an account's deterministic AccountIdentity rotates — e.g. a password
//! change rotates the OPAQUE export key, which rotates the identity key — the
//! holder of the **old** identity key signs a *supersession proof* that
//! authorises the **new** key as its successor. A peer that already trusts
//! the old key (from a prior TOFU pin) verifies the proof and bridges trust
//! to the new key WITHOUT a fresh TOFU bounce.
//!
//! The proof is published to the key-transparency log (a later sprint) so the
//! operator cannot equivocate — show one successor to one peer and a
//! different successor to another. This module is the proof primitive only.
//!
//! Invariant #6 ("identity rotation only via a KT-log supersession proof; old
//! keys do not come back") is enforced two ways here:
//!
//! - **one-directional** — the proof is signed by the *old* key over the
//!   *new* key, so only the legitimate prior key-holder can authorise a
//!   successor; a stranger cannot mint a transition.
//! - **time-bounded** — the validity window is capped at
//!   [`MAX_VALIDITY_SECS`] (30 days), so a leaked old key cannot
//!   indefinitely re-authorise arbitrary successors.
//!
//! Wire layout (all integers big-endian):
//!
//! ```text
//!   version(1) = 1
//!   old_pubkey(32)
//!   new_pubkey(32)
//!   issued_at_unix_secs(8, i64)
//!   expires_at_unix_secs(8, i64)
//!   signature(64)        // old key over: DOMAIN ‖ version ‖ old ‖ new ‖ issued ‖ expires
//! ```
//!
//! The signature covers every field, so tampering with any byte invalidates
//! it. [`verify_supersession`] is the strict entry point: signature (against
//! the *embedded* old key) → the embedded old key matches the caller's
//! trusted key → a sane validity window → freshness.
//!
//! **Note for the KT-log integration sprint (security review, G-4).** The
//! signature verifies via `lakko_crypto::signing` Ed25519, whose default
//! `verify` enforces scalar-malleability but NOT the group-element /
//! weak-key checks that only `verify_strict` does. That is harmless for the
//! standalone primitive — a malleated signature still authenticates the
//! *same* claims — but it means the signature **bytes are not unique** for a
//! given transition. When the proof is carried in a KT-log entry, any
//! dedup / identity / inclusion key MUST be derived from the **claim tuple**
//! `(old_pubkey, new_pubkey, issued_at, expires_at)`, never from the raw
//! signature bytes (otherwise an attacker holding one valid proof could mint
//! a second distinct-bytes proof for the same transition). Alternatively the
//! integration can switch to a strict verify. The KT entry must also tie
//! `old_pubkey` to the account's `username_hash` so the per-account pin this
//! module delegates to is itself non-equivocable.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::error::{Error, Result};
use crate::log::{check_no_rollback, verify_inclusion, InclusionProof, LogEntry, SignedRoot};

/// Domain separator mixed into the signing input.
const DOMAIN: &[u8] = b"lakko/identity-supersession/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of a big-endian `i64` timestamp.
const TS_LEN: usize = 8;

/// Total serialized supersession-proof length.
pub const SUPERSESSION_PROOF_LEN: usize =
    1 + PUBLIC_KEY_LEN + PUBLIC_KEY_LEN + TS_LEN + TS_LEN + SIGNATURE_LEN;

/// Maximum validity window for a supersession proof — 30 days. Bounds how
/// long a leaked old key can re-authorise a successor (invariant #6).
pub const MAX_VALIDITY_SECS: i64 = 30 * 24 * 60 * 60;

/// The authenticated claims carried by a supersession proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersessionProof {
    /// The retiring identity public key (the one peers already trust).
    pub old_pubkey: [u8; PUBLIC_KEY_LEN],
    /// The successor identity public key being authorised.
    pub new_pubkey: [u8; PUBLIC_KEY_LEN],
    /// When the proof was issued, unix seconds.
    pub issued_at_unix_secs: i64,
    /// Absolute expiry, unix seconds (`<= issued + 30 days`).
    pub expires_at_unix_secs: i64,
}

impl SupersessionProof {
    /// Construct proof claims to be signed by the old key.
    #[must_use]
    pub fn new(
        old_pubkey: [u8; PUBLIC_KEY_LEN],
        new_pubkey: [u8; PUBLIC_KEY_LEN],
        issued_at_unix_secs: i64,
        expires_at_unix_secs: i64,
    ) -> Self {
        Self {
            old_pubkey,
            new_pubkey,
            issued_at_unix_secs,
            expires_at_unix_secs,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + PUBLIC_KEY_LEN + PUBLIC_KEY_LEN + TS_LEN + TS_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.old_pubkey);
        buf.extend_from_slice(&self.new_pubkey);
        buf.extend_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        buf.extend_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims with the **old** identity signing key.
    ///
    /// The caller is responsible for `old_signing_key`'s public half equalling
    /// [`Self::old_pubkey`] (otherwise the proof will not verify) and for the
    /// validity window being sane — [`verify_supersession`] re-checks both.
    #[must_use]
    pub fn sign(&self, old_signing_key: &SigningKey) -> [u8; SUPERSESSION_PROOF_LEN] {
        let sig = old_signing_key.sign(&self.signing_input());
        let mut out = [0u8; SUPERSESSION_PROOF_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + PUBLIC_KEY_LEN].copy_from_slice(&self.old_pubkey);
        off += PUBLIC_KEY_LEN;
        out[off..off + PUBLIC_KEY_LEN].copy_from_slice(&self.new_pubkey);
        off += PUBLIC_KEY_LEN;
        out[off..off + TS_LEN].copy_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        off += TS_LEN;
        out[off..off + TS_LEN].copy_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        off += TS_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the signature against the **embedded**
    /// old public key. Returns the authenticated claims.
    ///
    /// This proves "the holder of `old_pubkey` signed this transition", but
    /// does NOT check that `old_pubkey` is the key the caller trusts, nor the
    /// validity window — use [`verify_supersession`] for the strict path.
    ///
    /// # Errors
    ///
    /// [`Error::SupersessionBadLength`] / [`Error::SupersessionBadVersion`] on
    /// a malformed envelope; [`Error::SupersessionBadSignature`] if the
    /// embedded key or the signature is malformed, or the signature does not
    /// verify.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SUPERSESSION_PROOF_LEN {
            return Err(Error::SupersessionBadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(Error::SupersessionBadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut old_pubkey = [0u8; PUBLIC_KEY_LEN];
        old_pubkey.copy_from_slice(&bytes[off..off + PUBLIC_KEY_LEN]);
        off += PUBLIC_KEY_LEN;
        let mut new_pubkey = [0u8; PUBLIC_KEY_LEN];
        new_pubkey.copy_from_slice(&bytes[off..off + PUBLIC_KEY_LEN]);
        off += PUBLIC_KEY_LEN;
        let mut iss = [0u8; TS_LEN];
        iss.copy_from_slice(&bytes[off..off + TS_LEN]);
        off += TS_LEN;
        let mut exp = [0u8; TS_LEN];
        exp.copy_from_slice(&bytes[off..off + TS_LEN]);
        off += TS_LEN;

        let claims = Self {
            old_pubkey,
            new_pubkey,
            issued_at_unix_secs: i64::from_be_bytes(iss),
            expires_at_unix_secs: i64::from_be_bytes(exp),
        };

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| Error::SupersessionBadSignature)?;
        let vk =
            VerifyingKey::from_bytes(&old_pubkey).map_err(|_| Error::SupersessionBadSignature)?;
        vk.verify(&claims.signing_input(), &sig)
            .map_err(|_| Error::SupersessionBadSignature)?;
        Ok(claims)
    }

    /// `true` when `now_unix_secs` is strictly past the expiry.
    #[must_use]
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        now_unix_secs > self.expires_at_unix_secs
    }

    /// `true` when the validity window is sane: `issued <= expires <=
    /// issued + 30 days` (using saturating arithmetic so a crafted
    /// near-`i64::MAX` issued time cannot overflow the bound check).
    #[must_use]
    pub fn validity_window_ok(&self) -> bool {
        self.expires_at_unix_secs >= self.issued_at_unix_secs
            && self.expires_at_unix_secs
                <= self.issued_at_unix_secs.saturating_add(MAX_VALIDITY_SECS)
    }
}

/// Strictly verify a supersession proof and return the authorised **new**
/// public key. The order is: signature (against the embedded old key) → the
/// embedded old key matches `trusted_old_pubkey` → a sane validity window →
/// freshness.
///
/// `trusted_old_pubkey` is the key the caller already pins for this account
/// (from a prior TOFU / KT-log inclusion). On success the caller may rotate
/// its pin to the returned new key.
///
/// # Errors
///
/// Propagates [`SupersessionProof::open`] errors, then
/// [`Error::SupersessionOldKeyMismatch`] /
/// [`Error::SupersessionInvalidValidityWindow`] /
/// [`Error::SupersessionExpired`] if the (now-authenticated) claims do not
/// bind to the trusted key, have an out-of-range window, or have lapsed.
pub fn verify_supersession(
    bytes: &[u8],
    trusted_old_pubkey: &[u8; PUBLIC_KEY_LEN],
    now_unix_secs: i64,
) -> Result<[u8; PUBLIC_KEY_LEN]> {
    let proof = SupersessionProof::open(bytes)?;
    if &proof.old_pubkey != trusted_old_pubkey {
        return Err(Error::SupersessionOldKeyMismatch);
    }
    if !proof.validity_window_ok() {
        return Err(Error::SupersessionInvalidValidityWindow);
    }
    if proof.is_expired(now_unix_secs) {
        return Err(Error::SupersessionExpired);
    }
    Ok(proof.new_pubkey)
}

/// The identity a peer should re-pin after a verified rotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotatedIdentity {
    /// The authorised successor public key.
    pub new_pubkey: [u8; PUBLIC_KEY_LEN],
    /// The log-entry version it was pinned at.
    pub version: u64,
}

/// What a peer currently pins for an account — its rotation trust anchor.
pub struct PinnedIdentity<'a> {
    /// BLAKE3 hash of the account's canonical username.
    pub username_hash: &'a [u8; 32],
    /// The identity key the peer currently trusts.
    pub old_pubkey: &'a [u8; PUBLIC_KEY_LEN],
    /// The KT-log version the peer pinned that key at.
    pub version: u64,
    /// The most recent signed root the peer has accepted (for the
    /// no-rollback check against the newly-presented root).
    pub last_seen_root: &'a SignedRoot,
}

/// The operator-served evidence for a single rotation.
pub struct RotationEvidence<'a> {
    /// The KT-log operator's verifying key (pinned out-of-band).
    pub operator: &'a VerifyingKey,
    /// The signed root the rotation entry is claimed to be under.
    pub new_root: &'a SignedRoot,
    /// The new log entry `(username_hash, new_pubkey, version)`.
    pub new_entry: &'a LogEntry,
    /// Its Merkle inclusion proof under `new_root`.
    pub inclusion: &'a InclusionProof,
    /// The wire-form [`SupersessionProof`] authorising the transition.
    pub supersession_proof: &'a [u8],
}

/// Verify a complete identity-rotation chain against the KT log (G-4,
/// invariant #6).
///
/// Given what the peer currently pins (`pinned`) and the operator's served
/// `evidence`, confirm a rotation by checking, in order:
///
/// 1. the operator actually signed `new_root`;
/// 2. no rollback vs the peer's last-seen root;
/// 3. the new entry is included under `new_root` (Merkle inclusion proof);
/// 4. the entry is for THIS account (its `username_hash` matches the pin);
/// 5. the entry's version is strictly newer than the pinned version;
/// 6. the supersession proof is signed by the pinned old key, fresh, within
///    the 30-day window, and authorises **exactly** the entry's new key.
///
/// On success the peer may re-pin to the returned [`RotatedIdentity`]. The
/// rotation is thus both **cryptographically authorised** (the old key signed
/// the successor) and **non-equivocable** (the entry is committed in the
/// append-only log under a signed root) — closing the blind-TOFU gap.
///
/// The entry's `username_hash` ties the proof to the account, so a proof
/// minted for one account cannot be presented for another (step 4 + step 6's
/// `old_pubkey == pinned` check). The Merkle leaf commits to the
/// `(username_hash, version, pubkey)` tuple, not the supersession signature
/// bytes, so the Ed25519 point-malleability of the signature cannot fork a
/// log entry.
///
/// # Errors
///
/// Propagates the per-step verification errors:
/// [`Error::SignedRootInvalid`] / [`Error::SignedRootRollback`] /
/// [`Error::InclusionProofMismatch`] /
/// [`Error::SupersessionEntryUsernameMismatch`] /
/// [`Error::SupersessionEntryVersionNotNewer`] / the
/// [`verify_supersession`] errors / [`Error::SupersessionEntryKeyMismatch`].
pub fn verify_supersession_chain(
    pinned: &PinnedIdentity,
    evidence: &RotationEvidence,
    now_unix_secs: i64,
) -> Result<RotatedIdentity> {
    // 1. The operator actually signed the newly-presented root.
    evidence.new_root.verify(evidence.operator)?;
    // 2. No rollback vs the last root the peer trusted.
    check_no_rollback(pinned.last_seen_root, evidence.new_root)?;
    // 3. The entry is included under the (now-verified) new root.
    verify_inclusion(evidence.new_entry, evidence.inclusion, evidence.new_root)?;
    // 4. The entry is for THIS account.
    if &evidence.new_entry.username_hash != pinned.username_hash {
        return Err(Error::SupersessionEntryUsernameMismatch);
    }
    // 5. Strictly newer version — no downgrade / replay of an old rotation.
    if evidence.new_entry.version <= pinned.version {
        return Err(Error::SupersessionEntryVersionNotNewer {
            entry: evidence.new_entry.version,
            pinned: pinned.version,
        });
    }
    // 6. The supersession proof authorises exactly this entry's new key.
    let authorised =
        verify_supersession(evidence.supersession_proof, pinned.old_pubkey, now_unix_secs)?;
    if evidence.new_entry.identity_pubkey.as_slice() != authorised.as_slice() {
        return Err(Error::SupersessionEntryKeyMismatch);
    }
    Ok(RotatedIdentity {
        new_pubkey: authorised,
        version: evidence.new_entry.version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample(old: &SigningKey) -> SupersessionProof {
        SupersessionProof::new(
            old.verifying_key().to_bytes(),
            [0xBB; PUBLIC_KEY_LEN],
            1_000,
            1_000 + MAX_VALIDITY_SECS,
        )
    }

    #[test]
    fn sign_then_verify_returns_new_key() {
        let old = old_key();
        let proof = sample(&old);
        let wire = proof.sign(&old);
        assert_eq!(wire.len(), SUPERSESSION_PROOF_LEN);
        let new = verify_supersession(&wire, &old.verifying_key().to_bytes(), 1_500)
            .expect("valid proof verifies");
        assert_eq!(new, [0xBB; PUBLIC_KEY_LEN]);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let old = old_key();
        let wire = sample(&old).sign(&old);
        for idx in [1usize, 40, 70, SUPERSESSION_PROOF_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert!(
                matches!(
                    SupersessionProof::open(&bad),
                    Err(Error::SupersessionBadSignature)
                ),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn old_key_must_match_the_trusted_pin() {
        let old = old_key();
        let wire = sample(&old).sign(&old);
        // A peer that trusts a DIFFERENT old key must reject it.
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key().to_bytes();
        assert!(matches!(
            verify_supersession(&wire, &other, 1_500),
            Err(Error::SupersessionOldKeyMismatch)
        ));
    }

    #[test]
    fn a_proof_signed_by_a_stranger_does_not_verify() {
        // An attacker forges old_pubkey = the victim's key, but cannot sign
        // for it. The signature check (against the embedded key) fails.
        let victim = old_key();
        let attacker = SigningKey::from_seed(&[0x99; 32]);
        let claims = SupersessionProof::new(
            victim.verifying_key().to_bytes(), // claim the victim's old key
            [0xCC; PUBLIC_KEY_LEN],
            1_000,
            1_000 + MAX_VALIDITY_SECS,
        );
        // ...but sign with the attacker's key.
        let wire = claims.sign(&attacker);
        assert!(matches!(
            SupersessionProof::open(&wire),
            Err(Error::SupersessionBadSignature)
        ));
    }

    #[test]
    fn expiry_is_enforced() {
        let old = old_key();
        let pk = old.verifying_key().to_bytes();
        let wire = sample(&old).sign(&old); // expires at 1000 + 30d
        assert!(verify_supersession(&wire, &pk, 1_000 + MAX_VALIDITY_SECS).is_ok());
        assert!(matches!(
            verify_supersession(&wire, &pk, 1_000 + MAX_VALIDITY_SECS + 1),
            Err(Error::SupersessionExpired)
        ));
    }

    #[test]
    fn validity_window_must_be_within_thirty_days() {
        let old = old_key();
        let pk = old.verifying_key().to_bytes();
        // expires more than 30 days after issued -> rejected.
        let too_long = SupersessionProof::new(pk, [0xBB; 32], 1_000, 1_000 + MAX_VALIDITY_SECS + 1)
            .sign(&old);
        assert!(matches!(
            verify_supersession(&too_long, &pk, 1_500),
            Err(Error::SupersessionInvalidValidityWindow)
        ));
        // expires before issued -> rejected.
        let inverted = SupersessionProof::new(pk, [0xBB; 32], 5_000, 4_000).sign(&old);
        assert!(matches!(
            verify_supersession(&inverted, &pk, 4_500),
            Err(Error::SupersessionInvalidValidityWindow)
        ));
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let old = old_key();
        assert!(matches!(
            SupersessionProof::open(&[0u8; 10]),
            Err(Error::SupersessionBadLength { got: 10 })
        ));
        let mut wire = sample(&old).sign(&old);
        wire[0] = 9;
        assert!(matches!(
            SupersessionProof::open(&wire),
            Err(Error::SupersessionBadVersion(9))
        ));
    }

    // ---- chain verification against the KT log (sprint 2) ----

    use crate::log::InMemoryLog;

    struct ChainFixture {
        operator_pub: VerifyingKey,
        username_hash: [u8; 32],
        old: SigningKey,
        old_pub: [u8; PUBLIC_KEY_LEN],
        new_pub: [u8; PUBLIC_KEY_LEN],
        last_seen: crate::log::SignedRoot,
        new_root: crate::log::SignedRoot,
        entry: LogEntry,
        inclusion: InclusionProof,
    }

    /// A two-entry KT log: genesis `(alice, old_pub, v1)` then a rotation
    /// `(alice, new_pub, v2)`, with a last-seen root taken after genesis and
    /// a current root + inclusion proof for the rotation entry.
    fn chain_fixture() -> ChainFixture {
        let mut log = InMemoryLog::new(SigningKey::from_seed(&[0x77; 32]));
        let operator_pub = log.operator_key();
        let username_hash = lakko_crypto::hash::blake3(b"alice");
        let old = SigningKey::from_seed(&[0x11; 32]);
        let old_pub = old.verifying_key().to_bytes();
        let new_pub = SigningKey::from_seed(&[0x22; 32]).verifying_key().to_bytes();

        log.append(LogEntry {
            username_hash,
            identity_pubkey: old_pub.to_vec(),
            version: 1,
        });
        let last_seen = log.sign_current_root(100);
        let idx = log.append(LogEntry {
            username_hash,
            identity_pubkey: new_pub.to_vec(),
            version: 2,
        });
        let new_root = log.sign_current_root(200);
        let inclusion = log.prove_inclusion(idx).expect("inclusion proof");
        let entry = log.entry(idx).expect("entry").clone();

        ChainFixture {
            operator_pub,
            username_hash,
            old,
            old_pub,
            new_pub,
            last_seen,
            new_root,
            entry,
            inclusion,
        }
    }

    fn proof_for(f: &ChainFixture, new_key: [u8; PUBLIC_KEY_LEN]) -> Vec<u8> {
        SupersessionProof::new(f.old_pub, new_key, 1_000, 1_000 + MAX_VALIDITY_SECS)
            .sign(&f.old)
            .to_vec()
    }

    #[test]
    fn chain_verifies_a_valid_rotation() {
        let f = chain_fixture();
        let proof = proof_for(&f, f.new_pub);
        let pinned = PinnedIdentity {
            username_hash: &f.username_hash,
            old_pubkey: &f.old_pub,
            version: 1,
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &f.operator_pub,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        let rotated =
            verify_supersession_chain(&pinned, &evidence, 1_500).expect("valid chain verifies");
        assert_eq!(rotated.new_pubkey, f.new_pub);
        assert_eq!(rotated.version, 2);
    }

    #[test]
    fn chain_rejects_an_entry_for_a_different_account() {
        let f = chain_fixture();
        let proof = proof_for(&f, f.new_pub);
        let other_account = lakko_crypto::hash::blake3(b"mallory");
        let pinned = PinnedIdentity {
            username_hash: &other_account, // peer is verifying a DIFFERENT account
            old_pubkey: &f.old_pub,
            version: 1,
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &f.operator_pub,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        assert!(matches!(
            verify_supersession_chain(&pinned, &evidence, 1_500),
            Err(Error::SupersessionEntryUsernameMismatch)
        ));
    }

    #[test]
    fn chain_rejects_a_non_newer_version() {
        let f = chain_fixture();
        let proof = proof_for(&f, f.new_pub);
        let pinned = PinnedIdentity {
            username_hash: &f.username_hash,
            old_pubkey: &f.old_pub,
            version: 2, // already at the entry's version — not a forward rotation
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &f.operator_pub,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        assert!(matches!(
            verify_supersession_chain(&pinned, &evidence, 1_500),
            Err(Error::SupersessionEntryVersionNotNewer { entry: 2, pinned: 2 })
        ));
    }

    #[test]
    fn chain_rejects_a_proof_authorising_a_different_key() {
        let f = chain_fixture();
        // A valid proof, but it authorises some OTHER key, not the one the
        // log entry actually rotates to.
        let proof = proof_for(&f, [0xEE; PUBLIC_KEY_LEN]);
        let pinned = PinnedIdentity {
            username_hash: &f.username_hash,
            old_pubkey: &f.old_pub,
            version: 1,
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &f.operator_pub,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        assert!(matches!(
            verify_supersession_chain(&pinned, &evidence, 1_500),
            Err(Error::SupersessionEntryKeyMismatch)
        ));
    }

    #[test]
    fn chain_rejects_an_expired_proof() {
        let f = chain_fixture();
        let proof = proof_for(&f, f.new_pub); // expires at 1000 + 30d
        let pinned = PinnedIdentity {
            username_hash: &f.username_hash,
            old_pubkey: &f.old_pub,
            version: 1,
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &f.operator_pub,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        assert!(matches!(
            verify_supersession_chain(&pinned, &evidence, 1_000 + MAX_VALIDITY_SECS + 1),
            Err(Error::SupersessionExpired)
        ));
    }

    #[test]
    fn chain_rejects_a_forged_root() {
        let f = chain_fixture();
        let proof = proof_for(&f, f.new_pub);
        // A peer that pins a DIFFERENT operator key rejects the root sig.
        let wrong_operator = SigningKey::from_seed(&[0xAA; 32]).verifying_key();
        let pinned = PinnedIdentity {
            username_hash: &f.username_hash,
            old_pubkey: &f.old_pub,
            version: 1,
            last_seen_root: &f.last_seen,
        };
        let evidence = RotationEvidence {
            operator: &wrong_operator,
            new_root: &f.new_root,
            new_entry: &f.entry,
            inclusion: &f.inclusion,
            supersession_proof: &proof,
        };
        assert!(matches!(
            verify_supersession_chain(&pinned, &evidence, 1_500),
            Err(Error::SignedRootInvalid)
        ));
    }
}
