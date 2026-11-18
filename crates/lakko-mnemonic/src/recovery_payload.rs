// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The another-device recovery payload, v3 (ADR-0018 Path B, amended by
//! ADR-0074 "Joining after a rotation", 2026-11-15).
//!
//! After an add-device pairing, the confirming device hands the joining device
//! everything it needs to hold the account's keys: a single-use device
//! authorisation grant (ACCESS), the 16-byte entropy of the 12-word recovery
//! phrase (IDENTITY), and since v3 the account's own **predecessor seeds**,
//! the keys the account rotated away from. The oldest of them is the key every
//! conversation leaf of the account is bound to; a device that joins without
//! it must not bind at all (G-56), so the seeds ride in the one row without
//! which nothing lands.
//!
//! ```text
//!   plaintext v3 = grant(32)
//!                ‖ phrase_entropy(16)
//!                ‖ count(1)                       0..=8
//!                ‖ count × ( seed(64) ‖ adopted_at_ms(8, big-endian, unsigned) )
//!   length       = exactly 49 + 72 × count       (49..=625)
//! ```
//!
//! The predecessors are listed oldest first, the order the pairing state
//! bundle and the backup use.
//!
//! The clients seal the plaintext under a 32-byte key both devices derive from
//! the account's DeviceGroup with the MLS exporter label
//! [`RECOVERY_EXPORTER_LABEL`] and the pairing's rendezvous pseudonym as
//! context, and post `RECOVERY_ROW_MAGIC ‖ sealed` to the rendezvous mailbox.
//! This module owns the plaintext codec and the row tag; the seal and the
//! exporter stay where they are (the clients call `sealWithKey` /
//! `openWithKey` and `DeviceGroup.exportSecret`).
//!
//! # Refusals
//!
//! [`compose_recovery_payload`] refuses exactly what
//! [`decode_recovery_payload`] refuses:
//!
//! * a length other than `49 + 72 × count`, or a count above
//!   [`RECOVERY_PREDECESSORS_MAX`];
//! * two predecessor seeds that are the same seed;
//! * a predecessor seed that is the current account seed, which is the BIP39
//!   seed of the phrase the same payload carries.
//!
//! The contract states the last two in terms of keys ("a seed whose key equals
//! the current key or another carried seed's key"). The account signature key
//! is a deterministic function of the 64-byte seed (`lakko-mls`
//! `Identity::from_seed`, HKDF-SHA-256 then Ed25519), so equal seeds give
//! equal keys, and two different seeds give the same key only through an
//! HKDF-SHA-256 collision. Comparing seeds therefore refuses the same inputs
//! without this crate depending on `lakko-mls`.
//!
//! # No dual accept
//!
//! v2 (`grant ‖ entropy`, 48 bytes) is refused: by length here, by the
//! exporter label (the key differs) and by the row magic
//! ([`unwrap_recovery_row`] answers [`RecoveryPayloadError::UnsupportedRow`]
//! for any other `lakko/recovery-payload/` version).
//!
//! # Secrets
//!
//! The grant, the entropy and every seed are secret. [`RecoveryPayload`] and
//! [`RecoveryPredecessor`] zeroize on drop and their `Debug` prints counts
//! only; the composed plaintext is returned as a [`Zeroizing`] buffer.

use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{Mnemonic, ENTROPY_LEN};

/// MLS exporter label of the per-pairing recovery key. The `/v3` suffix is
/// part of the wire contract (v2 was `lakko/device-recovery/v2`).
pub const RECOVERY_EXPORTER_LABEL: &str = "lakko/device-recovery/v3";

/// Exporter output length: the ChaCha20-Poly1305 key size.
pub const RECOVERY_EXPORTER_KEY_LEN: usize = 32;

/// ASCII tag in front of the sealed blob in a rendezvous-mailbox row.
pub const RECOVERY_ROW_MAGIC: &[u8] = b"lakko/recovery-payload/v3";

