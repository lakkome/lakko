// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the another-device recovery payload v3 and the own
//! binding verdict (ADR-0018 Path B; ADR-0074 "Joining after a rotation",
//! G-56). Mirror of `crates/lakko-ffi-wasm/src/recovery_payload.rs`.
//!
//! Three things a client must not re-derive:
//!
//! - the recovery plaintext v3 ([`lakko_mnemonic::recovery_payload`]):
//!   [`compose_recovery_payload`] / [`decode_recovery_payload`], plus the row
//!   tag ([`wrap_recovery_row`] / [`unwrap_recovery_row`]) and the exporter
//!   label ([`recovery_exporter_label`]). The seal stays where it was: the
//!   client derives the key with `DeviceGroup.exportSecret(identity,
//!   recoveryExporterLabel(), rendezvousPseudonym, recoveryExporterKeyLen())`
//!   and calls `sealWithKey` / `openWithKey`;
//! - the account's own conversation key read from its directory link
//!   ([`own_conversation_key`], over `lakko_transparency`);
//! - the binding verdict with `wait` ([`own_binding_verdict`], over
//!   `lakko_contacts::own_binding`).
//!
//! Every byte this module returns from a payload is secret: never log the
//! grant, the entropy or a seed; the predecessor count may be logged.

use lakko_contacts::own_binding::{binding_verdict, BindingVerdict, OwnLink};
use lakko_mnemonic::recovery_payload::{
    self as rp, RecoveryPayloadError, RecoveryPredecessor, RECOVERY_PREDECESSOR_SEED_LEN,
};

use crate::FfiError;

const _: () = assert!(rp::RECOVERY_PREDECESSORS_MAX == lakko_backup::MAX_OWN_PREDECESSORS);
const _: () = assert!(RECOVERY_PREDECESSOR_SEED_LEN == lakko_backup::PREDECESSOR_SEED_LEN);

fn payload_err(e: RecoveryPayloadError) -> FfiError {
    FfiError::RecoveryPayload(format!("{}: {e}", e.code()))
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    bytes
        .try_into()
        .map_err(|_| FfiError::RecoveryPayload(format!("bad-key-length: {what} must be 32 bytes")))
}

/// One own predecessor carried in the recovery payload. `seed` is SECRET.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiRecoveryPredecessor {
    /// The 64-byte BIP39 seed of the earlier phrase.
    pub seed: Vec<u8>,
    /// Unix milliseconds at which the next key replaced it.
    pub adopted_at_ms: u64,
}

/// A decoded recovery payload. Every byte field is SECRET.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiRecoveryPayload {
    /// The 32-byte single-use device-authorisation grant.
    pub grant: Vec<u8>,
    /// The 16-byte entropy of the current 12-word phrase.
    pub phrase_entropy: Vec<u8>,
    /// The own predecessors, oldest first (0..=8).
    pub predecessors: Vec<FfiRecoveryPredecessor>,
}

impl core::fmt::Debug for FfiRecoveryPredecessor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FfiRecoveryPredecessor")
            .field("adopted_at_ms", &self.adopted_at_ms)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for FfiRecoveryPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FfiRecoveryPayload")
            .field("predecessors", &self.predecessors.len())
            .finish_non_exhaustive()
    }
}

/// The MLS exporter label of the per-pairing recovery key,
/// `"lakko/device-recovery/v3"`.
#[uniffi::export]
#[must_use]
pub fn recovery_exporter_label() -> String {
    rp::RECOVERY_EXPORTER_LABEL.to_owned()
}

/// The exporter output length, 32.
#[uniffi::export]
#[must_use]
pub fn recovery_exporter_key_len() -> u32 {
    32
}

/// Most predecessors one payload carries, 8.
#[uniffi::export]
#[must_use]
pub fn recovery_predecessors_max() -> u32 {
    8
}

const _: () = assert!(rp::RECOVERY_EXPORTER_KEY_LEN == 32);
const _: () = assert!(rp::RECOVERY_PREDECESSORS_MAX == 8);

