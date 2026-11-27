// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channel recovery key and its owner-change proof ([ADR-0101]
//! section 7, "Owner").
//!
//! ## Why
//!
//! The channel owner key ([`crate::owner`]) is derived from the account
//! seed, so an ADR-0073 key reset loses it and nobody else holds it. ADR-0101
//! gives the owner a second key that survives the reset: the channel
//! recovery key, derived from the account's owned-channels **list key** `R`
//! ([`crate::owned`]), which the account keeps on the storage service under
//! locks a reset leaves openable (the password lock with the server half
//! `S`, a passkey lock).
//!
//! At channel creation (and for an existing channel whose registered key
//! differs, at the owner device's next publisher pass) the owner registers
//! the recovery public key with `services/channels` beside the owner key.
//! After a reset, the resetting device opens the list, derives each
//! channel's recovery key, and signs an owner change naming its new owner
//! key, which `services/channels` verifies with
//! [`verify_recovery_owner_change`] against the stored recovery key.
//!
//! ## Derivation (amended 2026-11-26)
//!
//! ```text
//! recovery_seed = BLAKE3-derive-key("lakko/channels/recovery-key/v2", R(32) ‖ channel_id(16))
//! recovery_key  = Ed25519 from recovery_seed
//! ```
//!
//! `R` never changes, so neither a password change nor a rotation
//! re-registers anything. The 2026-11-25 derivation (`.../v1` over a VOPRF
//! output auth evaluated under a per-account key) is removed with its OPRF
//! input: it let the operator take any channel whose id it knew.
//!
//! ## The owner-change proof
//!
//! ```text
//! signed = RECOVERY_OWNER_CHANGE_DOMAIN ‖ channel_id(16) ‖ new_owner_pk(32) ‖ unix_ms(8 BE)
//! header = v1.<unix_ms>.<sig_b64url>
//! ```
//!
//! Carried as the `x-lakko-channel-recovery-proof` header on
//! `PUT /v1/channels/<id>/owner`, whose body is the new owner key
//! ([`crate::owner::owner_handover_body`]). The verifier checks the window
//! ([`crate::owner::OWNER_PROOF_WINDOW_MS`]) before the signature. There is
//! no nonce: a replay within the window can only set the owner key the
//! proof already names, which is the state the first request left.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::owner::OWNER_PROOF_WINDOW_MS;
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of a channel's recovery seed, over the list key
/// `R` and the channel id. Frozen.
pub const CHANNEL_RECOVERY_KEY_DOMAIN: &str = "lakko/channels/recovery-key/v2";

/// Domain separator of the owner-change proof's signed bytes. Frozen.
pub const RECOVERY_OWNER_CHANGE_DOMAIN: &[u8] = b"lakko/channels/recovery-owner-change/v1\x00";

/// The request header the proof travels in (lowercase).
pub const RECOVERY_PROOF_HEADER_NAME: &str = "x-lakko-channel-recovery-proof";

/// Upper bound on the header's length; anything longer is refused unparsed.
pub const RECOVERY_PROOF_MAX_HEADER_LEN: usize = 128;

const VERSION: &str = "v1";
const SIGNATURE_LEN: usize = 64;
const KEY_LEN: usize = 32;

/// Why an owner-change proof was refused. The server maps every variant to
/// one response.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryProofError {
    /// The header did not parse.
    #[error("recovery proof header malformed")]
    Malformed,
    /// A version this crate does not speak.
    #[error("recovery proof header version unknown")]
    UnknownVersion,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("recovery proof outside the time window")]
    OutsideWindow,
    /// The stored recovery key or the new owner key is not a usable public
    /// key.
    #[error("recovery proof names an unusable key")]
    BadKey,
    /// The signature does not verify under the stored recovery key.
    #[error("recovery proof signature invalid")]
    BadSignature,
}

impl RecoveryProofError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownVersion => "unknownVersion",
            Self::OutsideWindow => "outsideWindow",
            Self::BadKey => "badKey",
            Self::BadSignature => "badSignature",
        }
    }
}

/// A parsed owner-change proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryOwnerProof {
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Ed25519 by the recovery key over the signed bytes.
    pub signature: [u8; SIGNATURE_LEN],
}

/// The bytes the recovery key signs.
#[must_use]
pub fn recovery_owner_change_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    new_owner_pk: &[u8; KEY_LEN],
    unix_ms: u64,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(RECOVERY_OWNER_CHANGE_DOMAIN.len() + CHANNEL_ID_LEN + KEY_LEN + 8);
    out.extend_from_slice(RECOVERY_OWNER_CHANGE_DOMAIN);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(new_owner_pk);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out
}

/// A channel's recovery key. Secret: `Debug` prints the public key only.
pub struct ChannelRecoveryKey {
    key: SigningKey,
}

impl core::fmt::Debug for ChannelRecoveryKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelRecoveryKey")
            .field("verifying_key", &self.key.verifying_key().to_bytes())
            .finish_non_exhaustive()
    }
}

