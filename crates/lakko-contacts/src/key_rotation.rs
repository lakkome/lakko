// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Key-rotation envelope — the device that changes the account keys hands
//! the new recovery-phrase entropy to the account's other devices
//! ([ADR-0074](../../../docs/adr/0074-account-takeover-resistance.md)
//! section 3.3; `docs/ux/identity-recovery.md` §18.1 and §18.3).
//!
//! ## Why this exists
//!
//! "This device was stolen" removes the stolen device and changes the
//! account keys while the person's other devices keep their chats. Those
//! kept devices need the new account seed, and they must get it without the
//! person typing a phrase on each of them. This envelope is the carrier: the
//! rotating device posts it into the account's own DeviceGroup, every kept
//! device reads it, and each adopts the new seed once the server-side change
//! has taken effect.
//!
//! ## What travels, and where
//!
//! `{prepared_at_ms, new_entropy, supersession_proof}`:
//!
//! * `new_entropy` — the 16 bytes of BIP39 entropy of the NEW recovery
//!   phrase. The phrase, the 64-byte seed and the account identity all
//!   derive from it (ADR-0017, ADR-0072).
//! * `supersession_proof` — the 145-byte proof of
//!   `lakko_transparency::supersession`, signed by the OLD account identity
//!   key over the new one. Carried opaque here (this crate does not depend on
//!   the transparency crate); the receiver verifies it.
//!
//! It rides as an MLS application message in the account's **DeviceGroup**,
//! inside the existing `ServiceEnvelope` carrier, classified by
//! [`crate::incoming_envelope`] so it is never rendered. The server relays
//! ciphertext under the DeviceGroup's epoch secret and learns nothing
//! (invariants #1, #2).
//!
//! ## Why it is safe to carry the entropy here, and what must be true first
//!
//! The entropy is the account. Whoever reads this envelope owns the account
//! from the moment the rotation commits. Two facts make the DeviceGroup an
//! acceptable carrier, and **both are the SENDER's obligations** — nothing in
//! this module can check them:
//!
//! 1. **The stolen device is out, and the epoch has advanced past it, BEFORE
//!    this envelope is sealed.** An MLS application message is readable by
//!    the members of the epoch it is sealed at and by nobody else. A device
//!    removed by a `Remove` commit does not hold the secrets of the epoch
//!    that commit produces (RFC 9420 forward secrecy on removal). So the
//!    rotating device must first commit the removal of every device it does
//!    not trust, see that commit win the journal, and only then compose and
//!    send this envelope at the new epoch. An envelope sealed at an epoch the
//!    stolen device still holds hands the thief the new account. This is an
//!    ordering rule of the weight of C-61 (ADR-0074 section 3.2), and like
//!    C-61 it deserves a test on every client that asserts the epoch of the
//!    emitted message, not the order of two calls.
//! 2. **It is sent only in the DeviceGroup.** The DeviceGroup's members are
//!    the account's own devices and nothing else (ADR-0007). The envelope
//!    must never be posted into a conversation group, where other people's
//!    devices are members.
//!
//! What is NOT relied on: any key derived from the old account seed (the
//! thief holds it — ADR-0074 "Alternatives rejected"), and the server.
//!
//! ## What the envelope does NOT establish
//!
//! Decoding proves a shape. **Before a kept device stores the envelope as
//! pending it must check, all three:**
//!
//! * the proof's old key is the account key this device holds;
//! * the proof verifies (signature, validity window, not expired);
//! * the account identity derived from `new_entropy` has exactly the proof's
//!   new key.
//!
//! Both FFI surfaces expose that check as one call so no client re-derives
//! it. A device then changes NOTHING until it sees the server-side commit
//! (the published bundle carries the proof's new key, or its own session is
//! refused as superseded); a pending envelope whose key has not become the
//! account's within [`KEY_ROTATION_PENDING_MAX_AGE_MS`] is dropped. A device
//! holding several pending envelopes keeps them keyed by the proof's new
//! key: which one becomes the account's is decided by the server-side
//! commit, never by arrival order or by `prepared_at_ms`.
//!
//! The MLS sender is one of the account's own devices by construction of the
//! DeviceGroup; this envelope carries no identity and no role of its own.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       KEY_ROTATION_ENVELOPE_DOMAIN_V1 || version || prepared_at_ms || new_entropy || supersession_proof
//!      └──────── 22 bytes, ASCII ───────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └── 16 B ───┘ └────── 145 B ──────┘
//!
//!                     "lakko/key-rotation/v1\x00"
//! ```
//!
//! Exactly [`KEY_ROTATION_ENVELOPE_LEN`] bytes (192). The version byte is
//! judged before the length, so a later revision that changes the length
//! reports as a version and not as a truncation.
//!
//! ## The entropy in memory
//!
//! [`KeyRotation`] holds secret bytes. It implements neither `Copy` nor a
//! derived `Debug`: the `Debug` output redacts the entropy, and no error of
//! this module carries any payload byte. Callers should drop a decoded value
//! as soon as it is stored under the device key.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it, since
//! [`crate::incoming_envelope`] dispatches on a byte-prefix match.