/// The part of the row magic every version shares. A row that starts with it
/// but is not [`RECOVERY_ROW_MAGIC`] is a recovery row of another version.
const RECOVERY_ROW_FAMILY: &[u8] = b"lakko/recovery-payload/";

/// Raw device-authorisation grant length
/// (`services/auth/src/device_authorize.rs` `GRANT_TOKEN_LEN`).
pub const RECOVERY_GRANT_LEN: usize = 32;

/// Length of one predecessor seed: the 64-byte BIP39 seed of an earlier
/// phrase.
pub const RECOVERY_PREDECESSOR_SEED_LEN: usize = 64;

/// Most predecessors one payload carries. Chosen equal to the backup's
/// `MAX_OWN_PREDECESSORS` (8), the bound every other carrier of the own
/// predecessors uses; the FFI crates assert the two are equal.
pub const RECOVERY_PREDECESSORS_MAX: usize = 8;

/// Bytes per predecessor entry: seed plus the adoption time.
const PREDECESSOR_ENTRY_LEN: usize = RECOVERY_PREDECESSOR_SEED_LEN + 8;

/// Length of a payload with no predecessor: `grant ‖ entropy ‖ count`.
pub const RECOVERY_PAYLOAD_MIN_LEN: usize = RECOVERY_GRANT_LEN + ENTROPY_LEN + 1;

/// Length of a payload with [`RECOVERY_PREDECESSORS_MAX`] predecessors.
pub const RECOVERY_PAYLOAD_MAX_LEN: usize =
    RECOVERY_PAYLOAD_MIN_LEN + RECOVERY_PREDECESSORS_MAX * PREDECESSOR_ENTRY_LEN;

/// Why a recovery payload or row was refused. [`Self::code`] gives the stable
/// kebab-case code the FFI surfaces carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryPayloadError {
    /// The grant is not [`RECOVERY_GRANT_LEN`] bytes (compose only).
    #[error("the grant must be {RECOVERY_GRANT_LEN} bytes")]
    BadGrantLength,
    /// The entropy is not [`ENTROPY_LEN`] bytes (compose only).
    #[error("the phrase entropy must be {ENTROPY_LEN} bytes")]
    BadEntropyLength,
    /// More than [`RECOVERY_PREDECESSORS_MAX`] predecessors.
    #[error("more than {RECOVERY_PREDECESSORS_MAX} predecessors")]
    TooManyPredecessors,
    /// The plaintext is not `49 + 72 × count` bytes.
    #[error("the payload length does not match its predecessor count")]
    BadLength,
    /// Two predecessors carry the same seed.
    #[error("a predecessor is listed twice")]
    DuplicatePredecessor,
    /// A predecessor is the account's current seed.
    #[error("a predecessor is the current account key")]
    CurrentKeyPredecessor,
    /// The row is a recovery row of a version other than v3.
    #[error("a recovery row of another version")]
    UnsupportedRow,
}

impl RecoveryPayloadError {
    /// Stable kebab-case code, frozen: the FFI messages start with it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadGrantLength => "bad-grant-length",
            Self::BadEntropyLength => "bad-entropy-length",
            Self::TooManyPredecessors => "too-many-predecessors",
            Self::BadLength => "bad-length",
            Self::DuplicatePredecessor => "duplicate-predecessor",
            Self::CurrentKeyPredecessor => "current-key-predecessor",
            Self::UnsupportedRow => "unsupported-row",
        }
    }
}

/// One own predecessor: the seed of an earlier phrase and when the account
/// adopted the key that replaced it.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryPredecessor {
    /// The 64-byte BIP39 seed of the earlier phrase. Secret.
    pub seed: [u8; RECOVERY_PREDECESSOR_SEED_LEN],
    /// Unix milliseconds at which the next key replaced this one.
    pub adopted_at_ms: u64,
}

impl core::fmt::Debug for RecoveryPredecessor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecoveryPredecessor")
            .field("adopted_at_ms", &self.adopted_at_ms)
            .finish_non_exhaustive()
    }
}

