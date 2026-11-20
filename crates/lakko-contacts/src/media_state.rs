// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Camera and microphone media-state signal envelope
//! (camera-off-cleanup, extended for the microphone 2026-11-07 —
//! sibling of [`crate::call_welcome`] and [`crate::call_kp_forward`]).
//!
//! ## The problem this solves
//!
//! A remote call participant turning their camera OFF is an
//! RTP-level **mute** (the outgoing `videoTrack.enabled` flips
//! to `false`) — there is no track-removal event and no mute
//! observer on the receiver. So the receiver's tile keeps
//! painting the last decoded (frozen / black) frame because
//! nothing tells it to reset its `camOn` flag. Muting the
//! microphone is the same kind of mute on the audio track, and a
//! receiver equally cannot tell a muted person from a silent one.
//! The fix is an app-level media-state signal the sender
//! broadcasts over the **existing** opaque SFU `signal-relay`
//! payload, sealed under the call's relay key
//! (`lakko_calls::relay`) — the SFU never parses the payload
//! (invariant #2 holds), it just forwards the bytes to the other
//! participants, who classify them and set their tile state.
//!
//! This module owns the shared **wire format** for that signal
//! plus the bounded, fail-closed decoder. Wiring it onto the
//! transport (the SFU signal-relay send / receive) lives in the
//! per-platform clients.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       MEDIA_STATE_ENVELOPE_DOMAIN_V1 || version_byte || flags
//!      └────────── 26 bytes, ASCII ──────────┘ └─ 0x02 ─┘ └─ 1 B ─┘
//!
//!                       "lakko/call-media-state/v1\x00"
//!
//!   flags: bit 0 (0x01) camera on
//!          bit 1 (0x02) microphone on
//!          bits 2..7    must be zero
//! ```
//!
//! Total envelope length is a fixed 28 bytes. The 26-byte domain
//! separator goes inside the sealed relay payload, NOT into any
//! visible header. The trailing `\x00` on the separator makes the
//! prefix impossible for a UTF-8 free-text payload to begin with,
//! exactly as the sibling envelopes do.
//!
//! ## Version 2 only (flag day, 2026-11-07)
//!
//! The version byte `0x01` was the camera-only layout (a state
//! byte of `0x00` / `0x01`). It is refused now: there is no
//! deployment to stay compatible with, so a client built before
//! this change and one built after it do not exchange media state
//! at all (each drops the other's envelope). Every client is
//! rebuilt from the same tree.
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
/// printable prefix is `lakko/call-media-state/v1`. Both payload
/// versions share it: the version byte, not the domain, tells
/// them apart.
pub const MEDIA_STATE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/call-media-state/v1\x00";

/// The only version byte accepted: the state byte is a flag set
/// ([`MEDIA_STATE_FLAG_CAMERA`], [`MEDIA_STATE_FLAG_MICROPHONE`]);
/// every other bit must be zero. `0x01` (the camera-only layout
/// before 2026-11-07) is refused.
pub const MEDIA_STATE_VERSION_V2: u8 = 0x02;

/// Version-2 flag: the sender's camera is on.
pub const MEDIA_STATE_FLAG_CAMERA: u8 = 0x01;

/// Version-2 flag: the sender's microphone is on (not muted).
pub const MEDIA_STATE_FLAG_MICROPHONE: u8 = 0x02;

/// Every bit the state byte may carry.
const MEDIA_STATE_V2_KNOWN_FLAGS: u8 = MEDIA_STATE_FLAG_CAMERA | MEDIA_STATE_FLAG_MICROPHONE;

/// Exact byte length of the payload that follows the domain
/// separator: one version byte + one state byte.
///
/// The media-state payload is a fixed-shape signal, so the
/// decoder validates against this **exact** length — anything
/// longer or shorter is fail-closed rejected.
pub const MEDIA_STATE_PAYLOAD_LEN: usize = 2;

/// Decoded media-state signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaState {
    /// `true` when the sender's camera is on, `false` when the
    /// sender turned it off. The receiver resets its tile's
    /// `camOn` state to this value.
    pub camera_on: bool,
    /// `true` when the sender's microphone is on, `false` when
    /// it is muted. The receiver sets its tile's `micOn` to this.
    pub microphone_on: bool,
}

