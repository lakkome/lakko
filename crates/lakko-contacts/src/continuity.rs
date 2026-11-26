// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The continuity mailbox of an account that reset its keys, and the binding
//! that announces it ([ADR-0101] sections 1 and 2).
//!
//! ## Why
//!
//! After an ADR-0073 key reset a peer and the reset identity share no secret,
//! and since ADR-0078 a mailbox address is derived from a mailbox key the
//! account seed alone yields, so a peer cannot compute the new identity's
//! mailbox. The resetting device therefore derives a **second** mailbox key,
//! the continuity mailbox, separate from the real one, and announces it for a
//! window in a binding signed by the new identity key. Discovery serves the
//! binding beside the identity bundle while it is unexpired (S2); a peer that
//! accepted the reset verifies it under the attested new key and writes the
//! successor Welcome there, signed with the ADR-0073 R.4 continuity reach key
//! ([`crate::reach::continuity_reach_seed`]).
//!
//! ## The key
//!
//! ```text
//! continuity_mailbox_seed = BLAKE3-derive-key("lakko/mailbox/continuity/v1", account_seed)
//! ```
//!
//! `account_seed` is the 32-byte Ed25519 seed of the account identity, the
//! input [`crate::mailbox::mailbox_seed`] takes, under a different context:
//! the continuity mailbox never shares a key with the real one. Its address
//! is [`crate::mailbox::mailbox_address`] of its public key, like any
//! mailbox, and its rows are ordinary sealed mailbox rows.
//!
//! ## Wire format (version 1)
//!
//! ```text
//! binding = version(1)=0x01 ‖ continuity_mailbox_pk(32) ‖ expires_at_ms(8 BE) ‖ sig(64)
//! sig     = Ed25519_identity(CONTINUITY_BINDING_DOMAIN ‖ version ‖ continuity_mailbox_pk ‖ expires_at_ms)
//! ```
//!
//! Fixed length [`CONTINUITY_BINDING_LEN`] (105). The version byte is
//! checked before the length.
//!
//! ## Verification
//!
//! [`ContinuityBinding::verify`] checks, in order: the identity key is a
//! usable public key; the mailbox key is a usable public key; the binding has
//! not expired (`now_ms < expires_at_ms`); the expiry is not more than
//! [`CONTINUITY_MAX_WINDOW_MS`] ahead of `now_ms`; the signature. A peer runs
//! it against the new identity key it took from the attested reset (ADR-0073
//! R.2), never against a key the binding names.
//!
//! The upper bound on the expiry is this module's addition to the ADR: the
//! ADR fixes the window at the DS journal retention, 30 days
//! ([`CONTINUITY_WINDOW_MS`], chosen, not measured); a binding claiming more
//! than that plus a day of clock slack is refused rather than honoured past
//! the window the operator was told about.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::mailbox::{MailboxKey, MAILBOX_KEY_LEN};

/// BLAKE3 derive-key context of the continuity mailbox seed. Frozen.
pub const CONTINUITY_MAILBOX_SEED_DOMAIN: &str = "lakko/mailbox/continuity/v1";

/// Domain separator the identity key signs a continuity binding under.
/// Frozen.
pub const CONTINUITY_BINDING_DOMAIN: &[u8] = b"lakko/mailbox/continuity-binding/v1\x00";

/// Schema version of a continuity binding.
pub const CONTINUITY_BINDING_VERSION_V1: u8 = 0x01;

const SIGNATURE_LEN: usize = 64;

/// Wire length of a binding: version, mailbox key, expiry, signature.
pub const CONTINUITY_BINDING_LEN: usize = 1 + MAILBOX_KEY_LEN + 8 + SIGNATURE_LEN;

/// The announcement window (ADR-0101 section 2): the DS journal retention,
/// 30 days. Chosen, not measured. A resetting device sets
/// `expires_at_ms = now + CONTINUITY_WINDOW_MS`.
pub const CONTINUITY_WINDOW_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// The latest expiry a verifier accepts, measured from its own clock: the
/// window plus one day of clock slack. Chosen, not measured.
pub const CONTINUITY_MAX_WINDOW_MS: u64 = CONTINUITY_WINDOW_MS + 24 * 60 * 60 * 1000;

