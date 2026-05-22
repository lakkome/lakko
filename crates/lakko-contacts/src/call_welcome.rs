// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call-Welcome envelope domain separator (Phase 3c3-welcome-ferry).
//!
//! Joiner-side glue for ferrying the
//! [`crate::call_invite::CallInviteCapability`] accept flow's
//! `welcome_for_owner` bytes back to the owner over the
//! **existing** [`crate::request::ContactRequest`] transport
//! (HPKE-sealed envelope routed via the owner's
//! [`crate::pseudonym::StrangerInbox`]).
//!
//! ## Why reuse the contact-invite envelope path
//!
//! The owner-side `acceptCallInvite` MLS bootstrap needs the
//! joiner's `welcomeForOwner` bytes to call
//! [`lakko_mls::Group::join_from_welcome`] and enter the call
//! group. The existing stranger-inbox transport (`ContactRequest`
//! envelope HPKE-sealed to the recipient's identity key, routed
//! via a long-lived pseudonym derived from the recipient's
//! `account_id`) already solves three of the four hard problems
//! the Welcome-ferry has:
//!
//!   1. **Confidentiality** — HPKE-seal to `owner_x25519_pubkey`
//!      bound to the identity already in the KT log. The
//!      server sees only `(pseudonym → opaque envelope)`.
//!   2. **Authenticity** — Ed25519-signed by the joiner's
//!      identity key on the envelope side; the owner can
//!      pin which joiner sent the Welcome.
//!   3. **Anti-abuse** — PoW + Privacy Pass on every
//!      ContactRequest already rate-limits cold senders.
//!
//! Spinning up a separate `/discovery/call-welcome` endpoint
//! would re-implement the same three; reusing means **zero
//! new server surface** for the welcome-ferry sprint. The only
//! piece missing is a way for the owner-side polling loop to
//! tell "this envelope is a Call-Welcome, not an intro
//! message" — that's what this module owns.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload = CALL_WELCOME_ENVELOPE_DOMAIN_V1 || welcome_bytes
//!                      └───── 22 bytes, ASCII ──────┘
//!
//!                       "lakko/call-welcome/v1\x00"
//! ```
//!
//! The 22-byte domain separator goes inside the HPKE-sealed
//! plaintext of the `ContactRequest.message_ciphertext`, NOT
//! into any visible header — so the server learns nothing
//! about whether an envelope carries a Welcome or an intro
//! message.
//!
//! ## Cross-kind isolation
//!
//! Owner-side polling MUST treat a plaintext that doesn't
//! start with `CALL_WELCOME_ENVELOPE_DOMAIN_V1` as a plain
//! intro message (ADR-0005 scenario A — "add me as a
//! contact"). The trailing `\x00` byte in the separator
//! ensures the prefix is **never a valid UTF-8-only
//! intro-message prefix** (intro messages are free-text UTF-8
//! that wouldn't naturally start with a NUL).
//!
//! ## What this module does NOT cover
//!
//! - The HPKE-seal of the outer envelope. That's
//!   [`crate::request::compose`] (joiner side) +
//!   [`crate::request::open_envelope`] (owner side).
//! - The pseudonym routing. That's
//!   [`crate::pseudonym::StrangerInbox::derive`] applied to
//!   the owner's `account_id` (which is the owner-side root
//!   the capability advertises via `owner_account_id`).
//! - The owner-side `Group::join_from_welcome` call after
//!   unwrapping. That's `lakko-mls`.
//! - Subsequent-joiner KP forwarding (multi-party). Same
//!   recipe applies — wrap the joiner's KP under a sibling
//!   domain separator (`lakko/call-keypackage/v1`) and ferry
//!   via the same path — but that's deferred to the
//!   3c3-multi-use sprint.

use core::result::Result as CoreResult;

/// 22-byte domain separator the joiner prepends to its
/// `welcome_for_owner` bytes before HPKE-sealing them into a
/// [`crate::request::ContactRequest`] envelope.
///
/// The trailing `\x00` byte is intentional: it makes the
/// prefix non-UTF-8-text-prefix on purpose so an intro
/// message can never start with it. The owner's polling loop
/// can dispatch on a clean byte-prefix test without parsing.
pub const CALL_WELCOME_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/call-welcome/v1\x00";