/// A decoded recovery payload.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryPayload {
    /// The single-use device-authorisation grant. Secret.
    pub grant: [u8; RECOVERY_GRANT_LEN],
    /// The 16-byte entropy of the current 12-word phrase. Secret.
    pub phrase_entropy: [u8; ENTROPY_LEN],
    /// The own predecessors, oldest first.
    pub predecessors: Vec<RecoveryPredecessor>,
}

impl core::fmt::Debug for RecoveryPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecoveryPayload")
            .field("predecessors", &self.predecessors.len())
            .finish_non_exhaustive()
    }
}

/// The checks both directions share: the count bound, no seed twice, no seed
/// equal to the current seed.
fn check_predecessors(
    phrase_entropy: &[u8; ENTROPY_LEN],
    predecessors: &[RecoveryPredecessor],
) -> Result<(), RecoveryPayloadError> {
    if predecessors.len() > RECOVERY_PREDECESSORS_MAX {
        return Err(RecoveryPayloadError::TooManyPredecessors);
    }
    for (i, a) in predecessors.iter().enumerate() {
        if predecessors[i + 1..].iter().any(|b| b.seed == a.seed) {
            return Err(RecoveryPayloadError::DuplicatePredecessor);
        }
    }
    if predecessors.is_empty() {
        return Ok(());
    }
    // 16 bytes are always valid BIP39 entropy; the error arm is the length
    // check, unreachable here.
    let current = Mnemonic::from_entropy(phrase_entropy)
        .map_err(|_| RecoveryPayloadError::BadEntropyLength)?
        .to_seed();
    if predecessors
        .iter()
        .any(|p| p.seed.as_slice() == current.as_ref())
    {
        return Err(RecoveryPayloadError::CurrentKeyPredecessor);
    }
    Ok(())
}

/// Build the v3 plaintext.
///
/// # Errors
///
/// [`RecoveryPayloadError::BadGrantLength`] / [`RecoveryPayloadError::BadEntropyLength`]
/// for an argument of the wrong length, and what [`decode_recovery_payload`]
/// refuses: [`RecoveryPayloadError::TooManyPredecessors`],
/// [`RecoveryPayloadError::DuplicatePredecessor`],
/// [`RecoveryPayloadError::CurrentKeyPredecessor`].
pub fn compose_recovery_payload(
    grant: &[u8],
    phrase_entropy: &[u8],
    predecessors: &[RecoveryPredecessor],
) -> Result<Zeroizing<Vec<u8>>, RecoveryPayloadError> {
    let grant: &[u8; RECOVERY_GRANT_LEN] = grant
        .try_into()
        .map_err(|_| RecoveryPayloadError::BadGrantLength)?;
    let entropy: &[u8; ENTROPY_LEN] = phrase_entropy
        .try_into()
        .map_err(|_| RecoveryPayloadError::BadEntropyLength)?;
    check_predecessors(entropy, predecessors)?;

    let count =
        u8::try_from(predecessors.len()).map_err(|_| RecoveryPayloadError::TooManyPredecessors)?;
    let mut out = Zeroizing::new(Vec::with_capacity(
        RECOVERY_PAYLOAD_MIN_LEN + predecessors.len() * PREDECESSOR_ENTRY_LEN,
    ));
    out.extend_from_slice(grant);
    out.extend_from_slice(entropy);
    out.push(count);
    for p in predecessors {
        out.extend_from_slice(&p.seed);
        out.extend_from_slice(&p.adopted_at_ms.to_be_bytes());
    }
    Ok(out)
}