const SIGNED_LEN: usize = CONTINUITY_BINDING_DOMAIN.len() + 1 + MAILBOX_KEY_LEN + 8;

/// Why a continuity binding was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ContinuityError {
    /// The binding is not [`CONTINUITY_BINDING_LEN`] bytes.
    #[error("continuity binding has the wrong length")]
    BadLength,
    /// The version byte is not [`CONTINUITY_BINDING_VERSION_V1`].
    #[error("continuity binding carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The identity key or the mailbox key is not a usable public key
    /// (malformed or of small order).
    #[error("continuity binding names an unusable key")]
    BadKey,
    /// The binding's expiry is at or before the verifier's clock.
    #[error("continuity binding has expired")]
    Expired,
    /// The binding's expiry lies more than [`CONTINUITY_MAX_WINDOW_MS`]
    /// ahead of the verifier's clock.
    #[error("continuity binding expires too far ahead")]
    ExpiryTooFar,
    /// The signature does not verify under the identity key given.
    #[error("continuity binding does not verify under this identity key")]
    BadSignature,
}

impl ContinuityError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadKey => "badKey",
            Self::Expired => "expired",
            Self::ExpiryTooFar => "expiryTooFar",
            Self::BadSignature => "badSignature",
        }
    }
}

/// The continuity mailbox seed of the account whose identity seed is
/// `account_seed`. Secret: whoever holds it reads the continuity mailbox.
#[must_use]
pub fn continuity_mailbox_seed(
    account_seed: &[u8; MAILBOX_KEY_LEN],
) -> SecretBytes<MAILBOX_KEY_LEN> {
    SecretBytes::new(blake3_derive_key_from_material(
        CONTINUITY_MAILBOX_SEED_DOMAIN,
        account_seed,
    ))
}

/// The continuity mailbox key of the account whose identity seed is
/// `account_seed`: claim it, read it and open its rows exactly as the real
/// mailbox key.
#[must_use]
pub fn continuity_mailbox_key(account_seed: &[u8; MAILBOX_KEY_LEN]) -> MailboxKey {
    MailboxKey::from_mailbox_seed(continuity_mailbox_seed(account_seed).as_bytes())
}

fn signed_bytes(mailbox_pk: &[u8; MAILBOX_KEY_LEN], expires_at_ms: u64) -> [u8; SIGNED_LEN] {
    let mut out = [0u8; SIGNED_LEN];
    let d = CONTINUITY_BINDING_DOMAIN.len();
    out[..d].copy_from_slice(CONTINUITY_BINDING_DOMAIN);
    out[d] = CONTINUITY_BINDING_VERSION_V1;
    out[d + 1..d + 1 + MAILBOX_KEY_LEN].copy_from_slice(mailbox_pk);
    out[d + 1 + MAILBOX_KEY_LEN..].copy_from_slice(&expires_at_ms.to_be_bytes());
    out
}

fn usable(key: &[u8; MAILBOX_KEY_LEN]) -> CoreResult<VerifyingKey, ContinuityError> {
    let key = VerifyingKey::from_bytes(key).map_err(|_| ContinuityError::BadKey)?;
    if key.is_weak() {
        return Err(ContinuityError::BadKey);
    }
    Ok(key)
}

/// The new identity key's signature over its continuity mailbox key and the
/// end of the announcement window.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ContinuityBinding {
    mailbox_pk: [u8; MAILBOX_KEY_LEN],
    expires_at_ms: u64,
    signature: [u8; SIGNATURE_LEN],
}

impl fmt::Debug for ContinuityBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContinuityBinding")
            .field("mailbox_pk", &self.mailbox_pk)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

impl ContinuityBinding {
    /// Sign `mailbox_pk` and `expires_at_ms` with the account identity key.
    /// Deterministic (Ed25519 signatures are).
    #[must_use]
    pub fn sign(
        identity: &SigningKey,
        mailbox_pk: &[u8; MAILBOX_KEY_LEN],
        expires_at_ms: u64,
    ) -> Self {
        Self {
            mailbox_pk: *mailbox_pk,
            expires_at_ms,
            signature: identity
                .sign(&signed_bytes(mailbox_pk, expires_at_ms))
                .to_bytes(),
        }
    }