/// Compose a media-state envelope carrying the camera and the
/// microphone state.
///
/// The output is the **inner** plaintext the sender seals under
/// the call's relay key and hands to the SFU `signal-relay`.
/// Returns a fixed 28-byte `Vec<u8>`:
/// `domain-separator || 0x02 || flags`.
#[must_use]
pub fn compose_media_state(camera_on: bool, microphone_on: bool) -> Vec<u8> {
    let mut flags = 0u8;
    if camera_on {
        flags |= MEDIA_STATE_FLAG_CAMERA;
    }
    if microphone_on {
        flags |= MEDIA_STATE_FLAG_MICROPHONE;
    }
    let mut out =
        Vec::with_capacity(MEDIA_STATE_ENVELOPE_DOMAIN_V1.len() + MEDIA_STATE_PAYLOAD_LEN);
    out.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
    out.push(MEDIA_STATE_VERSION_V2);
    out.push(flags);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`]
/// dispatcher.
///
/// Returns:
///   * `Ok(Some(MediaState))` — `plaintext` started with the
///     media-state domain separator AND the trailing payload
///     decoded cleanly (exact length, version `0x02`, no
///     unknown flag bit).
///   * `Ok(None)` — the prefix doesn't match; the dispatcher
///     falls through to the next classification step.
///   * `Err(MediaStateError)` — the prefix matched but the
///     payload is malformed. A syntactically-marked but
///     semantically-invalid envelope — the caller logs + drops
///     rather than silently mis-rendering it (fail-closed,
///     mirroring the sibling `try_unwrap_*` helpers).
///
/// # Errors
///
/// * [`MediaStateError::BadLength`] — trailing payload is not
///   exactly [`MEDIA_STATE_PAYLOAD_LEN`] bytes.
/// * [`MediaStateError::UnknownVersion`] — the version byte is not
///   [`MEDIA_STATE_VERSION_V2`] (`0x01` included).
/// * [`MediaStateError::BadFlag`] — the state byte has a bit
///   outside [`MEDIA_STATE_FLAG_CAMERA`] |
///   [`MEDIA_STATE_FLAG_MICROPHONE`].
pub fn try_unwrap_media_state(plaintext: &[u8]) -> CoreResult<Option<MediaState>, MediaStateError> {
    let Some(payload) = plaintext.strip_prefix(MEDIA_STATE_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let [version, state] = payload else {
        return Err(MediaStateError::BadLength {
            expected: MEDIA_STATE_PAYLOAD_LEN,
            got: payload.len(),
        });
    };
    if *version != MEDIA_STATE_VERSION_V2 {
        return Err(MediaStateError::UnknownVersion(*version));
    }
    if *state & !MEDIA_STATE_V2_KNOWN_FLAGS != 0 {
        return Err(MediaStateError::BadFlag(*state));
    }
    Ok(Some(MediaState {
        camera_on: *state & MEDIA_STATE_FLAG_CAMERA != 0,
        microphone_on: *state & MEDIA_STATE_FLAG_MICROPHONE != 0,
    }))
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

    /// The version byte is not [`MEDIA_STATE_VERSION_V2`].
    #[error("media-state payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),

    /// The state byte has a bit outside the camera and microphone
    /// flags.
    #[error("media-state state byte {0:#04x} carries an unknown flag")]
    BadFlag(u8),
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn compose_then_decode_round_trips_every_state() {
        for camera_on in [false, true] {
            for microphone_on in [false, true] {
                let wrapped = compose_media_state(camera_on, microphone_on);
                assert!(wrapped.starts_with(MEDIA_STATE_ENVELOPE_DOMAIN_V1));
                assert_eq!(
                    wrapped.len(),
                    MEDIA_STATE_ENVELOPE_DOMAIN_V1.len() + MEDIA_STATE_PAYLOAD_LEN,
                );
                let state = decode_media_state(&wrapped).unwrap();
                assert_eq!(state.camera_on, camera_on);
                assert_eq!(state.microphone_on, microphone_on);
            }
        }
    }

    #[test]
    fn layout_is_domain_version_flags() {
        let sep = MEDIA_STATE_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(compose_media_state(false, false)[sep..], [0x02, 0x00]);
        assert_eq!(compose_media_state(true, false)[sep..], [0x02, 0x01]);
        assert_eq!(compose_media_state(false, true)[sep..], [0x02, 0x02]);
        assert_eq!(compose_media_state(true, true)[sep..], [0x02, 0x03]);
    }

    #[test]
    fn the_camera_only_version_1_layout_is_refused() {
        // Flag day 2026-11-07: the old `0x01 || 0x00/0x01` envelope is not
        // read as anything.
        for camera in [0x00, 0x01] {
            let mut wrapped = MEDIA_STATE_ENVELOPE_DOMAIN_V1.to_vec();
            wrapped.extend_from_slice(&[0x01, camera]);
            assert_eq!(
                decode_media_state(&wrapped).unwrap_err(),
                MediaStateError::UnknownVersion(0x01),
            );
        }
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
    fn decode_rejects_a_bare_separator() {
        assert_eq!(
            decode_media_state(MEDIA_STATE_ENVELOPE_DOMAIN_V1).unwrap_err(),
            MediaStateError::BadLength {
                expected: 2,
                got: 0
            },
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        // Domain separator + a single byte (missing the flags).
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.push(MEDIA_STATE_VERSION_V2);
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
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[MEDIA_STATE_VERSION_V2, 0x01, 0xFF]);
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
        for version in [0x00, 0x01, 0x03, 0xFF] {
            let mut wrapped = Vec::new();
            wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
            wrapped.extend_from_slice(&[version, 0x01]);
            assert_eq!(
                decode_media_state(&wrapped).unwrap_err(),
                MediaStateError::UnknownVersion(version),
            );
        }
    }

    #[test]
    fn decode_rejects_unknown_flag_bits() {
        for flag in [0x04, 0x07, 0x80, 0xFF] {
            let mut wrapped = Vec::new();
            wrapped.extend_from_slice(MEDIA_STATE_ENVELOPE_DOMAIN_V1);
            wrapped.extend_from_slice(&[MEDIA_STATE_VERSION_V2, flag]);
            assert_eq!(
                decode_media_state(&wrapped).unwrap_err(),
                MediaStateError::BadFlag(flag),
            );
        }
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

        let media = compose_media_state(true, false);
        assert!(try_unwrap_call_welcome(&media).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&media).unwrap().is_none());

        let welcome = wrap_call_welcome(&vec![0xABu8; 256]);
        assert!(try_unwrap_media_state(&welcome).unwrap().is_none());
        let kp = wrap_call_kp_forward(&vec![0xCDu8; 512]);
        assert!(try_unwrap_media_state(&kp).unwrap().is_none());
    }

    proptest! {
        /// Any input: never a panic, and anything accepted re-composes to
        /// the identical bytes (the encoding is canonical).
        #[test]
        fn decode_is_total_and_canonical(
            prefixed in any::<bool>(),
            tail in proptest::collection::vec(any::<u8>(), 0..8),
        ) {
            // Half the cases start with the separator, so the version, length
            // and flag checks see arbitrary bytes, not only the domain check.
            let mut data = if prefixed { MEDIA_STATE_ENVELOPE_DOMAIN_V1.to_vec() } else { Vec::new() };
            data.extend_from_slice(&tail);
            if let Ok(state) = decode_media_state(&data) {
                prop_assert_eq!(compose_media_state(state.camera_on, state.microphone_on), data);
            }
        }

        /// Every (version, state) byte pair under the separator: exactly
        /// version 0x02 with one of the four flag combinations decodes.
        #[test]
        fn exactly_the_defined_bytes_decode(version in any::<u8>(), state in any::<u8>()) {
            let mut wrapped = MEDIA_STATE_ENVELOPE_DOMAIN_V1.to_vec();
            wrapped.extend_from_slice(&[version, state]);
            let accepted = decode_media_state(&wrapped).is_ok();
            prop_assert_eq!(accepted, version == MEDIA_STATE_VERSION_V2 && state <= 3);
        }

        /// Composing then decoding gives back what was composed.
        #[test]
        fn compose_decode_round_trip(camera_on in any::<bool>(), microphone_on in any::<bool>()) {
            let decoded = decode_media_state(&compose_media_state(camera_on, microphone_on)).unwrap();
            prop_assert_eq!(decoded, MediaState { camera_on, microphone_on });
        }
    }
}