use core::fmt;
use core::result::Result as CoreResult;

use thiserror::Error;

/// 22-byte domain separator prepended to the key-rotation payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with, mirroring the sibling envelopes. The printable
/// prefix is `lakko/key-rotation/v1`.
///
/// **The `v1` names the envelope FAMILY, not the schema version.** These
/// bytes are frozen: [`crate::incoming_envelope`] dispatches on this exact
/// prefix. The in-payload version byte is the knob that moves.
pub const KEY_ROTATION_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/key-rotation/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const KEY_ROTATION_VERSION_V1: u8 = 0x01;

/// Length of the BIP39 entropy of a 12-word recovery phrase (ADR-0072).
pub const KEY_ROTATION_ENTROPY_LEN: usize = 16;

/// Length of a supersession proof (`lakko_transparency::SUPERSESSION_PROOF_LEN`,
/// repeated here because this crate does not depend on that one; the FFI
/// crates, which depend on both, assert the two are equal).
pub const KEY_ROTATION_PROOF_LEN: usize = 145;

/// Bytes after the separator: version, `prepared_at_ms`, entropy, proof.
const PAYLOAD_LEN: usize = 1 + 8 + KEY_ROTATION_ENTROPY_LEN + KEY_ROTATION_PROOF_LEN;

/// The one length of a well-formed envelope: 192 bytes.
pub const KEY_ROTATION_ENVELOPE_LEN: usize = KEY_ROTATION_ENVELOPE_DOMAIN_V1.len() + PAYLOAD_LEN;

/// How long a kept device holds a pending envelope whose key has not become
/// the account's before it drops it: 14 days, the validity the rotating
/// device gives the supersession proof (ADR-0074 section 3.3). Chosen, not
/// measured.
pub const KEY_ROTATION_PENDING_MAX_AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// A decoded key-rotation envelope.
///
/// **Holds the new account's entropy.** See the module docs for what a
/// receiver must check before it keeps one, and for the ordering the sender
/// owes.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyRotation {
    /// Rotating-device clock, UNIX milliseconds, when the envelope was
    /// prepared. Used only to age a pending envelope out
    /// ([`KEY_ROTATION_PENDING_MAX_AGE_MS`]); it orders nothing and
    /// authorises nothing.
    pub prepared_at_ms: u64,
    /// BIP39 entropy of the NEW recovery phrase. Secret.
    pub new_entropy: [u8; KEY_ROTATION_ENTROPY_LEN],
    /// The supersession proof, old account key over new, opaque here.
    pub supersession_proof: [u8; KEY_ROTATION_PROOF_LEN],
}

