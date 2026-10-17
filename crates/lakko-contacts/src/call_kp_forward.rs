// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call KeyPackage-forward envelope domain separator
//! (Phase 3c3-kp-forward — sibling of
//! [`crate::call_welcome`]).
//!
//! Multi-party link-call orchestration glue. The first
//! joiner of a `CallInviteCapability` founds the call MLS
//! group with the owner added via the embedded KP (see
//! [`crate::call_invite::CallInviteCapability`] +
//! `lakko_calls::link_invite::accept_call_invite`). For every
//! **subsequent** joiner (the 3rd, 4th, … participants in a
//! group call) the topology flips: the subsequent joiner
//! does NOT found a new MLS group — that would split the
//! call into disjoint groups deriving the same `CallKey` but
//! not sharing an epoch. Instead the subsequent joiner ferries
//! its own fresh `KeyPackage` to the call group's current
//! committer (the first joiner or, after handover, the
//! owner), who runs an MLS Add proposal + commit and ferries
//! a Welcome back via [`crate::call_welcome`].
//!
//! ## Why a sibling envelope rather than a flag on call-welcome
//!
//! Two reasons:
//!
//!   1. **Wire-bound dispatch** — the owner-side polling loop
//!      reads `try_unwrap_call_welcome` first; if that returns
//!      `Some`, the loop drives `Group::join_from_welcome`.
//!      For multi-party orchestration the same loop also
//!      reads `try_unwrap_call_kp_forward`; if that returns
//!      `Some`, the loop drives `Group::add_member` + ferries
//!      back the resulting Welcome to the subsequent joiner.
//!      Two distinct domains keep the dispatch table flat —
//!      no in-payload type-byte to misparse.
//!   2. **Floor difference** — a KeyPackage is typically
//!      256–1024 bytes (vs. a Welcome's ~200+ bytes for a
//!      2-member group), so the truncation floor is
//!      different. Folding them into one envelope would
//!      either weaken the welcome floor or reject legitimate
//!      KPs.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload = CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 || kp_bytes
//!                      └────── 25 bytes, ASCII ──────┘
//!
//!                       "lakko/call-keypackage/v1\x00"
//! ```
//!
//! The 25-byte domain separator goes inside the HPKE-sealed
//! plaintext of the `ContactRequest.message_ciphertext`, NOT
//! into any visible header. The transport (HPKE-seal +
//! stranger-inbox routing) is unchanged from
//! [`crate::call_welcome`]; the only difference is which
//! prefix the owner-side polling loop matches against.
//!
//! ## The joiner's reply grant (ADR-0060 §6)
//!
//! Since ADR-0060 the joiner's mailbox is closed, so the committer could not
//! ferry the Welcome back without one of the joiner's reach keys. This
//! envelope carries no key of its own because its carrier already does: the
//! joiner passes its **`reply` reach grant** to [`crate::request::compose`]
//! beside this envelope, and it rides inside the same sealed, signed
//! `ContactRequest` plaintext ([`crate::request::OpenedRequest::reply_grant`]).
//! The owner (or the call's acting committer, to whom the owner hands it)
//! writes the [`crate::call_welcome`] envelope to the joiner's mailbox under
//! it. A grant of any other class is refused on compose and fails the open
//! there, so the check lives once, in the carrier.
//!
//! ## Cross-kind isolation
//!
//! The separator MUST be byte-distinct from every other
//! envelope domain prefix in this crate
//! (`CALL_WELCOME_ENVELOPE_DOMAIN_V1` notably) — the
//! [`tests::separator_distinct_from_sibling_domains`] test
//! asserts this. The length contrast (25 vs. 22) is a
//! deliberate cue but the actual dispatch rule is a
//! byte-prefix match.

use core::result::Result as CoreResult;

use crate::call_welcome::CallWelcomeError;

/// 25-byte domain separator the subsequent joiner prepends to
/// its `KeyPackage` bytes before HPKE-sealing them into a
/// [`crate::request::ContactRequest`] envelope.
///
/// Trailing `\x00` byte makes the prefix UTF-8-text-prefix-
/// impossible so an intro message can never start with it.
/// Length is intentionally distinct from
/// [`crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1`]
/// (25 vs. 22) as a defence-in-depth cue, though the actual
/// dispatch rule is a byte-prefix match, not a length check.
pub const CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/call-keypackage/v1\x00";