/// Parse a v3 plaintext. Bounded and fail-closed: the count is read and
/// bounded before any predecessor is copied, and the length must be exact.
///
/// # Errors
///
/// [`RecoveryPayloadError::BadLength`] for a length other than
/// `49 + 72 × count` (a v2 payload included),
/// [`RecoveryPayloadError::TooManyPredecessors`],
/// [`RecoveryPayloadError::DuplicatePredecessor`],
/// [`RecoveryPayloadError::CurrentKeyPredecessor`].
pub fn decode_recovery_payload(bytes: &[u8]) -> Result<RecoveryPayload, RecoveryPayloadError> {
    if bytes.len() < RECOVERY_PAYLOAD_MIN_LEN || bytes.len() > RECOVERY_PAYLOAD_MAX_LEN {
        return Err(RecoveryPayloadError::BadLength);
    }
    let count = usize::from(bytes[RECOVERY_PAYLOAD_MIN_LEN - 1]);
    if count > RECOVERY_PREDECESSORS_MAX {
        return Err(RecoveryPayloadError::TooManyPredecessors);
    }
    if bytes.len() != RECOVERY_PAYLOAD_MIN_LEN + count * PREDECESSOR_ENTRY_LEN {
        return Err(RecoveryPayloadError::BadLength);
    }

    let mut payload = RecoveryPayload {
        grant: [0u8; RECOVERY_GRANT_LEN],
        phrase_entropy: [0u8; ENTROPY_LEN],
        predecessors: Vec::with_capacity(count),
    };
    payload.grant.copy_from_slice(&bytes[..RECOVERY_GRANT_LEN]);
    payload
        .phrase_entropy
        .copy_from_slice(&bytes[RECOVERY_GRANT_LEN..RECOVERY_GRANT_LEN + ENTROPY_LEN]);
    for entry in bytes[RECOVERY_PAYLOAD_MIN_LEN..].chunks_exact(PREDECESSOR_ENTRY_LEN) {
        let mut p = RecoveryPredecessor {
            seed: [0u8; RECOVERY_PREDECESSOR_SEED_LEN],
            adopted_at_ms: 0,
        };
        p.seed
            .copy_from_slice(&entry[..RECOVERY_PREDECESSOR_SEED_LEN]);
        let mut at = [0u8; 8];
        at.copy_from_slice(&entry[RECOVERY_PREDECESSOR_SEED_LEN..]);
        p.adopted_at_ms = u64::from_be_bytes(at);
        payload.predecessors.push(p);
    }
    check_predecessors(&payload.phrase_entropy, &payload.predecessors)?;
    Ok(payload)
}

/// The rendezvous-mailbox row: `RECOVERY_ROW_MAGIC ‖ sealed`.
#[must_use]
pub fn wrap_recovery_row(sealed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(RECOVERY_ROW_MAGIC.len() + sealed.len());
    out.extend_from_slice(RECOVERY_ROW_MAGIC);
    out.extend_from_slice(sealed);
    out
}