impl fmt::Debug for KeyRotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyRotation")
            .field("prepared_at_ms", &self.prepared_at_ms)
            .field("new_entropy", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl KeyRotation {
    /// `true` when a pending envelope prepared at `prepared_at_ms` is too old
    /// to keep at `now_ms`. A `prepared_at_ms` in the future (the rotating
    /// device's clock ran ahead) is not expired: the bound is on age, and a
    /// negative age saturates to zero.
    #[must_use]
    pub const fn pending_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.prepared_at_ms) > KEY_ROTATION_PENDING_MAX_AGE_MS
    }
}

/// Compose a key-rotation envelope.
///
/// The output is the **inner** plaintext handed to the DeviceGroup's MLS
/// application-message send path. Infallible: every [`KeyRotation`] is
/// encodable. **The caller owes the ordering of the module docs**: the
/// untrusted device removed and the epoch advanced before this is sealed,
/// and the DeviceGroup as the only destination.
#[must_use]
pub fn compose_key_rotation(rotation: &KeyRotation) -> Vec<u8> {
    let mut out = Vec::with_capacity(KEY_ROTATION_ENVELOPE_LEN);
    out.extend_from_slice(KEY_ROTATION_ENVELOPE_DOMAIN_V1);
    out.push(KEY_ROTATION_VERSION_V1);
    out.extend_from_slice(&rotation.prepared_at_ms.to_be_bytes());
    out.extend_from_slice(&rotation.new_entropy);
    out.extend_from_slice(&rotation.supersession_proof);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(rotation))` — the domain separator matched and the envelope
///   is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed. The caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`KeyRotationError::Truncated`] — nothing follows the separator.
/// * [`KeyRotationError::UnknownVersion`] — the version byte is not
///   [`KEY_ROTATION_VERSION_V1`]. Checked BEFORE the length.
/// * [`KeyRotationError::BadLength`] — the envelope is not exactly
///   [`KEY_ROTATION_ENVELOPE_LEN`] bytes.
pub fn try_unwrap_key_rotation(
    plaintext: &[u8],
) -> CoreResult<Option<KeyRotation>, KeyRotationError> {
    let Some(payload) = plaintext.strip_prefix(KEY_ROTATION_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(KeyRotationError::Truncated)?;
    if version != KEY_ROTATION_VERSION_V1 {
        return Err(KeyRotationError::UnknownVersion(version));
    }
    if payload.len() != PAYLOAD_LEN {
        return Err(KeyRotationError::BadLength {
            got: plaintext.len(),
        });
    }
    let (stamp, rest) = rest.split_at(8);
    let (entropy, proof) = rest.split_at(KEY_ROTATION_ENTROPY_LEN);
    let bad_length = || KeyRotationError::BadLength {
        got: plaintext.len(),
    };
    Ok(Some(KeyRotation {
        prepared_at_ms: u64::from_be_bytes(stamp.try_into().map_err(|_| bad_length())?),
        new_entropy: entropy.try_into().map_err(|_| bad_length())?,
        supersession_proof: proof.try_into().map_err(|_| bad_length())?,
    }))
}

/// Strict standalone decoder: a non-key-rotation plaintext is a hard error.
///
/// # Errors
///
/// * [`KeyRotationError::WrongDomain`] — `bytes` does not start with
///   [`KEY_ROTATION_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_key_rotation`] returns.
pub fn decode_key_rotation(bytes: &[u8]) -> CoreResult<KeyRotation, KeyRotationError> {
    try_unwrap_key_rotation(bytes)?.ok_or(KeyRotationError::WrongDomain)
}

/// Why a key-rotation envelope was refused.
///
/// A reason code: no variant carries a payload byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeyRotationError {
    /// The plaintext does not carry the key-rotation domain separator. Only
    /// from [`decode_key_rotation`]; [`try_unwrap_key_rotation`] returns
    /// `Ok(None)`.
    #[error("plaintext does not carry the key-rotation domain separator")]
    WrongDomain,
    /// Nothing follows the separator.
    #[error("key-rotation envelope is truncated")]
    Truncated,
    /// The version byte is not [`KEY_ROTATION_VERSION_V1`].
    #[error("key-rotation envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The envelope is not exactly [`KEY_ROTATION_ENVELOPE_LEN`] bytes.
    #[error("key-rotation envelope is {got} bytes long")]
    BadLength {
        /// The whole plaintext's length.
        got: usize,
    },
}

impl KeyRotationError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::Truncated => "truncated",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadLength { .. } => "badLength",
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const ENTROPY: [u8; KEY_ROTATION_ENTROPY_LEN] = [0xE7; KEY_ROTATION_ENTROPY_LEN];

    fn proof() -> [u8; KEY_ROTATION_PROOF_LEN] {
        let mut p = [0u8; KEY_ROTATION_PROOF_LEN];
        for (i, b) in p.iter_mut().enumerate() {
            *b = u8::try_from(i % 251).unwrap();
        }
        p
    }

    fn rotation(at: u64) -> KeyRotation {
        KeyRotation {
            prepared_at_ms: at,
            new_entropy: ENTROPY,
            supersession_proof: proof(),
        }
    }

    #[test]
    fn compose_then_decode_round_trips() {
        for at in [0, 1, 1_700_000_000_000, u64::MAX] {
            let wire = compose_key_rotation(&rotation(at));
            assert_eq!(wire.len(), KEY_ROTATION_ENVELOPE_LEN);
            assert_eq!(decode_key_rotation(&wire).unwrap(), rotation(at));
            assert_eq!(try_unwrap_key_rotation(&wire).unwrap(), Some(rotation(at)));
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        assert_eq!(KEY_ROTATION_ENVELOPE_DOMAIN_V1.len(), 22);
        assert_eq!(KEY_ROTATION_ENVELOPE_LEN, 192);
        let wire = compose_key_rotation(&rotation(0x0102_0304_0506_0708));
        assert_eq!(&wire[..22], b"lakko/key-rotation/v1\x00");
        assert_eq!(wire[22], 0x01);
        assert_eq!(&wire[23..31], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&wire[31..47], &ENTROPY);
        assert_eq!(&wire[47..], &proof());
    }

    #[test]
    fn non_key_rotation_plaintext_falls_through() {
        for plain in [
            &b""[..],
            b"hello",
            b"lakko/key-rotation/v1",
            b"lakko/key-rotation/v2\x00",
            b"{\"v\":1}",
        ] {
            assert_eq!(try_unwrap_key_rotation(plain), Ok(None), "{plain:?}");
            assert_eq!(
                decode_key_rotation(plain),
                Err(KeyRotationError::WrongDomain)
            );
        }
    }

    #[test]
    fn bare_separator_is_truncated_not_a_fall_through() {
        assert_eq!(
            try_unwrap_key_rotation(KEY_ROTATION_ENVELOPE_DOMAIN_V1),
            Err(KeyRotationError::Truncated)
        );
    }

    #[test]
    fn every_other_length_is_refused() {
        let wire = compose_key_rotation(&rotation(7));
        for cut in (KEY_ROTATION_ENVELOPE_DOMAIN_V1.len() + 1)..wire.len() {
            assert_eq!(
                try_unwrap_key_rotation(&wire[..cut]),
                Err(KeyRotationError::BadLength { got: cut }),
                "cut at {cut}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_key_rotation(&long),
            Err(KeyRotationError::BadLength {
                got: KEY_ROTATION_ENVELOPE_LEN + 1
            })
        );
    }

    #[test]
    fn unknown_versions_are_refused_before_the_length_is_consulted() {
        let mut wire = compose_key_rotation(&rotation(7));
        wire[KEY_ROTATION_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert_eq!(
            try_unwrap_key_rotation(&wire),
            Err(KeyRotationError::UnknownVersion(0x02))
        );
        // A later, shorter revision reports as a version, not as a length.
        let mut v2 = KEY_ROTATION_ENVELOPE_DOMAIN_V1.to_vec();
        v2.extend_from_slice(&[0x02, 0xAA]);
        assert_eq!(
            try_unwrap_key_rotation(&v2),
            Err(KeyRotationError::UnknownVersion(0x02))
        );
        let mut zero = KEY_ROTATION_ENVELOPE_DOMAIN_V1.to_vec();
        zero.push(0x00);
        assert_eq!(
            try_unwrap_key_rotation(&zero),
            Err(KeyRotationError::UnknownVersion(0x00))
        );
    }

    #[test]
    fn debug_and_errors_never_show_the_entropy() {
        let shown = format!("{:?}", rotation(1));
        assert!(shown.contains("redacted"));
        assert!(!shown.contains("231"), "0xE7 = 231 must not be printed");
        assert!(!shown.to_lowercase().contains("e7"));
        for e in [
            KeyRotationError::WrongDomain,
            KeyRotationError::Truncated,
            KeyRotationError::UnknownVersion(0xE7),
            KeyRotationError::BadLength { got: 191 },
        ] {
            let text = e.to_string();
            assert!(!text.contains("231"));
        }
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(KeyRotationError::WrongDomain.code(), "wrongDomain");
        assert_eq!(KeyRotationError::Truncated.code(), "truncated");
        assert_eq!(KeyRotationError::UnknownVersion(9).code(), "unknownVersion");
        assert_eq!(KeyRotationError::BadLength { got: 1 }.code(), "badLength");
    }

    #[test]
    fn a_pending_envelope_expires_by_age_only() {
        let r = rotation(1_000_000);
        assert!(!r.pending_expired(0), "a future stamp is not expired");
        assert!(!r.pending_expired(1_000_000));
        assert!(!r.pending_expired(1_000_000 + KEY_ROTATION_PENDING_MAX_AGE_MS));
        assert!(r.pending_expired(1_000_000 + KEY_ROTATION_PENDING_MAX_AGE_MS + 1));
        assert!(!rotation(u64::MAX).pending_expired(u64::MAX));
        assert_eq!(KEY_ROTATION_PENDING_MAX_AGE_MS, 1_209_600_000);
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let siblings: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
        ];
        for d in siblings {
            assert_ne!(*d, KEY_ROTATION_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(KEY_ROTATION_ENVELOPE_DOMAIN_V1)
                    && !KEY_ROTATION_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?} and the key-rotation separator must not prefix each other"
            );
        }
    }

    #[test]
    fn key_rotation_does_not_cross_dispatch_as_a_sibling() {
        let wire = compose_key_rotation(&rotation(1_700_000_000_000));
        assert_eq!(
            crate::group_closed::try_unwrap_group_closed(&wire),
            Ok(None)
        );
        assert_eq!(crate::group_name::try_unwrap_group_name(&wire), Ok(None));
        assert_eq!(crate::pin::try_unwrap_pin(&wire), Ok(None));
        assert_eq!(
            crate::committer_beacon::try_unwrap_committer_beacon(&wire),
            Ok(None)
        );
        assert_eq!(
            crate::push_routing::try_unwrap_push_routing_secret(&wire),
            Ok(None)
        );
        // And a sibling's wire is not a key rotation.
        let closed = crate::group_closed::compose_group_closed(&crate::group_closed::GroupClosed {
            closed_at_ms: 1,
            successor_group_id: None,
        });
        assert_eq!(try_unwrap_key_rotation(&closed), Ok(None));
    }

    /// The dispatcher's own view: a key rotation is its own kind and is
    /// never the intro-message fallback.
    #[test]
    fn the_dispatcher_classifies_it_and_never_renders_it() {
        use crate::incoming_envelope::{
            dispatch_incoming_envelope, DispatchError, IncomingEnvelope,
        };
        let wire = compose_key_rotation(&rotation(42));
        match dispatch_incoming_envelope(&wire).unwrap() {
            IncomingEnvelope::KeyRotation(got) => assert_eq!(got, rotation(42)),
            other => panic!("expected KeyRotation, got {other:?}"),
        }
        assert_eq!(
            dispatch_incoming_envelope(&wire[..100]),
            Err(DispatchError::KeyRotation(KeyRotationError::BadLength {
                got: 100
            }))
        );
    }
}
