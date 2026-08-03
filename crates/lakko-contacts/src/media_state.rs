// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Camera on/off media-state signal envelope
//! (camera-off-cleanup — sibling of [`crate::call_welcome`]
//! and [`crate::call_kp_forward`]).
//!
//! ## The problem this solves
//!
//! A remote call participant turning their camera OFF is an
//! RTP-level **mute** (the outgoing `videoTrack.enabled` flips
//! to `false`) — there is no track-removal event and no mute
//! observer on the receiver. So the receiver's tile keeps
//! painting the last decoded (frozen / black) frame because
//! nothing tells it to reset its `camOn` flag. The fix is an
//! app-level "camera on/off" signal the sender broadcasts over
//! the **existing** opaque SFU `signal-relay` payload — the
//! SFU never parses the payload (invariant #2 holds), it just
//! forwards the bytes to the other participants, who classify
//! them and reset their tile state.
//!
//! This module owns the shared **wire format** for that signal
//! plus the bounded, fail-closed decoder. Wiring it onto the
//! transport (the SFU signal-relay send / receive) is Phase 2
//! and lives in the per-platform clients.
//!
//! ## Why reuse the relay-payload envelope pattern
//!
//! The signal rides the same opaque relay channel that already
//! carries the call-orchestration envelopes
//! ([`crate::call_welcome`], [`crate::call_kp_forward`]). Those
//! established the pattern: a domain-separated, fail-closed
//! envelope classified by a leading byte prefix, dispatched in
//! [`crate::incoming_envelope`]. The media-state signal is a
//! peer of those — the only structural difference is that its
//! payload is a **fixed, fully decoded flag** (one bool) rather
//! than an opaque variable-length blob, so it validates against
//! an **exact** length rather than a minimum floor and carries
//! an explicit in-payload schema-version byte for forward
//! compatibility.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       MEDIA_STATE_ENVELOPE_DOMAIN_V1 || version_byte || flag_byte
//!      └────────── 26 bytes, ASCII ──────────┘ └─ 0x01 ─┘ └ 0x00/0x01 ┘
//!
//!                       "lakko/call-media-state/v1\x00"
//! ```
//!
//! Total envelope length is a fixed 28 bytes. The 26-byte
//! domain separator goes inside the opaque relay payload, NOT
//! into any visible header — the server learns nothing about
//! what the payload carries. The trailing `\x00` on the
//! separator makes the prefix impossible for a UTF-8 free-text
//! payload to begin with, exactly as the sibling envelopes do.
//!
//! ## Cross-kind isolation
//!
//! The separator MUST be byte-distinct from every other
//! envelope-domain prefix in this crate — the
//! [`tests::separator_distinct_from_sibling_domains`] test
//! asserts this. Dispatch is a byte-prefix match, so a distinct
//! prefix guarantees a media-state payload never cross-dispatches
//! as a call-welcome / KP-forward and vice versa.

use core::result::Result as CoreResult;

use thiserror::Error;

/// 26-byte domain separator the sender prepends to the
/// media-state payload before handing it to the relay
/// transport.
///
/// The trailing `\x00` byte makes the prefix impossible for a
/// UTF-8 free-text payload to start with, mirroring
/// [`crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1`]. The
/// printable prefix is `lakko/call-media-state/v1`.
pub const MEDIA_STATE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/call-media-state/v1\x00";

/// Schema-version byte carried as the first byte of the payload
/// (after the domain separator).
///
/// This is an in-payload forward-compatibility knob distinct
/// from the `/v1` in the domain string: a future revision that
/// keeps the same domain but adds a field bumps this byte so an
/// old decoder fail-closes rather than mis-reading the new
/// layout. Today the only accepted value is `0x01`.
pub const MEDIA_STATE_VERSION_V1: u8 = 0x01;

/// Exact byte length of the payload that follows the domain
/// separator: one version byte + one flag byte.
///
/// Unlike the sibling envelopes (which carry opaque
/// variable-length blobs bounded below by a minimum floor), the
/// media-state payload is a fixed-shape signal, so the decoder
/// validates against this **exact** length — anything longer or
/// shorter is fail-closed rejected.
pub const MEDIA_STATE_PAYLOAD_LEN: usize = 2;

/// Decoded camera on/off signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaState {
    /// `true` when the sender's camera is on, `false` when the
    /// sender turned it off. The receiver resets its tile's
    /// `camOn` state to this value.
    pub camera_on: bool,
}

