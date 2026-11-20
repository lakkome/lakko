// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Witness co-signing of KT-log roots (ADR-0020).
//!
//! The KT log is operator-signed + append-only. The G-30 #2 chain verifier
//! (ADR-0019) makes a *leaked-old-key* equivocation detectable, but a malicious
//! **operator** — it holds the root signing key — can still sign two divergent
//! valid roots and show each to a different victim (a **split-view** / fork).
//! Each client's local no-rollback check passes, so the fork is invisible.
//!
//! ADR-0020's defence is an independent **witness** that co-signs every root,
//! but only after verifying (via a Merkle consistency proof, see
//! [`verify_consistency`](crate::verify_consistency)) that the new root
//! append-only-extends its last-co-signed root. A client requires BOTH the
//! operator signature AND a valid witness co-signature before trusting a root.
//! To fork, the operator would need the witness to co-sign two mutually-
//! inconsistent roots — which the honest witness refuses, since two forks
//! cannot both extend its single last-co-signed root.
//!
//! This module is the co-signature primitive: a compact, self-describing wire
//! blob the witness signs and a client verifies. It is domain-separated from
//! the operator's root signature and from every other Lakko Ed25519 use, so a
//! signature of one kind can never verify as another.
//!
//! ADR-0020 shipped with a single pinned witness — collusion of the operator
//! with that one witness re-opens the fork. [`verify_witness_quorum`] is the
//! K-of-N generalisation (ADR-0039, G-30 finding-1): the client pins `N`
//! witness keys and requires at least `K` DISTINCT witnesses to have co-signed
//! the same root, so the operator must now corrupt `K` independent witnesses,
//! not one.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

use crate::error::{Error, Result};
use crate::merkle::TreeHash;

/// Domain separator for the witness co-signature. Distinct from the operator's
/// root signing payload (which is un-prefixed `tree_root ‖ log_size ‖
/// timestamp`) and from the supersession / leaf-genesis domains.
///
/// Version 2 (ADR-0098 section 5) binds the name of the log being co-signed,
/// so one witness key can serve several logs and a co-signature for one log
/// never verifies for another. Version 1 (no log name) is refused; there is
/// no dual format.
const WITNESS_DOMAIN: &[u8] = b"lakko/transparency/witness/v2";

/// Co-signature wire-format version.
const VERSION: u8 = 2;

/// Wire length of a witness co-signature:
/// `version(1) ‖ tree_root(32) ‖ log_size(8 BE) ‖ signature(64)`.
pub const WITNESS_COSIG_LEN: usize = 1 + 32 + 8 + SIGNATURE_LEN;

/// The append-only logs a witness co-signs (ADR-0098 section 5). A closed set:
/// the log's name is part of every co-signature, so adding a log is a code
/// change, never a configuration value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LogId {
    /// The key-transparency log (`services/transparency`).
    Kt = 1,
    /// The moderation log (`services/moderation`).
    Moderation = 2,
}

impl LogId {
    /// Every log, in wire-byte order.
    pub const ALL: [Self; 2] = [Self::Kt, Self::Moderation];

    /// The name bound into the co-signature's signing input.
    #[must_use]
    pub const fn name(self) -> &'static [u8] {
        match self {
            Self::Kt => b"lakko/kt-log/v1",
            Self::Moderation => b"lakko/moderation-log/v1",
        }
    }

    /// The value used for this log in JSON request bodies (`"kt"`,
    /// `"moderation"`).
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Kt => "kt",
            Self::Moderation => "moderation",
        }
    }

    /// Parse the JSON value; `None` for anything else.
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|log| log.wire_name() == name)
    }

    /// The one-byte identifier (1 = KT, 2 = moderation), used as a database
    /// key by the witness.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }
}