impl ChannelRecoveryKey {
    /// The recovery key from its 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; KEY_LEN]) -> Self {
        Self {
            key: SigningKey::from_seed(seed),
        }
    }

    /// The recovery key of `channel_id` from the account's owned-channels
    /// list key `R`: `BLAKE3-derive-key(CHANNEL_RECOVERY_KEY_DOMAIN, R ‖
    /// channel_id)`.
    #[must_use]
    pub fn from_list_key(list_key: &[u8; KEY_LEN], channel_id: &[u8; CHANNEL_ID_LEN]) -> Self {
        let mut material = [0u8; KEY_LEN + CHANNEL_ID_LEN];
        material[..KEY_LEN].copy_from_slice(list_key);
        material[KEY_LEN..].copy_from_slice(channel_id);
        let mut seed =
            hash::blake3_derive_key_from_material(CHANNEL_RECOVERY_KEY_DOMAIN, &material);
        material.fill(0);
        let key = Self::from_seed(&seed);
        seed.fill(0);
        key
    }

    /// The public key the owner registers with `services/channels`.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// Sign an owner change of `channel_id` to `new_owner_pk` at `now_ms`.
    #[must_use]
    pub fn sign_owner_change(
        &self,
        channel_id: &[u8; CHANNEL_ID_LEN],
        new_owner_pk: &[u8; KEY_LEN],
        now_ms: u64,
    ) -> RecoveryOwnerProof {
        RecoveryOwnerProof {
            unix_ms: now_ms,
            signature: self
                .key
                .sign(&recovery_owner_change_bytes(
                    channel_id,
                    new_owner_pk,
                    now_ms,
                ))
                .to_bytes(),
        }
    }
}

/// Verify an owner change of `channel_id` to `new_owner_pk` under the stored
/// recovery key, at the verifier's clock `now_ms`.
///
/// # Errors
///
/// [`RecoveryProofError::BadKey`], [`RecoveryProofError::OutsideWindow`],
/// [`RecoveryProofError::BadSignature`].
pub fn verify_recovery_owner_change(
    proof: &RecoveryOwnerProof,
    channel_id: &[u8; CHANNEL_ID_LEN],
    new_owner_pk: &[u8; KEY_LEN],
    recovery_key: &[u8; KEY_LEN],
    now_ms: u64,
) -> Result<(), RecoveryProofError> {
    let key = VerifyingKey::from_bytes(recovery_key).map_err(|_| RecoveryProofError::BadKey)?;
    let new_owner =
        VerifyingKey::from_bytes(new_owner_pk).map_err(|_| RecoveryProofError::BadKey)?;
    if key.is_weak() || new_owner.is_weak() {
        return Err(RecoveryProofError::BadKey);
    }
    if now_ms.abs_diff(proof.unix_ms) > OWNER_PROOF_WINDOW_MS {
        return Err(RecoveryProofError::OutsideWindow);
    }
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| RecoveryProofError::BadSignature)?;
    key.verify(
        &recovery_owner_change_bytes(channel_id, new_owner_pk, proof.unix_ms),
        &signature,
    )
    .map_err(|_| RecoveryProofError::BadSignature)
}