/// Minimum plausible Welcome length. The MLS Welcome wire
/// format is bounded below by a 2-byte version + 1-byte
/// cipher-suite + a few framing bytes; in practice a real
/// Welcome from OpenMLS for a 2-member group is ~200+ bytes.
/// We pin the floor at 32 bytes (well below the practical
/// minimum) — anything smaller is almost-certainly NOT a
/// Welcome and surfaces here as `None` rather than a confused
/// `Some(empty_slice)`.
pub const CALL_WELCOME_MIN_PAYLOAD_LEN: usize = 32;

/// Wrap MLS Welcome bytes in the call-welcome envelope.
///
/// The output is the **inner** plaintext the joiner hands to
/// [`crate::request::compose`] as the intro-message argument.
/// The transport layer (HPKE-seal + Ed25519 signature on the
/// envelope) is unchanged from the contact-invite scenario.
///
/// Returns a fresh `Vec<u8>` with the 22-byte prefix +
/// `welcome_bytes`. The caller is responsible for HPKE-sealing
/// the result.
///
/// # Errors
///
/// None — wrap is infallible. Bounds-checks happen at the
/// codec layer (`request::compose`) which will reject an
/// over-sized payload.
#[must_use]
pub fn wrap_call_welcome(welcome_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CALL_WELCOME_ENVELOPE_DOMAIN_V1.len() + welcome_bytes.len());
    out.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
    out.extend_from_slice(welcome_bytes);
    out
}