/// Build the v3 plaintext `grant ‖ entropy ‖ count ‖ count × (seed ‖
/// adopted_at_ms)`, predecessors oldest first.
///
/// # Errors
///
/// [`FfiError::RecoveryPayload`] `"<code>: <reason>"` with the codes of
/// `RecoveryPayloadError`: `bad-grant-length`, `bad-entropy-length`,
/// `too-many-predecessors`, `duplicate-predecessor`,
/// `current-key-predecessor`; or `bad-seed-length` for a seed that is not 64
/// bytes.
#[uniffi::export]
pub fn compose_recovery_payload(
    grant: Vec<u8>,
    phrase_entropy: Vec<u8>,
    predecessors: Vec<FfiRecoveryPredecessor>,
) -> Result<Vec<u8>, FfiError> {
    let preds = predecessors
        .iter()
        .map(|p| {
            let seed: [u8; RECOVERY_PREDECESSOR_SEED_LEN] =
                p.seed.as_slice().try_into().map_err(|_| {
                    FfiError::RecoveryPayload(
                        "bad-seed-length: a predecessor seed must be 64 bytes".into(),
                    )
                })?;
            Ok(RecoveryPredecessor {
                seed,
                adopted_at_ms: p.adopted_at_ms,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    rp::compose_recovery_payload(&grant, &phrase_entropy, &preds)
        .map(|wire| wire.to_vec())
        .map_err(payload_err)
}

/// Parse a v3 plaintext opened from a recovery row.
///
/// # Errors
///
/// [`FfiError::RecoveryPayload`] with `bad-length` (a v2 payload included),
/// `too-many-predecessors`, `duplicate-predecessor`,
/// `current-key-predecessor`. Any refusal means: the keys were not handed
/// over.
#[uniffi::export]
pub fn decode_recovery_payload(plaintext: Vec<u8>) -> Result<FfiRecoveryPayload, FfiError> {
    let p = rp::decode_recovery_payload(&plaintext).map_err(payload_err)?;
    Ok(FfiRecoveryPayload {
        grant: p.grant.to_vec(),
        phrase_entropy: p.phrase_entropy.to_vec(),
        predecessors: p
            .predecessors
            .iter()
            .map(|q| FfiRecoveryPredecessor {
                seed: q.seed.to_vec(),
                adopted_at_ms: q.adopted_at_ms,
            })
            .collect(),
    })
}

/// The mailbox row `"lakko/recovery-payload/v3" ‖ sealed`.
#[uniffi::export]
#[must_use]
pub fn wrap_recovery_row(sealed: Vec<u8>) -> Vec<u8> {
    rp::wrap_recovery_row(&sealed)
}

/// The sealed blob of a v3 recovery row, or `None` for a row that is not a
/// recovery row (skip it).
///
/// # Errors
///
/// [`FfiError::RecoveryPayload`] `unsupported-row` for a recovery row of
/// another version (v2 included): the keys were not handed over.
#[uniffi::export]
pub fn unwrap_recovery_row(row: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
    rp::unwrap_recovery_row(&row)
        .map(|sealed| sealed.map(<[u8]>::to_vec))
        .map_err(payload_err)
}

/// The account's own conversation key from the `conversation_link` of its
/// directory bundle, verified against THIS device's current account key
/// (`Identity.fromSeed(accountSeed).signaturePublicKey`, never the fetched
/// one). `None` for an absent or empty link, one that does not verify (minted
/// before a reset, or forged) and one naming the current key.
///
/// # Errors
///
/// [`FfiError::RecoveryPayload`] `bad-key-length` when `current_key` is not
/// 32 bytes.
#[uniffi::export]
pub fn own_conversation_key(
    link: Option<Vec<u8>>,
    current_key: Vec<u8>,
) -> Result<Option<Vec<u8>>, FfiError> {
    let current = key32(&current_key, "the current key")?;
    Ok(lakko_transparency::own_conversation_key(link.as_deref(), &current).map(|k| k.to_vec()))
}

/// What the account's own conversation link said.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiOwnLink {
    /// No link, or one that did not verify: the account follows no rotation.
    Absent,
    /// The link names this 32-byte conversation key.
    Key {
        /// The conversation key.
        key: Vec<u8>,
    },
    /// The bundle could not be read.
    Unknown,
}

/// What to do with the device's stored conversation binding.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiBindingVerdict {
    /// The stored binding is kept.
    Keep,
    /// Kept: the binding names the current key on a device that already
    /// holds conversations.
    KeepUnderCurrentKey,
    /// Bind nothing, publish nothing, join nothing, open no DeviceGroup: park
    /// in "earlier keys missing".
    Wait,
    /// Mint the binding under this 32-byte key, which this device holds.
    Mint {
        /// The account key to bind under.
        key: Vec<u8>,
    },
}

/// The binding verdict (ADR-0074 "Joining after a rotation"; the 17 shared
/// vectors live in `lakko_contacts::own_binding`). Run it with
/// [`FfiOwnLink::Absent`] first; read the link only when that answer is not
/// `Keep` / `KeepUnderCurrentKey`.
///
/// `bound_key` is the account key the stored binding names (`None` when there
/// is none or it did not verify); `predecessor_keys` the own predecessor keys
/// this device holds, oldest first, current excluded.
///
/// # Errors
///
/// [`FfiError::RecoveryPayload`] `bad-key-length` for any key that is not 32
/// bytes.
#[uniffi::export]
pub fn own_binding_verdict(
    bound_key: Option<Vec<u8>>,
    current_key: Vec<u8>,
    predecessor_keys: Vec<Vec<u8>>,
    link: FfiOwnLink,
    holds_conversations: bool,
) -> Result<FfiBindingVerdict, FfiError> {
    let bound = bound_key
        .as_deref()
        .map(|b| key32(b, "the bound key"))
        .transpose()?;
    let current = key32(&current_key, "the current key")?;
    let preds = predecessor_keys
        .iter()
        .map(|k| key32(k, "a predecessor key"))
        .collect::<Result<Vec<_>, _>>()?;
    let link = match link {
        FfiOwnLink::Absent => OwnLink::Absent,
        FfiOwnLink::Unknown => OwnLink::Unknown,
        FfiOwnLink::Key { key } => OwnLink::Key(key32(&key, "the link key")?),
    };
    Ok(
        match binding_verdict(bound.as_ref(), &current, &preds, &link, holds_conversations) {
            BindingVerdict::Keep => FfiBindingVerdict::Keep,
            BindingVerdict::KeepUnderCurrentKey => FfiBindingVerdict::KeepUnderCurrentKey,
            BindingVerdict::Wait => FfiBindingVerdict::Wait,
            BindingVerdict::Mint(k) => FfiBindingVerdict::Mint { key: k.to_vec() },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(b: u8, at: u64) -> FfiRecoveryPredecessor {
        FfiRecoveryPredecessor {
            seed: vec![b; 64],
            adopted_at_ms: at,
        }
    }

    #[test]
    fn golden_plaintext_over_the_ffi() {
        let wire = compose_recovery_payload(
            vec![1; 32],
            vec![2; 16],
            vec![pred(3, 1_700_000_000_000), pred(4, 1_700_000_100_000)],
        )
        .unwrap();
        assert_eq!(wire.len(), 193);
        let digest = lakko_crypto::hash::sha256(&wire);
        let hex: String = digest.iter().fold(String::new(), |mut s, b| {
            use core::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex,
            "43260742d3c1e705179d7ac8baf1f0566b5fd9621eb1df08bb9a48e34dd56a41"
        );
        let back = decode_recovery_payload(wire).unwrap();
        assert_eq!(
            back.predecessors,
            vec![pred(3, 1_700_000_000_000), pred(4, 1_700_000_100_000)]
        );
    }

    #[test]
    fn refusals_carry_codes() {
        let e = decode_recovery_payload(vec![0; 48]).unwrap_err();
        assert!(e.to_string().starts_with("bad-length:"), "{e}");
        let e = compose_recovery_payload(
            vec![1; 32],
            vec![2; 16],
            vec![FfiRecoveryPredecessor {
                seed: vec![3; 63],
                adopted_at_ms: 0,
            }],
        )
        .unwrap_err();
        assert!(e.to_string().starts_with("bad-seed-length:"), "{e}");
        let e = unwrap_recovery_row(b"lakko/recovery-payload/v2xyz".to_vec()).unwrap_err();
        assert!(e.to_string().starts_with("unsupported-row:"), "{e}");
        assert_eq!(unwrap_recovery_row(b"welcome".to_vec()).unwrap(), None);
        assert_eq!(
            unwrap_recovery_row(wrap_recovery_row(vec![7])).unwrap(),
            Some(vec![7])
        );
        assert_eq!(recovery_exporter_label(), "lakko/device-recovery/v3");
    }

    #[test]
    fn the_verdict_over_the_ffi() {
        let cur = vec![0xC0; 32];
        let k0 = vec![0x10; 32];
        assert_eq!(
            own_binding_verdict(
                None,
                cur.clone(),
                vec![],
                FfiOwnLink::Key { key: k0.clone() },
                false
            )
            .unwrap(),
            FfiBindingVerdict::Wait
        );
        assert_eq!(
            own_binding_verdict(
                Some(cur.clone()),
                cur.clone(),
                vec![k0.clone()],
                FfiOwnLink::Key { key: k0.clone() },
                false
            )
            .unwrap(),
            FfiBindingVerdict::Mint { key: k0 }
        );
        assert_eq!(
            own_binding_verdict(None, cur.clone(), vec![], FfiOwnLink::Unknown, false).unwrap(),
            FfiBindingVerdict::Wait
        );
        assert!(own_binding_verdict(None, vec![0; 31], vec![], FfiOwnLink::Absent, false).is_err());
    }

    #[test]
    fn own_conversation_key_over_the_ffi() {
        use lakko_crypto::signing::SigningKey;
        let conv = SigningKey::from_seed(&[1; 32]);
        let cur = SigningKey::from_seed(&[2; 32]);
        let link = lakko_transparency::ConversationLink::sign(&conv, &cur, 1).unwrap();
        let cur_key = cur.verifying_key().to_bytes().to_vec();
        assert_eq!(
            own_conversation_key(Some(link.to_vec()), cur_key.clone()).unwrap(),
            Some(conv.verifying_key().to_bytes().to_vec())
        );
        assert_eq!(own_conversation_key(None, cur_key.clone()).unwrap(), None);
        assert_eq!(own_conversation_key(Some(vec![]), cur_key).unwrap(), None);
        assert_eq!(
            own_conversation_key(Some(link.to_vec()), vec![9; 32]).unwrap(),
            None
        );
    }
}