impl RecoveryOwnerProof {
    /// The header value `v1.<unix_ms>.<sig_b64url>`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}",
            self.unix_ms,
            URL_SAFE_NO_PAD.encode(self.signature)
        )
    }

    /// Parse a header value. Bounded and fail-closed: three dot-separated
    /// fields, a decimal time, a 64-byte signature in base64url without
    /// padding, nothing else.
    ///
    /// # Errors
    ///
    /// [`RecoveryProofError::UnknownVersion`] or
    /// [`RecoveryProofError::Malformed`].
    pub fn parse(value: &str) -> Result<Self, RecoveryProofError> {
        if value.len() > RECOVERY_PROOF_MAX_HEADER_LEN {
            return Err(RecoveryProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(RecoveryProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(RecoveryProofError::Malformed)?;
        let signature = parts.next().ok_or(RecoveryProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(RecoveryProofError::Malformed);
        }
        if version != VERSION {
            return Err(RecoveryProofError::UnknownVersion);
        }
        if unix_ms.is_empty() || unix_ms.len() > 20 || !unix_ms.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(RecoveryProofError::Malformed);
        }
        let unix_ms = unix_ms
            .parse::<u64>()
            .map_err(|_| RecoveryProofError::Malformed)?;
        let signature: [u8; SIGNATURE_LEN] = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| RecoveryProofError::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| RecoveryProofError::Malformed)?;
        Ok(Self { unix_ms, signature })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const NOW: u64 = 1_800_000_000_000;

    fn new_owner() -> [u8; 32] {
        SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
    }

    #[test]
    fn a_signed_owner_change_verifies_and_nothing_else_does() {
        let key = ChannelRecoveryKey::from_list_key(&[0x41; 32], &CHANNEL);
        let rk = key.verifying_key().to_bytes();
        let proof = key.sign_owner_change(&CHANNEL, &new_owner(), NOW);
        assert_eq!(
            verify_recovery_owner_change(&proof, &CHANNEL, &new_owner(), &rk, NOW + 1000),
            Ok(())
        );
        // Another channel, another new owner, another recovery key.
        assert_eq!(
            verify_recovery_owner_change(&proof, &[0x0D; 16], &new_owner(), &rk, NOW),
            Err(RecoveryProofError::BadSignature)
        );
        let other_owner = SigningKey::from_seed(&[0x23; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_recovery_owner_change(&proof, &CHANNEL, &other_owner, &rk, NOW),
            Err(RecoveryProofError::BadSignature)
        );
        let other_rk = ChannelRecoveryKey::from_list_key(&[0x42; 32], &CHANNEL)
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_recovery_owner_change(&proof, &CHANNEL, &new_owner(), &other_rk, NOW),
            Err(RecoveryProofError::BadSignature)
        );
        // Outside the window, either way.
        for now in [
            NOW + OWNER_PROOF_WINDOW_MS + 1,
            NOW - OWNER_PROOF_WINDOW_MS - 1,
        ] {
            assert_eq!(
                verify_recovery_owner_change(&proof, &CHANNEL, &new_owner(), &rk, now),
                Err(RecoveryProofError::OutsideWindow)
            );
        }
        // An unusable key.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            verify_recovery_owner_change(&proof, &CHANNEL, &weak, &rk, NOW),
            Err(RecoveryProofError::BadKey)
        );
        assert_eq!(
            verify_recovery_owner_change(&proof, &CHANNEL, &new_owner(), &weak, NOW),
            Err(RecoveryProofError::BadKey)
        );
    }

    #[test]
    fn the_seed_is_the_derivation_over_the_list_key_and_channel() {
        let key = ChannelRecoveryKey::from_list_key(&[0x41; 32], &CHANNEL);
        let mut material = [0x41u8; 48];
        material[32..].copy_from_slice(&CHANNEL);
        let seed =
            hash::blake3_derive_key_from_material("lakko/channels/recovery-key/v2", &material);
        assert_eq!(
            key.verifying_key().to_bytes(),
            ChannelRecoveryKey::from_seed(&seed)
                .verifying_key()
                .to_bytes()
        );
        // Another channel of the same list, another list: another key.
        let other_channel = ChannelRecoveryKey::from_list_key(&[0x41; 32], &[0x0D; 16]);
        let other_list = ChannelRecoveryKey::from_list_key(&[0x40; 32], &CHANNEL);
        assert_ne!(key.verifying_key(), other_channel.verifying_key());
        assert_ne!(key.verifying_key(), other_list.verifying_key());
        // Distinct from the owner key's derivation context and proof domain.
        assert_ne!(CHANNEL_RECOVERY_KEY_DOMAIN, crate::owner::OWNER_KEY_DOMAIN);
        assert!(!RECOVERY_OWNER_CHANGE_DOMAIN.starts_with(crate::owner::OWNER_PROOF_DOMAIN));
        let dbg = format!("{key:?}");
        assert!(dbg.contains("verifying_key"));
    }

    #[test]
    fn the_header_round_trips_and_refuses_malformed_input() {
        let key = ChannelRecoveryKey::from_seed(&[0x31; 32]);
        let proof = key.sign_owner_change(&CHANNEL, &new_owner(), NOW);
        let header = proof.encode();
        assert!(header.len() <= RECOVERY_PROOF_MAX_HEADER_LEN);
        assert_eq!(RecoveryOwnerProof::parse(&header), Ok(proof));
        for bad in [
            "",
            "v1",
            "v1.1",
            "v1..AAAA",
            "v1.x.AAAA",
            "v1.1.AAAA",
            "v1.1.AAAA.extra",
        ] {
            assert_eq!(
                RecoveryOwnerProof::parse(bad),
                Err(RecoveryProofError::Malformed),
                "{bad}"
            );
        }
        assert_eq!(
            RecoveryOwnerProof::parse(&header.replacen("v1", "v2", 1)),
            Err(RecoveryProofError::UnknownVersion)
        );
        assert_eq!(
            RecoveryOwnerProof::parse(&"a".repeat(RECOVERY_PROOF_MAX_HEADER_LEN + 1)),
            Err(RecoveryProofError::Malformed)
        );
    }

    proptest! {
        #[test]
        fn parse_never_panics(s in ".{0,200}") {
            let _ = RecoveryOwnerProof::parse(&s);
        }

        #[test]
        fn a_proof_for_any_inputs_verifies_only_for_them(seed in any::<[u8; 32]>(), channel in any::<[u8; 16]>(), now in 0u64..u64::MAX / 2) {
            let key = ChannelRecoveryKey::from_seed(&seed);
            let rk = key.verifying_key().to_bytes();
            let proof = key.sign_owner_change(&channel, &new_owner(), now);
            if !key.verifying_key().is_weak() {
                prop_assert_eq!(verify_recovery_owner_change(&proof, &channel, &new_owner(), &rk, now), Ok(()));
            }
            let mut other = channel;
            other[0] ^= 1;
            prop_assert!(verify_recovery_owner_change(&proof, &other, &new_owner(), &rk, now).is_err());
        }
    }
}