/// Minimum plausible KeyPackage length.
///
/// OpenMLS produces KPs in the 400–1024-byte range
/// depending on credential + signature size; we pin the
/// floor at 256 bytes (well below the practical minimum)
/// so a truncated payload surfaces as a typed error rather
/// than a confused `Some(too_short_kp)` that the MLS layer
/// would later reject opaquely.
pub const CALL_KP_FORWARD_MIN_PAYLOAD_LEN: usize = 256;

/// Wrap a fresh MLS KeyPackage in the call-KP-forward
/// envelope.
///
/// The output is the **inner** plaintext the subsequent
/// joiner hands to [`crate::request::compose`] as the
/// intro-message argument. The transport layer (HPKE-seal +
/// Ed25519 signature on the envelope) is unchanged from the
/// call-welcome scenario; only the embedded domain separator
/// differs.
///
/// Returns a fresh `Vec<u8>` with the 25-byte prefix +
/// `kp_bytes`. The caller is responsible for HPKE-sealing
/// the result.
///
/// # Errors
///
/// None — wrap is infallible. Bounds-checks happen at the
/// codec layer (`request::compose`).
#[must_use]
pub fn wrap_call_kp_forward(kp_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1.len() + kp_bytes.len());
    out.extend_from_slice(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1);
    out.extend_from_slice(kp_bytes);
    out
}