    /// The binding of the account whose identity seed is `account_seed`:
    /// its continuity mailbox key signed by its identity key.
    #[must_use]
    pub fn for_account_seed(account_seed: &[u8; MAILBOX_KEY_LEN], expires_at_ms: u64) -> Self {
        let identity = SigningKey::from_seed(account_seed);
        Self::sign(
            &identity,
            &continuity_mailbox_key(account_seed).public(),
            expires_at_ms,
        )
    }

    /// The continuity mailbox key this binding names, **not verified**.
    #[must_use]
    pub const fn unverified_mailbox_pk(&self) -> &[u8; MAILBOX_KEY_LEN] {
        &self.mailbox_pk
    }

    /// The end of the announcement window, unix milliseconds.
    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// Encode as `version ‖ mailbox_pk ‖ expires_at_ms ‖ sig`.
    #[must_use]
    pub fn encode(&self) -> [u8; CONTINUITY_BINDING_LEN] {
        let mut out = [0u8; CONTINUITY_BINDING_LEN];
        out[0] = CONTINUITY_BINDING_VERSION_V1;
        out[1..=MAILBOX_KEY_LEN].copy_from_slice(&self.mailbox_pk);
        out[1 + MAILBOX_KEY_LEN..1 + MAILBOX_KEY_LEN + 8]
            .copy_from_slice(&self.expires_at_ms.to_be_bytes());
        out[1 + MAILBOX_KEY_LEN + 8..].copy_from_slice(&self.signature);
        out
    }

    /// Decode a binding. Layout only: the signature and the expiry are
    /// checked by [`Self::verify`], which knows the identity key and the
    /// clock.
    ///
    /// # Errors
    ///
    /// [`ContinuityError::UnknownVersion`] (checked first) and
    /// [`ContinuityError::BadLength`].
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, ContinuityError> {
        let (&version, rest) = bytes.split_first().ok_or(ContinuityError::BadLength)?;
        if version != CONTINUITY_BINDING_VERSION_V1 {
            return Err(ContinuityError::UnknownVersion(version));
        }
        if rest.len() != CONTINUITY_BINDING_LEN - 1 {
            return Err(ContinuityError::BadLength);
        }
        let (pk, rest) = rest.split_at(MAILBOX_KEY_LEN);
        let (expiry, sig) = rest.split_at(8);
        let mailbox_pk: [u8; MAILBOX_KEY_LEN] =
            pk.try_into().map_err(|_| ContinuityError::BadLength)?;
        let expires_at_ms =
            u64::from_be_bytes(expiry.try_into().map_err(|_| ContinuityError::BadLength)?);
        let signature: [u8; SIGNATURE_LEN] =
            sig.try_into().map_err(|_| ContinuityError::BadLength)?;
        Ok(Self {
            mailbox_pk,
            expires_at_ms,
            signature,
        })
    }

    /// Verify the binding against the identity key the caller trusts (the
    /// attested new key of a reset), at the caller's clock `now_ms`, and
    /// return the continuity mailbox key it names.
    ///
    /// # Errors
    ///
    /// [`ContinuityError::BadKey`], [`ContinuityError::Expired`],
    /// [`ContinuityError::ExpiryTooFar`], [`ContinuityError::BadSignature`].
    pub fn verify(
        &self,
        identity_pk: &[u8; MAILBOX_KEY_LEN],
        now_ms: u64,
    ) -> CoreResult<[u8; MAILBOX_KEY_LEN], ContinuityError> {
        let identity = usable(identity_pk)?;
        usable(&self.mailbox_pk)?;
        if now_ms >= self.expires_at_ms {
            return Err(ContinuityError::Expired);
        }
        if self.expires_at_ms - now_ms > CONTINUITY_MAX_WINDOW_MS {
            return Err(ContinuityError::ExpiryTooFar);
        }
        let signature =
            Signature::from_bytes(&self.signature).map_err(|_| ContinuityError::BadSignature)?;
        identity
            .verify(
                &signed_bytes(&self.mailbox_pk, self.expires_at_ms),
                &signature,
            )
            .map_err(|_| ContinuityError::BadSignature)?;
        Ok(self.mailbox_pk)
    }
}