/// The sealed blob of a v3 recovery row; `Ok(None)` for a row that is not a
/// recovery row at all (a Welcome, a history bundle, a state bundle), which
/// the caller skips.
///
/// # Errors
///
/// [`RecoveryPayloadError::UnsupportedRow`] for a recovery row of another
/// version (v2 included) or a v3 tag with nothing after it: the joiner shows
/// its "could not hand over the keys" error.
pub fn unwrap_recovery_row(row: &[u8]) -> Result<Option<&[u8]>, RecoveryPayloadError> {
    match row.strip_prefix(RECOVERY_ROW_MAGIC) {
        Some([]) => Err(RecoveryPayloadError::UnsupportedRow),
        Some(sealed) => Ok(Some(sealed)),
        None if row.starts_with(RECOVERY_ROW_FAMILY) => Err(RecoveryPayloadError::UnsupportedRow),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pred(byte: u8, at: u64) -> RecoveryPredecessor {
        RecoveryPredecessor {
            seed: [byte; RECOVERY_PREDECESSOR_SEED_LEN],
            adopted_at_ms: at,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// The contract's golden plaintext (contract-g56 section 3.1).
    #[test]
    fn golden_plaintext_vector() {
        let wire = compose_recovery_payload(
            &[0x01; 32],
            &[0x02; 16],
            &[pred(0x03, 1_700_000_000_000), pred(0x04, 1_700_000_100_000)],
        )
        .expect("composes");
        assert_eq!(wire.len(), 193);
        assert_eq!(hex(&wire[113..121]), "0000018bcfe56800");
        assert_eq!(hex(&wire[185..193]), "0000018bcfe6eea0");
        assert_eq!(
            hex(&lakko_crypto::hash::sha256(&wire)),
            "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41"
        );
        let back = decode_recovery_payload(&wire).expect("decodes");
        assert_eq!(back.grant, [0x01; 32]);
        assert_eq!(back.phrase_entropy, [0x02; 16]);
        assert_eq!(back.predecessors.len(), 2);
        assert_eq!(back.predecessors[0].seed, [0x03; 64]);
        assert_eq!(back.predecessors[0].adopted_at_ms, 1_700_000_000_000);
        assert_eq!(back.predecessors[1].seed, [0x04; 64]);
        assert_eq!(back.predecessors[1].adopted_at_ms, 1_700_000_100_000);
    }

    #[test]
    fn bounds_are_the_contract_numbers() {
        assert_eq!(RECOVERY_PAYLOAD_MIN_LEN, 49);
        assert_eq!(RECOVERY_PAYLOAD_MAX_LEN, 625);
        assert_eq!(RECOVERY_EXPORTER_LABEL, "lakko/device-recovery/v3");
        assert_eq!(RECOVERY_ROW_MAGIC, b"lakko/recovery-payload/v3");
    }

    #[test]
    fn an_empty_list_is_count_zero() {
        let wire = compose_recovery_payload(&[7; 32], &[8; 16], &[]).expect("composes");
        assert_eq!(wire.len(), 49);
        assert_eq!(wire[48], 0);
        let back = decode_recovery_payload(&wire).expect("decodes");
        assert!(back.predecessors.is_empty());
    }

    #[test]
    fn a_v2_payload_is_refused() {
        let mut v2 = vec![1u8; 32];
        v2.extend_from_slice(&[2u8; 16]);
        assert_eq!(
            decode_recovery_payload(&v2).unwrap_err(),
            RecoveryPayloadError::BadLength
        );
    }

    #[test]
    fn nine_predecessors_are_refused_both_ways() {
        let preds: Vec<_> = (0..9).map(|i| pred(0x30 + i, u64::from(i))).collect();
        assert_eq!(
            compose_recovery_payload(&[1; 32], &[2; 16], &preds).unwrap_err(),
            RecoveryPayloadError::TooManyPredecessors
        );
        let mut wire = vec![1u8; 32];
        wire.extend_from_slice(&[2u8; 16]);
        wire.push(9);
        for p in &preds {
            wire.extend_from_slice(&p.seed);
            wire.extend_from_slice(&p.adopted_at_ms.to_be_bytes());
        }
        assert_eq!(
            decode_recovery_payload(&wire).unwrap_err(),
            RecoveryPayloadError::BadLength,
            "697 bytes is past the maximum length"
        );
        // A count of 9 inside an in-bounds length.
        let mut short = wire[..RECOVERY_PAYLOAD_MIN_LEN + PREDECESSOR_ENTRY_LEN].to_vec();
        short[48] = 9;
        assert_eq!(
            decode_recovery_payload(&short).unwrap_err(),
            RecoveryPayloadError::TooManyPredecessors
        );
    }

    #[test]
    fn eight_predecessors_are_the_maximum() {
        let preds: Vec<_> = (0..8).map(|i| pred(0x30 + i, u64::from(i))).collect();
        let wire = compose_recovery_payload(&[1; 32], &[2; 16], &preds).expect("composes");
        assert_eq!(wire.len(), RECOVERY_PAYLOAD_MAX_LEN);
        assert_eq!(
            decode_recovery_payload(&wire)
                .expect("decodes")
                .predecessors
                .len(),
            8
        );
    }

    #[test]
    fn a_duplicate_is_refused_both_ways() {
        let preds = [pred(0x03, 1), pred(0x03, 2)];
        assert_eq!(
            compose_recovery_payload(&[1; 32], &[2; 16], &preds).unwrap_err(),
            RecoveryPayloadError::DuplicatePredecessor
        );
        let ok = compose_recovery_payload(&[1; 32], &[2; 16], &[pred(0x03, 1), pred(0x04, 2)])
            .expect("composes");
        let mut wire = ok.to_vec();
        wire.copy_within(49..49 + 64, 49 + 72);
        assert_eq!(
            decode_recovery_payload(&wire).unwrap_err(),
            RecoveryPayloadError::DuplicatePredecessor
        );
    }

    #[test]
    fn the_current_seed_as_a_predecessor_is_refused_both_ways() {
        let entropy = [0x02u8; 16];
        let current = Mnemonic::from_entropy(&entropy)
            .expect("16 bytes")
            .to_seed();
        let mut seed = [0u8; 64];
        seed.copy_from_slice(current.as_ref());
        let preds = [
            pred(0x03, 1),
            RecoveryPredecessor {
                seed,
                adopted_at_ms: 2,
            },
        ];
        assert_eq!(
            compose_recovery_payload(&[1; 32], &entropy, &preds).unwrap_err(),
            RecoveryPayloadError::CurrentKeyPredecessor
        );
        let mut wire =
            compose_recovery_payload(&[1; 32], &entropy, &[pred(0x03, 1), pred(0x04, 2)])
                .expect("composes")
                .to_vec();
        wire[49 + 72..49 + 72 + 64].copy_from_slice(&seed);
        assert_eq!(
            decode_recovery_payload(&wire).unwrap_err(),
            RecoveryPayloadError::CurrentKeyPredecessor
        );
    }

    #[test]
    fn wrong_argument_lengths_are_refused() {
        assert_eq!(
            compose_recovery_payload(&[1; 31], &[2; 16], &[]).unwrap_err(),
            RecoveryPayloadError::BadGrantLength
        );
        assert_eq!(
            compose_recovery_payload(&[1; 32], &[2; 15], &[]).unwrap_err(),
            RecoveryPayloadError::BadEntropyLength
        );
    }

    #[test]
    fn rows_are_told_apart() {
        let row = wrap_recovery_row(&[9, 9, 9]);
        assert_eq!(
            unwrap_recovery_row(&row).expect("v3"),
            Some(&[9u8, 9, 9][..])
        );
        assert_eq!(unwrap_recovery_row(b"a welcome").expect("not ours"), None);
        assert_eq!(unwrap_recovery_row(b"").expect("not ours"), None);
        assert_eq!(
            unwrap_recovery_row(b"lakko/recovery-payload/v2abc").unwrap_err(),
            RecoveryPayloadError::UnsupportedRow
        );
        assert_eq!(
            unwrap_recovery_row(RECOVERY_ROW_MAGIC).unwrap_err(),
            RecoveryPayloadError::UnsupportedRow
        );
    }

    #[test]
    fn debug_prints_no_secret() {
        let payload = decode_recovery_payload(
            &compose_recovery_payload(&[0xAB; 32], &[0xCD; 16], &[pred(0xEF, 5)]).expect("ok"),
        )
        .expect("ok");
        let s = format!("{payload:?} {:?}", payload.predecessors[0]);
        assert!(
            !s.contains("171") && !s.contains("205") && !s.contains("239"),
            "{s}"
        );
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(RecoveryPayloadError::BadLength.code(), "bad-length");
        assert_eq!(
            RecoveryPayloadError::UnsupportedRow.code(),
            "unsupported-row"
        );
        assert_eq!(
            RecoveryPayloadError::CurrentKeyPredecessor.code(),
            "current-key-predecessor"
        );
        assert_eq!(
            RecoveryPayloadError::DuplicatePredecessor.code(),
            "duplicate-predecessor"
        );
        assert_eq!(
            RecoveryPayloadError::TooManyPredecessors.code(),
            "too-many-predecessors"
        );
    }

    fn arb_preds() -> impl Strategy<Value = Vec<RecoveryPredecessor>> {
        prop::collection::btree_set(any::<[u8; 8]>(), 0..=RECOVERY_PREDECESSORS_MAX)
            .prop_flat_map(|prefixes| {
                let n = prefixes.len();
                (Just(prefixes), prop::collection::vec(any::<u64>(), n))
            })
            .prop_map(|(prefixes, times)| {
                prefixes
                    .into_iter()
                    .zip(times)
                    .map(|(prefix, at)| {
                        let mut seed = [0x5Au8; 64];
                        seed[..8].copy_from_slice(&prefix);
                        RecoveryPredecessor {
                            seed,
                            adopted_at_ms: at,
                        }
                    })
                    .collect()
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        #[test]
        fn round_trip(grant in any::<[u8; 32]>(), entropy in any::<[u8; 16]>(), preds in arb_preds()) {
            let wire = compose_recovery_payload(&grant, &entropy, &preds).expect("composes");
            prop_assert_eq!(wire.len(), RECOVERY_PAYLOAD_MIN_LEN + preds.len() * PREDECESSOR_ENTRY_LEN);
            let back = decode_recovery_payload(&wire).expect("decodes");
            prop_assert_eq!(back.grant, grant);
            prop_assert_eq!(back.phrase_entropy, entropy);
            prop_assert_eq!(back.predecessors.len(), preds.len());
            for (a, b) in back.predecessors.iter().zip(&preds) {
                prop_assert_eq!(a.seed, b.seed);
                prop_assert_eq!(a.adopted_at_ms, b.adopted_at_ms);
            }
            let again = compose_recovery_payload(&back.grant, &back.phrase_entropy, &back.predecessors)
                .expect("composes");
            prop_assert_eq!(again.as_slice(), wire.as_slice());
        }

        #[test]
        fn every_truncation_and_extension_is_refused(
            grant in any::<[u8; 32]>(), entropy in any::<[u8; 16]>(), preds in arb_preds(), extra in any::<u8>()
        ) {
            let wire = compose_recovery_payload(&grant, &entropy, &preds).expect("composes");
            for cut in 0..wire.len() {
                prop_assert!(decode_recovery_payload(&wire[..cut]).is_err(), "cut at {}", cut);
            }
            let mut longer = wire.to_vec();
            longer.push(extra);
            prop_assert!(decode_recovery_payload(&longer).is_err());
        }

        #[test]
        fn arbitrary_bytes_never_decode_unless_canonical(bytes in prop::collection::vec(any::<u8>(), 0..700)) {
            if let Ok(p) = decode_recovery_payload(&bytes) {
                let again = compose_recovery_payload(&p.grant, &p.phrase_entropy, &p.predecessors)
                    .expect("what decodes composes");
                prop_assert_eq!(again.as_slice(), bytes.as_slice());
            }
        }

        #[test]
        fn random_bytes_of_a_wrong_length_never_decode(bytes in prop::collection::vec(any::<u8>(), 0..700)) {
            let ok_len = bytes.len() >= RECOVERY_PAYLOAD_MIN_LEN
                && bytes.len() <= RECOVERY_PAYLOAD_MAX_LEN
                && (bytes.len() - RECOVERY_PAYLOAD_MIN_LEN) % PREDECESSOR_ENTRY_LEN == 0
                && usize::from(bytes[RECOVERY_PAYLOAD_MIN_LEN - 1])
                    == (bytes.len() - RECOVERY_PAYLOAD_MIN_LEN) / PREDECESSOR_ENTRY_LEN;
            if !ok_len {
                prop_assert!(decode_recovery_payload(&bytes).is_err());
            }
        }
    }
}