/// Compose a media-state envelope for the given camera flag.
///
/// The output is the **inner** plaintext the sender hands to
/// the relay transport (the SFU `signal-relay` payload in
/// Phase 2). Returns a fixed 28-byte `Vec<u8>`:
/// `domain-separator || version-byte || flag-byte`.
///
/// # Errors
///
/// None — compose is infallible.
#[must_use]
pub fn compose_media_state(camera_on: bool) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(MEDIA_STATE_ENVELOPE_DOMAIN_V1.len() + MEDIA_STATE_PAYLOAD_LEN);
    out.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
    out.push(MEDIA_STATE_VERSION_V1);
    out.push(u8::from(camera_on));
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`]
/// dispatcher.
///
/// Returns:
///   * `Ok(Some(MediaState))` — `plaintext` started with the
///     media-state domain separator AND the trailing payload
///     decoded cleanly (exact length, known version, valid
///     flag byte).
///   * `Ok(None)` — the prefix doesn't match; the dispatcher
///     falls through to the next classification step.
///   * `Err(MediaStateError)` — the prefix matched but the
///     payload is malformed (wrong length / unknown version /
///     out-of-range flag byte). A syntactically-marked but
///     semantically-invalid envelope — the caller logs + drops
///     rather than silently mis-rendering it (fail-closed,
///     mirroring the sibling `try_unwrap_*` helpers).
///
/// # Errors
///
/// * [`MediaStateError::BadLength`] — trailing payload is not
///   exactly [`MEDIA_STATE_PAYLOAD_LEN`] bytes.
/// * [`MediaStateError::UnknownVersion`] — the version byte is
///   not [`MEDIA_STATE_VERSION_V1`].
/// * [`MediaStateError::BadFlag`] — the flag byte is neither
///   `0x00` nor `0x01`.
pub fn try_unwrap_media_state(plaintext: &[u8]) -> CoreResult<Option<MediaState>, MediaStateError> {
    if !plaintext.starts_with(MEDIA_STATE_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[MEDIA_STATE_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() != MEDIA_STATE_PAYLOAD_LEN {
        return Err(MediaStateError::BadLength {
            expected: MEDIA_STATE_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    if payload[0] != MEDIA_STATE_VERSION_V1 {
        return Err(MediaStateError::UnknownVersion(payload[0]));
    }
    let camera_on = match payload[1] {
        0 => false,
        1 => true,
        other => return Err(MediaStateError::BadFlag(other)),
    };
    Ok(Some(MediaState { camera_on }))
}

/// Strict standalone decoder: parse a media-state envelope,
/// treating a non-media-state plaintext as a hard error.
///
/// This is the fail-closed entry point a caller uses when it
/// already knows the payload is *supposed* to be a media-state
/// signal (e.g. a relay-payload it pre-classified). A plaintext
/// that does not carry the media-state domain separator is
/// rejected with [`MediaStateError::WrongDomain`] rather than
/// silently falling through — the dispatcher-facing
/// [`try_unwrap_media_state`] is the helper that returns
/// `Ok(None)` for the fall-through case.
///
/// # Errors
///
/// * [`MediaStateError::WrongDomain`] — `bytes` does not start
///   with [`MEDIA_STATE_ENVELOPE_DOMAIN_V1`].
/// * [`MediaStateError::BadLength`] / [`MediaStateError::UnknownVersion`]
///   / [`MediaStateError::BadFlag`] — see [`try_unwrap_media_state`].
pub fn decode_media_state(bytes: &[u8]) -> CoreResult<MediaState, MediaStateError> {
    match try_unwrap_media_state(bytes)? {
        Some(state) => Ok(state),
        None => Err(MediaStateError::WrongDomain),
    }
}

/// Errors specific to the media-state envelope decode path.
///
/// Dedicated (rather than reusing [`crate::call_welcome::CallWelcomeError`])
/// because the fixed-shape payload has richer failure modes
/// than the siblings' single truncated-payload case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MediaStateError {
    /// The plaintext does not start with the media-state domain
    /// separator. Only surfaced by [`decode_media_state`]; the
    /// dispatcher-facing [`try_unwrap_media_state`] returns
    /// `Ok(None)` for this case instead so the dispatch chain
    /// can continue.
    #[error("plaintext does not carry the media-state domain separator")]
    WrongDomain,

    /// The domain separator matched but the trailing payload is
    /// not exactly [`MEDIA_STATE_PAYLOAD_LEN`] bytes.
    #[error("media-state payload is {got} bytes, expected exactly {expected}")]
    BadLength {
        /// Mirror of [`MEDIA_STATE_PAYLOAD_LEN`] at the time the
        /// error fired — kept on the variant so the log line is
        /// self-contained.
        expected: usize,
        /// Observed payload length (after the separator). Always
        /// `!= expected`.
        got: usize,
    },

    /// The version byte is not [`MEDIA_STATE_VERSION_V1`].
    #[error("media-state payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),

    /// The flag byte is neither `0x00` (camera off) nor `0x01`
    /// (camera on).
    #[error("media-state flag byte {0:#04x} is neither 0x00 nor 0x01")]
    BadFlag(u8),
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_then_decode_round_trips_camera_on() {
        let wrapped = compose_media_state(true);
        assert!(wrapped.starts_with(MEDIA_STATE_ENVELOPE_DOMAIN_V1));
        assert_eq!(
            wrapped.len(),
            MEDIA_STATE_ENVELOPE_DOMAIN_V1.len() + MEDIA_STATE_PAYLOAD_LEN,
        );
        let state = decode_media_state(&wrapped).unwrap();
        assert!(state.camera_on);
    }

    #[test]
    fn compose_then_decode_round_trips_camera_off() {
        let wrapped = compose_media_state(false);
        let state = decode_media_state(&wrapped).unwrap();
        assert!(!state.camera_on);
    }

    #[test]
    fn compose_layout_is_domain_version_flag() {
        // Pin the exact wire layout — the format is frozen.
        let on = compose_media_state(true);
        let off = compose_media_state(false);
        let sep = MEDIA_STATE_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(&on[..sep], MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        assert_eq!(on[sep], MEDIA_STATE_VERSION_V1);
        assert_eq!(on[sep + 1], 0x01);
        assert_eq!(off[sep + 1], 0x00);
    }

    #[test]
    fn try_unwrap_returns_none_for_non_media_state_plaintext() {
        // A plain intro message / unrelated relay payload must
        // fall through, not be misread as a media-state signal.
        let intro = b"hi alice it's bob";
        assert!(try_unwrap_media_state(intro).unwrap().is_none());
    }

    #[test]
    fn decode_rejects_non_media_state_plaintext() {
        let intro = b"hi alice it's bob";
        assert_eq!(
            decode_media_state(intro).unwrap_err(),
            MediaStateError::WrongDomain,
        );
    }

    #[test]
    fn decode_returns_none_via_try_unwrap_for_empty() {
        // Degenerate empty plaintext — falls through, not a
        // truncated media-state envelope.
        assert!(try_unwrap_media_state(&[]).unwrap().is_none());
        assert_eq!(
            decode_media_state(&[]).unwrap_err(),
            MediaStateError::WrongDomain,
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        // Domain separator + a single byte (missing the flag).
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.push(MEDIA_STATE_VERSION_V1);
        assert_eq!(
            decode_media_state(&wrapped).unwrap_err(),
            MediaStateError::BadLength {
                expected: 2,
                got: 1
            },
        );
    }

    #[test]
    fn decode_rejects_overlong_payload() {
        // Domain separator + three bytes — exact-length check
        // rejects a payload that is longer than expected.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[MEDIA_STATE_VERSION_V1, 0x01, 0xFF]);
        assert_eq!(
            decode_media_state(&wrapped).unwrap_err(),
            MediaStateError::BadLength {
                expected: 2,
                got: 3
            },
        );
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0x02, 0x01]); // version 2 is unknown
        assert_eq!(
            decode_media_state(&wrapped).unwrap_err(),
            MediaStateError::UnknownVersion(0x02),
        );
    }

    #[test]
    fn decode_rejects_out_of_range_flag_byte() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[MEDIA_STATE_VERSION_V1, 0x02]); // flag must be 0/1
        assert_eq!(
            decode_media_state(&wrapped).unwrap_err(),
            MediaStateError::BadFlag(0x02),
        );
    }

    #[test]
    fn domain_separator_is_26_bytes_and_ends_with_null() {
        // Format pin. The wire format is frozen.
        assert_eq!(MEDIA_STATE_ENVELOPE_DOMAIN_V1.len(), 26);
        assert_eq!(*MEDIA_STATE_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        let printable = &MEDIA_STATE_ENVELOPE_DOMAIN_V1[..25];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/call-media-state/v1",
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        // Cross-kind isolation across every envelope-domain
        // constant in the crate — a future collision surfaces
        // here.
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(
                *d, MEDIA_STATE_ENVELOPE_DOMAIN_V1,
                "media-state separator must be byte-distinct from {d:?}",
            );
        }
    }

    #[test]
    fn media_state_does_not_cross_dispatch_as_sibling() {
        // A media-state envelope MUST NOT decode as a call
        // welcome / KP-forward, and vice versa.
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};

        let media = compose_media_state(true);
        assert!(try_unwrap_call_welcome(&media).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&media).unwrap().is_none());

        let welcome = wrap_call_welcome(&vec![0xABu8; 256]);
        assert!(try_unwrap_media_state(&welcome).unwrap().is_none());
        let kp = wrap_call_kp_forward(&vec![0xCDu8; 512]);
        assert!(try_unwrap_media_state(&kp).unwrap().is_none());
    }
}