/// Decode and verify a binding in one call: what a peer runs on the bytes
/// discovery served.
///
/// # Errors
///
/// Every error of [`ContinuityBinding::decode`] and
/// [`ContinuityBinding::verify`].
pub fn verify_continuity_binding(
    bytes: &[u8],
    identity_pk: &[u8; MAILBOX_KEY_LEN],
    now_ms: u64,
) -> CoreResult<[u8; MAILBOX_KEY_LEN], ContinuityError> {
    ContinuityBinding::decode(bytes)?.verify(identity_pk, now_ms)
}

/// The fuzz oracle behind `fuzz_api::continuity_binding_decode`: decode
/// `bytes`; whatever decodes re-encodes to the same bytes, never verifies
/// under a fixed identity key the fuzzer does not hold, and a binding that
/// key signs over the decoded mailbox key verifies at a time inside its
/// window. `None` when the decoder refuses; `Some(true)` when all hold.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let binding = ContinuityBinding::decode(bytes).ok()?;
    let round_trip = binding.encode().as_slice() == bytes;
    let fixed = SigningKey::from_seed(&[0x5D; 32]);
    let fixed_pub = fixed.verifying_key().to_bytes();
    let now = binding.expires_at_ms().saturating_sub(1);
    let forged = binding.verify(&fixed_pub, now).is_ok();
    let honest = if usable(binding.unverified_mailbox_pk()).is_ok() && binding.expires_at_ms() > 0 {
        ContinuityBinding::sign(
            &fixed,
            binding.unverified_mailbox_pk(),
            binding.expires_at_ms(),
        )
        .verify(&fixed_pub, now)
        .is_ok()
    } else {
        true
    };
    Some(round_trip && !forged && honest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox::{mailbox_address, mailbox_seed};
    use proptest::prelude::*;

    const SEED: [u8; 32] = [0x11; 32];
    const NOW: u64 = 1_800_000_000_000;

    fn hex(b: &[u8]) -> String {
        use core::fmt::Write as _;
        b.iter().fold(String::new(), |mut out, x| {
            let _ = write!(out, "{x:02x}");
            out
        })
    }

    #[test]
    fn the_continuity_mailbox_is_never_the_real_one() {
        let continuity = continuity_mailbox_seed(&SEED);
        assert_ne!(continuity.as_bytes(), mailbox_seed(&SEED).as_bytes());
        assert_ne!(
            CONTINUITY_MAILBOX_SEED_DOMAIN,
            crate::mailbox::MAILBOX_SEED_DOMAIN
        );
        let key = continuity_mailbox_key(&SEED);
        assert_ne!(key.public(), MailboxKey::from_account_seed(&SEED).public());
        assert_eq!(key.address(), mailbox_address(&key.public()));
    }

    #[test]
    fn golden_seed_and_binding() {
        assert_eq!(
            hex(continuity_mailbox_seed(&SEED).as_bytes()),
            GOLDEN_SEED_HEX
        );
        let binding = ContinuityBinding::for_account_seed(&SEED, NOW + CONTINUITY_WINDOW_MS);
        assert_eq!(binding.encode().len(), CONTINUITY_BINDING_LEN);
        assert_eq!(CONTINUITY_BINDING_LEN, 105);
        assert_eq!(hex(&binding.encode()), GOLDEN_BINDING_HEX);
    }

    const GOLDEN_SEED_HEX: &str =
        "4c8d81f76ab213bdc399a75d33159de82d5572c14d2736e331256965c5b1ace1";
    const GOLDEN_BINDING_HEX: &str = "01cccef9f9c5308ad4dbb9b4cf2ddd0f1c45ce0ae3a614315191ebe5f9831aa9ee000001a3b2db1800bf260a0450aff9e4794c90b831ebfc878ad40d3e7d04e4abbbb797a00eee79a26f4719fe51f03f6c55f707d2fa977750100dc7f310e53eda67b9ef4b1c815500";

    #[test]
    fn a_signed_binding_verifies_and_nothing_else_does() {
        let identity = SigningKey::from_seed(&SEED);
        let id_pub = identity.verifying_key().to_bytes();
        let binding = ContinuityBinding::for_account_seed(&SEED, NOW + CONTINUITY_WINDOW_MS);
        let wire = binding.encode();
        assert_eq!(
            verify_continuity_binding(&wire, &id_pub, NOW),
            Ok(continuity_mailbox_key(&SEED).public())
        );
        // Another identity key.
        let other = SigningKey::from_seed(&[0x12; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_continuity_binding(&wire, &other, NOW),
            Err(ContinuityError::BadSignature)
        );
        // Expired, at and after the expiry.
        assert_eq!(
            verify_continuity_binding(&wire, &id_pub, NOW + CONTINUITY_WINDOW_MS),
            Err(ContinuityError::Expired)
        );
        // Too far ahead of the verifier's clock.
        let far = ContinuityBinding::sign(
            &identity,
            &continuity_mailbox_key(&SEED).public(),
            NOW + CONTINUITY_MAX_WINDOW_MS + 1,
        );
        assert_eq!(far.verify(&id_pub, NOW), Err(ContinuityError::ExpiryTooFar));
        // Every byte flip is refused.
        for i in 0..wire.len() {
            let mut bad = wire;
            bad[i] ^= 0x01;
            assert!(
                verify_continuity_binding(&bad, &id_pub, NOW).is_err(),
                "byte {i}"
            );
        }
        // Unusable keys.
        let mut identity_point = [0u8; 32];
        identity_point[0] = 1;
        assert_eq!(
            verify_continuity_binding(&wire, &identity_point, NOW),
            Err(ContinuityError::BadKey)
        );
    }

    #[test]
    fn version_is_checked_before_length() {
        let wire = ContinuityBinding::for_account_seed(&SEED, NOW + 1).encode();
        assert_eq!(
            ContinuityBinding::decode(&[]),
            Err(ContinuityError::BadLength)
        );
        assert_eq!(
            ContinuityBinding::decode(&[2]),
            Err(ContinuityError::UnknownVersion(2))
        );
        assert_eq!(
            ContinuityBinding::decode(&wire[..wire.len() - 1]),
            Err(ContinuityError::BadLength)
        );
        let mut long = wire.to_vec();
        long.push(0);
        assert_eq!(
            ContinuityBinding::decode(&long),
            Err(ContinuityError::BadLength)
        );
        assert_eq!(fuzz_decode(&wire), Some(true));
    }

    #[test]
    fn the_signed_domain_is_distinct_from_the_mailbox_binding() {
        let a = CONTINUITY_BINDING_DOMAIN;
        let b = crate::mailbox::MAILBOX_BINDING_DOMAIN;
        assert!(!a.starts_with(b) && !b.starts_with(a));
        assert!(!a.starts_with(crate::x25519_binding::X25519_BINDING_DOMAIN));
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_never_forges(bytes in proptest::collection::vec(any::<u8>(), 0..130)) {
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
            let mut framed = vec![CONTINUITY_BINDING_VERSION_V1];
            framed.extend_from_slice(&bytes);
            if let Some(ok) = fuzz_decode(&framed) {
                prop_assert!(ok);
            }
        }

        #[test]
        fn a_signed_binding_verifies_inside_its_window(seed in any::<[u8; 32]>(), now in 0u64..u64::MAX / 2, offset in 1u64..CONTINUITY_MAX_WINDOW_MS) {
            let binding = ContinuityBinding::for_account_seed(&seed, now + offset);
            let id_pub = SigningKey::from_seed(&seed).verifying_key().to_bytes();
            prop_assert_eq!(
                verify_continuity_binding(&binding.encode(), &id_pub, now),
                Ok(continuity_mailbox_key(&seed).public())
            );
        }
    }
}