/// The signing pre-image a witness co-signs:
/// `DOMAIN ‖ version ‖ name_len(1) ‖ log name ‖ tree_root(32) ‖ log_size(8 BE)`.
///
/// It binds the log and the `(tree_root, log_size)` that uniquely identifies a
/// tree state (two forks differ in `tree_root` at the same `log_size`). The
/// operator's own timestamp is NOT covered — the witness attests the tree
/// state, not the operator's clock.
fn cosig_signing_input(log: LogId, tree_root: &TreeHash, log_size: u64) -> Vec<u8> {
    let name = log.name();
    let mut payload = Vec::with_capacity(WITNESS_DOMAIN.len() + 2 + name.len() + 32 + 8);
    payload.extend_from_slice(WITNESS_DOMAIN);
    payload.push(VERSION);
    // Names are fixed constants well under 256 bytes.
    #[allow(clippy::cast_possible_truncation)]
    payload.push(name.len() as u8);
    payload.extend_from_slice(name);
    payload.extend_from_slice(tree_root);
    payload.extend_from_slice(&log_size.to_be_bytes());
    payload
}

/// Produce a witness co-signature over a root's `(tree_root, log_size)`.
///
/// The witness MUST have verified the root's consistency with its last-co-signed
/// root BEFORE calling this (that is the split-view defence — see the module
/// docs); this primitive only mints the signature. Returns the self-describing
/// [`WITNESS_COSIG_LEN`]-byte wire blob the operator serves to clients.
#[must_use]
pub fn sign_witness_cosig(
    witness_key: &SigningKey,
    log: LogId,
    tree_root: &TreeHash,
    log_size: u64,
) -> [u8; WITNESS_COSIG_LEN] {
    let sig = witness_key.sign(&cosig_signing_input(log, tree_root, log_size));
    let mut out = [0u8; WITNESS_COSIG_LEN];
    out[0] = VERSION;
    let mut off = 1;
    out[off..off + 32].copy_from_slice(tree_root);
    off += 32;
    out[off..off + 8].copy_from_slice(&log_size.to_be_bytes());
    off += 8;
    out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
    out
}

/// Verify a witness co-signature against the pinned `witness_key`, binding it to
/// the `log` and the `(expected_tree_root, expected_log_size)` the caller is
/// verifying. The log is not in the blob; it enters the signature check, so a
/// co-signature for another log fails as [`Error::WitnessCosigInvalid`].
///
/// The blob is self-describing; this checks (in order) the length, the version,
/// that its embedded `(tree_root, log_size)` matches what the caller expects (so
/// a co-sig minted for a different root is rejected before the signature check),
/// and finally the Ed25519 signature against `witness_key`.
///
/// # Errors
///
/// - [`Error::WitnessCosigBadLength`] if `bytes` is not [`WITNESS_COSIG_LEN`].
/// - [`Error::WitnessCosigBadVersion`] on an unknown version byte.
/// - [`Error::WitnessCosigBindingMismatch`] if the embedded root differs from
///   the expected one.
/// - [`Error::WitnessCosigInvalid`] if `witness_key` is not a valid Ed25519 key
///   or the signature does not verify.
pub fn verify_witness_cosig(
    bytes: &[u8],
    witness_key: &[u8],
    log: LogId,
    expected_tree_root: &TreeHash,
    expected_log_size: u64,
) -> Result<()> {
    if bytes.len() != WITNESS_COSIG_LEN {
        return Err(Error::WitnessCosigBadLength { got: bytes.len() });
    }
    if bytes[0] != VERSION {
        return Err(Error::WitnessCosigBadVersion(bytes[0]));
    }

    // Fixed offsets; the length check above guarantees these slices exist. The
    // `map_err` keeps the crate `expect`-free (the conversion cannot actually
    // fail on a length-checked 32-byte slice).
    let embedded_root: TreeHash = bytes[1..33]
        .try_into()
        .map_err(|_| Error::WitnessCosigBadLength { got: bytes.len() })?;
    let size_bytes: [u8; 8] = bytes[33..41]
        .try_into()
        .map_err(|_| Error::WitnessCosigBadLength { got: bytes.len() })?;
    let embedded_size = u64::from_be_bytes(size_bytes);
    let sig_bytes = &bytes[41..WITNESS_COSIG_LEN];

    if &embedded_root != expected_tree_root || embedded_size != expected_log_size {
        return Err(Error::WitnessCosigBindingMismatch);
    }

    let vk = VerifyingKey::from_bytes(witness_key).map_err(|_| Error::WitnessCosigInvalid)?;
    let signature = Signature::from_bytes(sig_bytes).map_err(|_| Error::WitnessCosigInvalid)?;
    vk.verify(
        &cosig_signing_input(log, &embedded_root, embedded_size),
        &signature,
    )
    .map_err(|_| Error::WitnessCosigInvalid)
}