/// Discriminated unwrap. Returns `Ok(Some(kp_bytes))` when
/// `plaintext` starts with the call-KP-forward domain
/// separator and the trailing payload is at least
/// [`CALL_KP_FORWARD_MIN_PAYLOAD_LEN`] bytes long.
///
/// Returns `Ok(None)` when the prefix doesn't match — the
/// owner-side polling loop falls through to the next
/// dispatch step (typically `try_unwrap_call_welcome` for
/// the first-joiner branch, then the intro-message
/// rendering path).
///
/// Reuses [`CallWelcomeError`] from the sibling
/// [`crate::call_welcome`] module — both envelopes have the
/// same single failure mode (truncated payload) and a
/// shared error type keeps the FFI surface compact.
///
/// # Errors
///
/// * [`CallWelcomeError::TruncatedPayload`] when the prefix
///   matched but the trailing payload is shorter than
///   [`CALL_KP_FORWARD_MIN_PAYLOAD_LEN`]. Log + drop.
pub fn try_unwrap_call_kp_forward(plaintext: &[u8]) -> CoreResult<Option<&[u8]>, CallWelcomeError> {
    if !plaintext.starts_with(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() < CALL_KP_FORWARD_MIN_PAYLOAD_LEN {
        return Err(CallWelcomeError::TruncatedPayload {
            min: CALL_KP_FORWARD_MIN_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    Ok(Some(payload))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1;

    #[test]
    fn wrap_then_unwrap_round_trips() {
        let kp = vec![0xCDu8; 512];
        let wrapped = wrap_call_kp_forward(&kp);
        assert!(wrapped.starts_with(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1));
        assert_eq!(
            wrapped.len(),
            CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1.len() + kp.len(),
        );

        let unwrapped = try_unwrap_call_kp_forward(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped, kp.as_slice());
    }

    #[test]
    fn unwrap_returns_none_for_intro_message_plaintext() {
        let intro = b"hi alice, it's bob -- wanna call?";
        let result = try_unwrap_call_kp_forward(intro).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unwrap_returns_none_for_call_welcome_envelope() {
        // The owner-side polling loop reads
        // `try_unwrap_call_welcome` first; if that returns
        // `Some`, the KP-forward parser is never invoked.
        // But the dispatch contract requires that even if a
        // call-welcome plaintext is fed to this parser
        // directly, it surfaces `None` (no false-positive
        // dispatch to add_member instead of join_from_welcome).
        let mut welcome_envelope = Vec::new();
        welcome_envelope.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        welcome_envelope.extend_from_slice(&[0xABu8; 300]);
        let result = try_unwrap_call_kp_forward(&welcome_envelope).unwrap();
        assert!(
            result.is_none(),
            "call-welcome must NOT cross-dispatch as KP-forward",
        );
    }

    #[test]
    fn unwrap_rejects_truncated_payload() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 100]); // 100 bytes < 256-byte floor

        let err = try_unwrap_call_kp_forward(&wrapped).unwrap_err();
        assert_eq!(
            err,
            CallWelcomeError::TruncatedPayload { min: 256, got: 100 },
        );
    }

    #[test]
    fn unwrap_accepts_exactly_min_payload_len() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0xCDu8; CALL_KP_FORWARD_MIN_PAYLOAD_LEN]);
        let unwrapped = try_unwrap_call_kp_forward(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped.len(), CALL_KP_FORWARD_MIN_PAYLOAD_LEN);
    }

    #[test]
    fn empty_plaintext_returns_none_not_error() {
        let result = try_unwrap_call_kp_forward(&[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn domain_separator_is_25_bytes_and_ends_with_null() {
        // Format pin. The wire format is frozen.
        assert_eq!(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1.len(), 25);
        assert_eq!(*CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        let printable = &CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1[..24];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/call-keypackage/v1",
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        // Cross-kind isolation across every envelope-domain
        // constant in the crate. A future addition that
        // accidentally collides with one of these surfaces
        // here.
        let other_domains: &[&[u8]] = &[
            CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(
                *d, CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
                "call-KP-forward separator must be byte-distinct from {d:?}",
            );
        }
    }

    #[test]
    fn wrap_preserves_arbitrary_kp_bytes_verbatim() {
        // Surface if a future refactor accidentally
        // re-encodes / pads / truncates the KP.
        let kp: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
        let wrapped = wrap_call_kp_forward(&kp);
        let unwrapped = try_unwrap_call_kp_forward(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped, kp.as_slice());
    }

    /// ADR-0060 §6: a cold-start KeyPackage forward reaches the owner with
    /// the joiner's reply grant beside it, in the one sealed carrier; a grant
    /// of any other class never leaves the joiner.
    #[test]
    fn a_cold_start_forward_carries_the_joiners_reply_grant() {
        use crate::reach::{ReachClass, ReachGrant};
        use crate::request::{compose, decode_and_verify, open_envelope};
        use lakko_crypto::agreement::SecretKey as X25519SecretKey;
        use lakko_crypto::signing::SigningKey;

        let owner = X25519SecretKey::from_bytes([0xCD; 32]);
        let joiner = SigningKey::from_seed(&[0x51; 32]);
        let grant = ReachGrant::derive(&[0x51; 32], ReachClass::Reply, &[0x53; 16], 0).unwrap();
        let wrapped = wrap_call_kp_forward(&[0xCD; 512]);
        let wire = compose(
            &joiner,
            &[0x54; 16],
            &owner.public_key(),
            [0; 32],
            0,
            b"",
            None,
            Some(&grant),
            None,
            &wrapped,
        )
        .unwrap();
        let opened = open_envelope(&decode_and_verify(&wire).unwrap(), &owner).unwrap();
        assert_eq!(opened.reply_grant, Some(grant));
        assert_eq!(
            try_unwrap_call_kp_forward(&opened.intro).unwrap().unwrap(),
            &[0xCD; 512][..]
        );

        let contact = ReachGrant::derive(&[0x51; 32], ReachClass::Contact, b"", 0).unwrap();
        assert!(compose(
            &joiner,
            &[0x54; 16],
            &owner.public_key(),
            [0; 32],
            0,
            b"",
            None,
            Some(&contact),
            None,
            &wrapped,
        )
        .is_err());
    }

    #[test]
    fn welcome_and_kp_forward_round_trips_do_not_cross_dispatch() {
        // Strongest property: a payload built with one
        // wrap helper MUST surface as `None` (not the
        // sibling envelope) when fed to the other unwrap
        // helper. Catches the failure mode where a future
        // change to one separator accidentally aligns the
        // prefix with the other.
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};

        let welcome_bytes = vec![0xEEu8; 256];
        let wrapped_welcome = wrap_call_welcome(&welcome_bytes);
        // Welcome envelope MUST NOT decode as KP-forward.
        assert!(try_unwrap_call_kp_forward(&wrapped_welcome)
            .unwrap()
            .is_none());

        let kp_bytes = vec![0xFFu8; 512];
        let wrapped_kp = wrap_call_kp_forward(&kp_bytes);
        // KP-forward envelope MUST NOT decode as Welcome.
        assert!(try_unwrap_call_welcome(&wrapped_kp).unwrap().is_none());
    }
}