/// Discriminated unwrap. Returns `Ok(Some(welcome_bytes))`
/// when `plaintext` starts with the call-welcome domain
/// separator and the trailing payload is at least
/// [`CALL_WELCOME_MIN_PAYLOAD_LEN`] bytes long.
///
/// Returns `Ok(None)` when the prefix doesn't match — the
/// owner-side polling loop falls through to the
/// intro-message rendering path.
///
/// Returns `Err(CallWelcomeError::TruncatedPayload)` ONLY
/// when the prefix matches but the trailing payload is
/// shorter than `CALL_WELCOME_MIN_PAYLOAD_LEN`. This is
/// distinct from "not a call-welcome" because a malicious /
/// buggy sender produced a syntactically-marked but
/// semantically-invalid envelope — surfacing it as an error
/// lets the owner-side polling loop log + drop rather than
/// silently fall through to intro-message handling.
///
/// # Errors
///
/// See discussion above — only `TruncatedPayload`.
pub fn try_unwrap_call_welcome(plaintext: &[u8]) -> CoreResult<Option<&[u8]>, CallWelcomeError> {
    if !plaintext.starts_with(CALL_WELCOME_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[CALL_WELCOME_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() < CALL_WELCOME_MIN_PAYLOAD_LEN {
        return Err(CallWelcomeError::TruncatedPayload {
            min: CALL_WELCOME_MIN_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    Ok(Some(payload))
}

/// Errors specific to the call-welcome envelope unwrap path.
///
/// Distinct from [`crate::error::ContactError`] — this layer
/// has only one failure mode and surfacing it through a
/// shared error type would force the FFI surface to bind a
/// much larger enum than necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallWelcomeError {
    /// The plaintext starts with
    /// [`CALL_WELCOME_ENVELOPE_DOMAIN_V1`] but the trailing
    /// payload is shorter than
    /// [`CALL_WELCOME_MIN_PAYLOAD_LEN`]. Treat as a malformed
    /// or hostile envelope — log + drop.
    TruncatedPayload {
        /// Mirror of [`CALL_WELCOME_MIN_PAYLOAD_LEN`] at the
        /// time the error fired — kept on the variant so the
        /// log line is self-contained.
        min: usize,
        /// Observed payload length (after the prefix). Always
        /// `< min`.
        got: usize,
    },
}

impl core::fmt::Display for CallWelcomeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TruncatedPayload { min, got } => {
                write!(f, "call-welcome payload is {got} bytes, minimum is {min}")
            },
        }
    }
}

impl std::error::Error for CallWelcomeError {}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_then_unwrap_round_trips() {
        let welcome = vec![0xABu8; 200];
        let wrapped = wrap_call_welcome(&welcome);

        // Layout pin: `prefix || welcome`.
        assert!(wrapped.starts_with(CALL_WELCOME_ENVELOPE_DOMAIN_V1));
        assert_eq!(
            wrapped.len(),
            CALL_WELCOME_ENVELOPE_DOMAIN_V1.len() + welcome.len(),
        );

        // Round-trip.
        let unwrapped = try_unwrap_call_welcome(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped, welcome.as_slice());
    }

    #[test]
    fn unwrap_returns_none_for_intro_message_plaintext() {
        // Intro messages are free-text UTF-8; the leading byte
        // is whatever the user typed. Owner-side polling MUST
        // surface `None` and fall through to the intro-message
        // rendering path.
        let intro = b"hi bob, it's alice from the meetup";
        let result = try_unwrap_call_welcome(intro).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unwrap_returns_none_for_partial_prefix_match() {
        // A plaintext that happens to share a few bytes with
        // the prefix but diverges before the full 22-byte
        // separator MUST NOT be misidentified as a welcome.
        let almost = b"lakko/contact-request/v1\x00 payload here";
        let result = try_unwrap_call_welcome(almost).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unwrap_rejects_truncated_payload() {
        // Domain separator + a too-small payload — a malformed
        // / hostile envelope. Owner-side log + drop.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 10]); // 10 bytes < 32-byte floor

        let err = try_unwrap_call_welcome(&wrapped).unwrap_err();
        assert_eq!(err, CallWelcomeError::TruncatedPayload { min: 32, got: 10 },);
    }

    #[test]
    fn unwrap_accepts_exactly_min_payload_len() {
        // Floor-boundary check — exactly `MIN_PAYLOAD_LEN`
        // bytes after the prefix is accepted.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0xCDu8; CALL_WELCOME_MIN_PAYLOAD_LEN]);

        let unwrapped = try_unwrap_call_welcome(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped.len(), CALL_WELCOME_MIN_PAYLOAD_LEN);
        assert!(unwrapped.iter().all(|b| *b == 0xCD));
    }

    #[test]
    fn empty_plaintext_returns_none_not_error() {
        // An empty intro message — degenerate but legal — must
        // fall through to the intro-message path, not surface
        // as a truncated welcome.
        let result = try_unwrap_call_welcome(&[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn domain_separator_is_22_bytes_and_ends_with_null() {
        // Format pin. The wire format is frozen.
        assert_eq!(CALL_WELCOME_ENVELOPE_DOMAIN_V1.len(), 22);
        assert_eq!(*CALL_WELCOME_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        // ASCII prefix is `lakko/call-welcome/v1`.
        let printable = &CALL_WELCOME_ENVELOPE_DOMAIN_V1[..21];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/call-welcome/v1",
        );
    }

    #[test]
    fn separator_distinct_from_contact_invite_domains() {
        // Cross-kind isolation: a call-welcome separator MUST
        // NOT collide with any contact-invite domain
        // separator. A future module that adds a sibling
        // envelope (e.g. multi-joiner KP forward) MUST pick a
        // distinct prefix.
        let other_domains: &[&[u8]] = &[
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(*d, CALL_WELCOME_ENVELOPE_DOMAIN_V1);
            // Length-based mismatch is the strongest dispatch
            // rule — surface here if a future bump
            // accidentally aligns the lengths.
            if d.len() == CALL_WELCOME_ENVELOPE_DOMAIN_V1.len() {
                assert_ne!(*d, CALL_WELCOME_ENVELOPE_DOMAIN_V1);
            }
        }
    }

    #[test]
    fn wrap_preserves_arbitrary_welcome_bytes_verbatim() {
        // The wrap step is byte-for-byte append — surface
        // here if a future refactor accidentally re-encodes
        // / pads / truncates the welcome.
        let welcome: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
        let wrapped = wrap_call_welcome(&welcome);
        let unwrapped = try_unwrap_call_welcome(&wrapped).unwrap().unwrap();
        assert_eq!(unwrapped, welcome.as_slice());
    }
}