/// Verify that a **quorum** of `threshold` DISTINCT pinned witnesses each
/// co-signed the `(expected_tree_root, expected_log_size)` (ADR-0039, G-30
/// finding-1).
///
/// This is the K-of-N generalisation of [`verify_witness_cosig`]: instead of a
/// single pinned witness, the caller pins `N` witness keys (`pinned_keys`) and
/// requires at least `threshold` (`K`) of them to have co-signed the SAME root.
/// Requiring `K` independent witnesses raises the split-view bar from "one
/// honest witness" to "at least `N − K + 1` of the `N` are honest", so the
/// operator can no longer fork the log by colluding with (or compromising) a
/// single witness.
///
/// # Dedup-by-distinct-key (the security crux)
///
/// The tally counts **distinct pinned keys**, never co-signatures. Each pinned
/// key contributes AT MOST ONE to the quorum, no matter how many of the
/// supplied `cosigs` verify under it, and a duplicated entry in `pinned_keys` is
/// likewise counted once. This is load-bearing: `cosigs` is attacker-controlled
/// (a malicious operator serves them to the client), so without per-key dedup an
/// attacker could reach any threshold by replaying one honest witness's
/// co-signature `K` times, or by presenting `K` distinct-but-valid signatures
/// all produced by a single witness. Counting distinct KEYS — not signatures —
/// defeats both: `K` genuinely different pinned witnesses must each have signed
/// the root.
///
/// # Fail-closed on a zero threshold
///
/// A `threshold` of `0` is a misconfiguration — a zero quorum would accept ANY
/// root, including one no witness signed. It is rejected fail-closed with
/// [`Error::WitnessQuorumBadThreshold`] BEFORE any co-signature is examined, so
/// a config bug can never silently disable the split-view defence
/// (invariant #4).
///
/// Each candidate co-signature is checked with [`verify_witness_cosig`], so its
/// length / version / `(tree_root, log_size)` binding / signature gates all
/// apply — a co-sig bound to a different root, signed by a non-pinned key, or
/// malformed simply does not count toward the quorum (it is never fatal). The
/// work is bounded by `pinned_keys.len() * cosigs.len()` signature checks, both
/// caller-supplied; nothing is allocated proportional to attacker input beyond
/// the caller's own slices.
///
/// # Errors
///
/// - [`Error::WitnessQuorumBadThreshold`] if `threshold == 0`.
/// - [`Error::WitnessQuorumNotMet`] if fewer than `threshold` distinct pinned
///   keys produced a valid co-signature over the expected root; it carries the
///   achieved and required counts.
pub fn verify_witness_quorum(
    cosigs: &[&[u8]],
    pinned_keys: &[&[u8]],
    threshold: usize,
    log: LogId,
    expected_tree_root: &TreeHash,
    expected_log_size: u64,
) -> Result<()> {
    if threshold == 0 {
        return Err(Error::WitnessQuorumBadThreshold);
    }

    // Distinct pinned keys that produced at least one valid co-signature over
    // the expected root. Storing the keys (not merely a count) lets us skip a
    // duplicate pinned-key entry so it can never be double-counted — the
    // dedup-by-distinct-key property.
    let mut satisfied: Vec<&[u8]> = Vec::new();
    for &key in pinned_keys {
        if satisfied.contains(&key) {
            // This exact key already contributed; a duplicate entry adds nothing.
            continue;
        }
        let key_has_valid_cosig = cosigs.iter().any(|&cosig| {
            verify_witness_cosig(cosig, key, log, expected_tree_root, expected_log_size).is_ok()
        });
        if key_has_valid_cosig {
            satisfied.push(key);
            if satisfied.len() >= threshold {
                return Ok(());
            }
        }
    }

    Err(Error::WitnessQuorumNotMet {
        achieved: satisfied.len(),
        required: threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_at(seed: u8) -> TreeHash {
        lakko_crypto::hash::blake3(&[seed; 16])
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        assert_eq!(cosig.len(), WITNESS_COSIG_LEN);
        verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 42).expect("witness co-sig verifies");
    }

    #[test]
    fn wrong_witness_key_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let other = SigningKey::from_seed(&[0x44; 32])
            .verifying_key()
            .to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        assert!(matches!(
            verify_witness_cosig(&cosig, &other, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigInvalid)
        ));
    }

    #[test]
    fn binding_mismatch_wrong_root_or_size_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        // A co-sig spliced onto a DIFFERENT root.
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &root_at(0xCD), 42),
            Err(Error::WitnessCosigBindingMismatch)
        ));
        // Same root, DIFFERENT size — a fork at a different point.
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 43),
            Err(Error::WitnessCosigBindingMismatch)
        ));
    }

    #[test]
    fn tampered_signature_byte_fails() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        cosig[WITNESS_COSIG_LEN - 1] ^= 0x01;
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigInvalid)
        ));
    }

    #[test]
    fn tampered_embedded_root_fails_binding() {
        // Flipping a byte in the embedded root changes both the binding (so it
        // no longer matches the expected root) — caught at the binding gate.
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        cosig[1] ^= 0x01; // first byte of the embedded tree_root
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigBindingMismatch)
        ));
    }

    #[test]
    fn wrong_length_rejected() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        assert!(matches!(
            verify_witness_cosig(&cosig[..WITNESS_COSIG_LEN - 1], &vk, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigBadLength { .. })
        ));
    }

    #[test]
    fn unknown_version_rejected() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        cosig[0] = 0xFF;
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigBadVersion(0xFF))
        ));
    }

    #[test]
    fn distinct_domain_from_operator_root_signature() {
        // A witness co-sig's signing pre-image is domain-prefixed, so the same
        // (tree_root, log_size) bytes the OPERATOR signs un-prefixed produce a
        // different message — the two signature kinds never cross-validate.
        let tr = root_at(0xAB);
        let input = cosig_signing_input(LogId::Kt, &tr, 42);
        assert!(input.starts_with(WITNESS_DOMAIN));
        assert_ne!(input.len(), 32 + 8); // not the operator's bare payload
    }

    // --- K-of-N witness quorum (ADR-0039, G-30 finding-1) ---

    /// Mint N witnesses from the given seeds; return their signing keys and the
    /// matching 32-byte verifying-key bytes (the pinned-key form).
    fn witnesses(seeds: &[u8]) -> (Vec<SigningKey>, Vec<[u8; 32]>) {
        let sks: Vec<SigningKey> = seeds
            .iter()
            .map(|s| SigningKey::from_seed(&[*s; 32]))
            .collect();
        let vks: Vec<[u8; 32]> = sks.iter().map(|sk| sk.verifying_key().to_bytes()).collect();
        (sks, vks)
    }

    fn as_refs<T: AsRef<[u8]>>(items: &[T]) -> Vec<&[u8]> {
        items.iter().map(AsRef::as_ref).collect()
    }

    #[test]
    fn quorum_met_with_k_distinct_valid() {
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[1, 2, 3]);
        let cosigs: Vec<[u8; WITNESS_COSIG_LEN]> = sks
            .iter()
            .map(|sk| sign_witness_cosig(sk, LogId::Kt, &tr, 7))
            .collect();
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        // 3-of-3 and 2-of-3 both satisfied by three distinct valid co-sigs.
        verify_witness_quorum(&cosig_refs, &key_refs, 3, LogId::Kt, &tr, 7).expect("3-of-3 met");
        verify_witness_quorum(&cosig_refs, &key_refs, 2, LogId::Kt, &tr, 7).expect("2-of-3 met");
    }

    #[test]
    fn quorum_not_met_with_k_minus_one_distinct() {
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[1, 2, 3]);
        // Only two of the three pinned witnesses actually co-signed.
        let cosigs = [
            sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7),
            sign_witness_cosig(&sks[1], LogId::Kt, &tr, 7),
        ];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 3, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 2,
                required: 3
            })
        ));
    }

    #[test]
    fn duplicate_cosig_under_one_key_counts_once() {
        // The anti-double-count crux: the SAME key's valid co-sig supplied
        // twice (2 cosigs, 1 pinned key) can only ever tally 1 — a replayed
        // co-sig cannot inflate the quorum to 2.
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[9]);
        let c = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7);
        let cosigs = [c, c]; // same witness's co-sig, twice
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 2, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 1,
                required: 2
            })
        ));
        // The same single distinct key does satisfy a threshold of 1.
        verify_witness_quorum(&cosig_refs, &key_refs, 1, LogId::Kt, &tr, 7).expect("1-of-1 met");
    }

    #[test]
    fn duplicate_pinned_key_counts_once() {
        // A key listed twice in `pinned_keys` (a config duplicate) still counts
        // once — distinctness is over keys, not slots.
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[9]);
        let c = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7);
        let cosigs = [c];
        let cosig_refs = as_refs(&cosigs);
        let key = vks[0];
        let pinned: [&[u8]; 2] = [&key, &key]; // same key twice
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &pinned, 2, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 1,
                required: 2
            })
        ));
    }

    #[test]
    fn garbage_cosig_among_valid_is_ignored() {
        // A malformed / bad co-sig does not block an otherwise-met quorum.
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[1, 2, 3]);
        let c1 = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7);
        let c2 = sign_witness_cosig(&sks[1], LogId::Kt, &tr, 7);
        let c3 = sign_witness_cosig(&sks[2], LogId::Kt, &tr, 7);
        let garbage = [0xAAu8; WITNESS_COSIG_LEN];
        let short = [0x00u8; 3];
        let cosigs: [&[u8]; 5] = [&garbage, &c1, &short, &c2, &c3];
        let key_refs = as_refs(&vks);
        verify_witness_quorum(&cosigs, &key_refs, 3, LogId::Kt, &tr, 7)
            .expect("3 distinct valid despite two junk co-sigs");
    }

    #[test]
    fn cosig_over_different_root_does_not_count() {
        // A valid co-sig bound to a DIFFERENT (tree_root, log_size) fails the
        // binding gate inside verify_witness_cosig, so it never counts.
        let tr = root_at(0xAB);
        let other = root_at(0xCD);
        let (sks, vks) = witnesses(&[1]);
        let c = sign_witness_cosig(&sks[0], LogId::Kt, &other, 7); // signed over `other`
        let cosigs = [c];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 1, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 0,
                required: 1
            })
        ));
        // Same witness, right root but a different log_size — also excluded.
        let c_size = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 8);
        let cosigs2 = [c_size];
        let cosig_refs2 = as_refs(&cosigs2);
        assert!(matches!(
            verify_witness_quorum(&cosig_refs2, &key_refs, 1, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet { achieved: 0, .. })
        ));
    }

    #[test]
    fn cosig_from_non_pinned_key_does_not_count() {
        let tr = root_at(0xAB);
        let (_pinned_sks, vks) = witnesses(&[1]); // pin witness 1
        let intruder = SigningKey::from_seed(&[0x77; 32]); // not pinned
        let c = sign_witness_cosig(&intruder, LogId::Kt, &tr, 7);
        let cosigs = [c];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 1, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 0,
                required: 1
            })
        ));
    }

    #[test]
    fn zero_threshold_fails_closed() {
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[1]);
        let c = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7);
        let cosigs = [c];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        // A zero quorum must NOT trivially pass even with a valid co-sig present.
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 0, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumBadThreshold)
        ));
    }

    #[test]
    fn threshold_above_pinned_count_is_unsatisfiable() {
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[1, 2]);
        let cosigs = [
            sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7),
            sign_witness_cosig(&sks[1], LogId::Kt, &tr, 7),
        ];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        // Only two pinned keys exist; a threshold of 3 can never be reached.
        assert!(matches!(
            verify_witness_quorum(&cosig_refs, &key_refs, 3, LogId::Kt, &tr, 7),
            Err(Error::WitnessQuorumNotMet {
                achieved: 2,
                required: 3
            })
        ));
    }

    #[test]
    fn one_of_one_matches_single_witness_path() {
        // Backward-compat: N=1, K=1 accepts exactly when verify_witness_cosig
        // would, proving the quorum generalises the single-witness primitive.
        let tr = root_at(0xAB);
        let (sks, vks) = witnesses(&[5]);
        let c = sign_witness_cosig(&sks[0], LogId::Kt, &tr, 7);
        let cosigs = [c];
        let cosig_refs = as_refs(&cosigs);
        let key_refs = as_refs(&vks);
        verify_witness_quorum(&cosig_refs, &key_refs, 1, LogId::Kt, &tr, 7).expect("1-of-1 met");
        verify_witness_cosig(&c, &vks[0], LogId::Kt, &tr, 7)
            .expect("single-witness path also verifies");
    }

    // --- v2: the log name is bound (ADR-0098 section 5) ---

    #[test]
    fn cosig_for_one_log_never_verifies_for_the_other() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        for (signed, other) in [
            (LogId::Kt, LogId::Moderation),
            (LogId::Moderation, LogId::Kt),
        ] {
            let cosig = sign_witness_cosig(&wk, signed, &tr, 42);
            verify_witness_cosig(&cosig, &vk, signed, &tr, 42).expect("same log verifies");
            assert!(matches!(
                verify_witness_cosig(&cosig, &vk, other, &tr, 42),
                Err(Error::WitnessCosigInvalid)
            ));
            let cosigs: [&[u8]; 1] = [&cosig];
            let keys: [&[u8]; 1] = [&vk];
            assert!(matches!(
                verify_witness_quorum(&cosigs, &keys, 1, other, &tr, 42),
                Err(Error::WitnessQuorumNotMet { achieved: 0, .. })
            ));
        }
    }

    #[test]
    fn version_one_blob_is_refused() {
        let wk = SigningKey::from_seed(&[0x33; 32]);
        let vk = wk.verifying_key().to_bytes();
        let tr = root_at(0xAB);
        let mut cosig = sign_witness_cosig(&wk, LogId::Kt, &tr, 42);
        assert_eq!(cosig[0], 2);
        cosig[0] = 1;
        assert!(matches!(
            verify_witness_cosig(&cosig, &vk, LogId::Kt, &tr, 42),
            Err(Error::WitnessCosigBadVersion(1))
        ));
    }

    #[test]
    fn signing_input_layout_is_pinned() {
        let tr = [0x11u8; 32];
        let input = cosig_signing_input(LogId::Moderation, &tr, 7);
        let mut expected = b"lakko/transparency/witness/v2".to_vec();
        expected.push(2);
        expected.push(23);
        expected.extend_from_slice(b"lakko/moderation-log/v1");
        expected.extend_from_slice(&tr);
        expected.extend_from_slice(&7u64.to_be_bytes());
        assert_eq!(input, expected);
    }

    #[test]
    fn log_id_wire_names_round_trip() {
        for log in LogId::ALL {
            assert_eq!(LogId::from_wire_name(log.wire_name()), Some(log));
        }
        assert_eq!(LogId::from_wire_name("KT"), None);
        assert_eq!(LogId::from_wire_name(""), None);
        assert_eq!(LogId::Kt.byte(), 1);
        assert_eq!(LogId::Moderation.byte(), 2);
    }
}
