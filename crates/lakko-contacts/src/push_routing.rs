// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Opaque per-group push routing id + the in-group
//! secret-distribution envelope (ADR-0042 — close the push
//! social-graph leak, invariant #5).
//!
//! ## The problem this solves
//!
//! The push subsystem routes group wakes by a 32-byte group
//! identifier that is **identical to the Delivery Service (DS)
//! journal key** (`services/ds`'s `/ds/group/{group_id_hex}`
//! path). That one value materialises "these devices are one
//! group" both inside `services/push-gw` (the subscription row
//! and the notify JOIN) and — worse — **in cleartext at Apple /
//! Google** (the delivered payload's `g` field + the APNs
//! `thread-id`). That directly violates invariant #5 (the
//! social graph is not materialised on the server).
//!
//! The fix routes push under a stable **opaque `push_gid`** that
//! is *not* the DS journal key. `push_gid` is a one-way,
//! domain-separated derivation of a per-group
//! **`push_routing_secret`** — 32 bytes of E2EE key material
//! minted once at group creation and distributed in-band over an
//! MLS application message (this module owns that envelope + the
//! derivation). push-gw keeps handling the 32-byte value as the
//! same opaque `BINARY(32)` lookup key it always did, so its
//! code + schema are untouched (invariant #2); only the value's
//! *meaning* shifts from "DS journal key" to "opaque routing id".
//!
//! ## Why reuse the relay-payload envelope pattern
//!
//! The secret rides an MLS application message on the group
//! journal (E2EE — the DS sees ciphertext, invariant #2). It is
//! classified by a leading-byte domain prefix in the receive-loop
//! envelope dispatch — the exact
//! [`crate::media_state`] pattern — so it is **never** rendered
//! as a chat bubble and a malformed envelope is fail-closed
//! dropped rather than mis-parsed. The only structural difference
//! from `media_state` is the payload: a fixed **32-byte secret**
//! rather than a one-byte flag, so the codec validates against an
//! **exact** length that includes the secret and there is no flag
//! byte to range-check (any 32 bytes is a valid secret).
//!
//! ## Wire shape (distribution envelope)
//!
//! ```text
//!   plaintext_payload =
//!       PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1 || version_byte || secret(32)
//!      └────────── 29 bytes, ASCII ───────────┘ └── 0x01 ───┘ └── 32 bytes ──┘
//!
//!                   "lakko/push-routing-secret/v1\x00"
//! ```
//!
//! Total envelope length is a fixed 62 bytes. The 29-byte domain
//! separator goes inside the E2EE MLS application-message
//! plaintext, NOT into any visible header — the server learns
//! nothing about what the payload carries. The trailing `\x00` on
//! the separator makes the prefix impossible for a UTF-8
//! free-text payload to begin with, exactly as the sibling
//! envelopes do.
//!
//! ## `push_gid` derivation
//!
//! ```text
//!   push_gid = keyed-BLAKE3(push_routing_secret,
//!                           "lakko/push/group-routing-id/v1")
//! ```
//!
//! Keyed BLAKE3 with the 32-byte secret as the key and the domain
//! string as the message. Deterministic, one-way, domain-
//! separated, and **stable** (no epoch input), so the value is a
//! fixed per-group routing id: a subscriber survives membership
//! commits without re-subscribing and the iOS `thread-id` stays
//! constant. `push_gid` is unrelated to any `group_id`, so a
//! push-gw-vs-DS cross-correlation can no longer link a
//! subscription to a group without the group secret.
//!
//! ## Cross-kind isolation
//!
//! The separator MUST be byte-distinct from every other
//! envelope-domain prefix in this crate — the
//! [`tests::separator_distinct_from_sibling_domains`] test
//! asserts this. Dispatch is a byte-prefix match, so a distinct
//! prefix guarantees a push-routing-secret payload never
//! cross-dispatches as a call-welcome / KP-forward / media-state
//! and vice versa.

use core::result::Result as CoreResult;

use lakko_crypto::hash::blake3_keyed;
use thiserror::Error;

/// Length of a `push_routing_secret` in bytes.
///
/// 32 bytes of E2EE key material minted once per group at
/// creation. Also the length of every derived [`derive_push_gid`]
/// output, so the value drops straight into push-gw's existing
/// `BINARY(32)` / hex key-space.
pub const PUSH_ROUTING_SECRET_LEN: usize = 32;

/// Length of a derived `push_gid` in bytes (a BLAKE3 digest).
pub const PUSH_GID_LEN: usize = 32;

/// Domain string keyed into the [`derive_push_gid`] BLAKE3
/// keyed-hash as the *message* (the secret is the key).
///
/// Unlike [`PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`] this is a
/// hashed message, not a dispatch prefix, so it carries **no**
/// trailing `\x00`. Changing this string changes every group's
/// `push_gid` — the golden test [`tests::derive_push_gid_golden`]
/// pins it so an accidental edit fails CI.
pub const PUSH_GID_DERIVE_DOMAIN_V1: &[u8] = b"lakko/push/group-routing-id/v1";

/// 29-byte domain separator the sender prepends to the
/// distribution-envelope payload before it rides the group
/// journal as an MLS application message.
///
/// The trailing `\x00` byte makes the prefix impossible for a
/// UTF-8 free-text payload to start with, mirroring
/// [`crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1`]. The
/// printable prefix is `lakko/push-routing-secret/v1`.
pub const PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/push-routing-secret/v1\x00";

/// Schema-version byte carried as the first byte of the payload
/// (after the domain separator).
///
/// An in-payload forward-compatibility knob distinct from the
/// `/v1` in the domain string: a future revision that keeps the
/// same domain but changes the layout bumps this byte so an old
/// decoder fail-closes rather than mis-reading the new shape.
/// Today the only accepted value is `0x01`.
pub const PUSH_ROUTING_SECRET_VERSION_V1: u8 = 0x01;

/// Exact byte length of the payload that follows the domain
/// separator: one version byte + the 32-byte secret.
///
/// The decoder validates against this **exact** length —
/// anything longer or shorter is fail-closed rejected.
pub const PUSH_ROUTING_SECRET_PAYLOAD_LEN: usize = 1 + PUSH_ROUTING_SECRET_LEN;

/// Derive the stable opaque `push_gid` from a group's
/// `push_routing_secret`.
///
/// `push_gid = keyed-BLAKE3(push_routing_secret,
/// "lakko/push/group-routing-id/v1")` — the 32-byte secret is the
/// BLAKE3 key, [`PUSH_GID_DERIVE_DOMAIN_V1`] is the message.
/// Deterministic + one-way + domain-separated + stable (no epoch
/// input). The output is 32 bytes, so it drops straight into
/// push-gw's `BINARY(32)` / hex key-space, and it is unrelated to
/// the group's `group_id` (invariant #5 — no cross-correlation
/// without the secret).
#[must_use]
pub fn derive_push_gid(push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN]) -> [u8; PUSH_GID_LEN] {
    blake3_keyed(push_routing_secret, PUSH_GID_DERIVE_DOMAIN_V1)
}

/// Compose a push-routing-secret distribution envelope for the
/// given 32-byte secret.
///
/// The output is the **inner** plaintext the sender hands to the
/// MLS group journal as an application message (E2EE — the DS
/// sees ciphertext). Returns a fixed 62-byte `Vec<u8>`:
/// `domain-separator || version-byte || secret(32)`.
#[must_use]
pub fn compose_push_routing_secret(secret: &[u8; PUSH_ROUTING_SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len() + PUSH_ROUTING_SECRET_PAYLOAD_LEN,
    );
    out.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
    out.push(PUSH_ROUTING_SECRET_VERSION_V1);
    out.extend_from_slice(secret);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`]
/// dispatcher.
///
/// Returns:
///   * `Ok(Some(secret))` — `plaintext` started with the
///     push-routing-secret domain separator AND the trailing
///     payload decoded cleanly (exact length + known version).
///   * `Ok(None)` — the prefix doesn't match; the dispatcher
///     falls through to the next classification step.
///   * `Err(PushRoutingSecretError)` — the prefix matched but the
///     payload is malformed (wrong length / unknown version). A
///     syntactically-marked but semantically-invalid envelope —
///     the caller logs + drops rather than silently mis-handling
///     it (fail-closed, mirroring the sibling `try_unwrap_*`
///     helpers).
///
/// # Errors
///
/// * [`PushRoutingSecretError::BadLength`] — trailing payload is
///   not exactly [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] bytes.
/// * [`PushRoutingSecretError::UnknownVersion`] — the version
///   byte is not [`PUSH_ROUTING_SECRET_VERSION_V1`].
pub fn try_unwrap_push_routing_secret(
    plaintext: &[u8],
) -> CoreResult<Option<[u8; PUSH_ROUTING_SECRET_LEN]>, PushRoutingSecretError> {
    if !plaintext.starts_with(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() != PUSH_ROUTING_SECRET_PAYLOAD_LEN {
        return Err(PushRoutingSecretError::BadLength {
            expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    if payload[0] != PUSH_ROUTING_SECRET_VERSION_V1 {
        return Err(PushRoutingSecretError::UnknownVersion(payload[0]));
    }
    let mut secret = [0u8; PUSH_ROUTING_SECRET_LEN];
    // Exact-length gate above guarantees `payload[1..]` is exactly
    // `PUSH_ROUTING_SECRET_LEN` bytes, so this copy cannot panic.
    secret.copy_from_slice(&payload[1..]);
    Ok(Some(secret))
}

/// Strict standalone decoder: parse a push-routing-secret
/// envelope, treating a non-matching plaintext as a hard error.
///
/// This is the fail-closed entry point a caller uses when it
/// already knows the payload is *supposed* to be a
/// push-routing-secret envelope (e.g. a group-journal application
/// message it pre-classified). A plaintext that does not carry
/// the domain separator is rejected with
/// [`PushRoutingSecretError::WrongDomain`] rather than silently
/// falling through — the dispatcher-facing
/// [`try_unwrap_push_routing_secret`] is the helper that returns
/// `Ok(None)` for the fall-through case.
///
/// # Errors
///
/// * [`PushRoutingSecretError::WrongDomain`] — `bytes` does not
///   start with [`PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1`].
/// * [`PushRoutingSecretError::BadLength`] /
///   [`PushRoutingSecretError::UnknownVersion`] — see
///   [`try_unwrap_push_routing_secret`].
pub fn decode_push_routing_secret(
    bytes: &[u8],
) -> CoreResult<[u8; PUSH_ROUTING_SECRET_LEN], PushRoutingSecretError> {
    match try_unwrap_push_routing_secret(bytes)? {
        Some(secret) => Ok(secret),
        None => Err(PushRoutingSecretError::WrongDomain),
    }
}

/// Errors specific to the push-routing-secret envelope decode
/// path.
///
/// Dedicated (rather than reusing a sibling's error) because the
/// fixed-shape 32-byte-secret payload has a distinct failure set
/// (no flag byte to range-check, unlike
/// [`crate::media_state::MediaStateError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PushRoutingSecretError {
    /// The plaintext does not start with the push-routing-secret
    /// domain separator. Only surfaced by
    /// [`decode_push_routing_secret`]; the dispatcher-facing
    /// [`try_unwrap_push_routing_secret`] returns `Ok(None)` for
    /// this case instead so the dispatch chain can continue.
    #[error("plaintext does not carry the push-routing-secret domain separator")]
    WrongDomain,

    /// The domain separator matched but the trailing payload is
    /// not exactly [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] bytes.
    #[error("push-routing-secret payload is {got} bytes, expected exactly {expected}")]
    BadLength {
        /// Mirror of [`PUSH_ROUTING_SECRET_PAYLOAD_LEN`] at the
        /// time the error fired — kept on the variant so the log
        /// line is self-contained.
        expected: usize,
        /// Observed payload length (after the separator). Always
        /// `!= expected`.
        got: usize,
    },

    /// The version byte is not
    /// [`PUSH_ROUTING_SECRET_VERSION_V1`].
    #[error("push-routing-secret payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SECRET: [u8; PUSH_ROUTING_SECRET_LEN] = [0x42u8; PUSH_ROUTING_SECRET_LEN];

    // -------------------------------------------------------------------------
    // derive_push_gid
    // -------------------------------------------------------------------------

    #[test]
    fn derive_push_gid_is_deterministic() {
        let a = derive_push_gid(&SAMPLE_SECRET);
        let b = derive_push_gid(&SAMPLE_SECRET);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_push_gid_golden() {
        // Golden vector pins the exact derivation for seed 0x42*32.
        // A change to `PUSH_GID_DERIVE_DOMAIN_V1`, the keyed-hash
        // primitive, or the key/message argument order surfaces
        // here as a mismatch — so every platform (Rust core + FFI)
        // is proven to derive the same push_gid. Pinned as a byte
        // array to keep the crate dependency-free (no `hex`).
        const GOLDEN_PUSH_GID: [u8; PUSH_GID_LEN] = [
            0x61, 0xf2, 0xd2, 0x24, 0xca, 0x4d, 0x00, 0xe4, 0x32, 0x7d, 0x98, 0x39, 0x02, 0x74,
            0xa4, 0x90, 0xdc, 0x2d, 0x0f, 0x03, 0x21, 0x98, 0xd0, 0x3c, 0xb7, 0x27, 0x44, 0xeb,
            0xa4, 0x01, 0x2a, 0x8a,
        ];
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_eq!(gid, GOLDEN_PUSH_GID);
    }

    #[test]
    fn derive_push_gid_differs_from_secret() {
        // The routing id is one-way: it is not the secret echoed.
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_ne!(gid, SAMPLE_SECRET);
    }

    #[test]
    fn derive_push_gid_differs_from_group_id() {
        // The whole point of ADR-0042: push_gid must not equal the
        // DS journal key (group_id). A group_id is unrelated bytes
        // (an opaque MLS GroupId), so the derived gid must differ.
        let group_id = [0x42u8; 32]; // pretend the group_id == the secret bytes
        let gid = derive_push_gid(&SAMPLE_SECRET);
        assert_ne!(gid, group_id, "push_gid must never equal the group_id");
    }

    #[test]
    fn derive_push_gid_secret_separation() {
        // Two distinct secrets yield distinct routing ids.
        let a = derive_push_gid(&[0x01u8; 32]);
        let b = derive_push_gid(&[0x02u8; 32]);
        assert_ne!(a, b);
    }

    // -------------------------------------------------------------------------
    // envelope round-trip
    // -------------------------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        let wrapped = compose_push_routing_secret(&SAMPLE_SECRET);
        assert!(wrapped.starts_with(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1));
        assert_eq!(
            wrapped.len(),
            PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len() + PUSH_ROUTING_SECRET_PAYLOAD_LEN,
        );
        let decoded = decode_push_routing_secret(&wrapped).unwrap();
        assert_eq!(decoded, SAMPLE_SECRET);
    }

    #[test]
    fn compose_then_try_unwrap_round_trips() {
        let wrapped = compose_push_routing_secret(&SAMPLE_SECRET);
        let decoded = try_unwrap_push_routing_secret(&wrapped).unwrap();
        assert_eq!(decoded, Some(SAMPLE_SECRET));
    }

    #[test]
    fn compose_layout_is_domain_version_secret() {
        // Pin the exact wire layout — the format is frozen.
        let wrapped = compose_push_routing_secret(&SAMPLE_SECRET);
        let sep = PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(&wrapped[..sep], PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        assert_eq!(wrapped[sep], PUSH_ROUTING_SECRET_VERSION_V1);
        assert_eq!(&wrapped[sep + 1..], &SAMPLE_SECRET);
        assert_eq!(wrapped.len(), 62);
    }

    // -------------------------------------------------------------------------
    // fail-closed decode paths
    // -------------------------------------------------------------------------

    #[test]
    fn try_unwrap_returns_none_for_non_matching_plaintext() {
        let intro = b"hi alice it's bob";
        assert!(try_unwrap_push_routing_secret(intro).unwrap().is_none());
    }

    #[test]
    fn decode_rejects_non_matching_plaintext() {
        let intro = b"hi alice it's bob";
        assert_eq!(
            decode_push_routing_secret(intro).unwrap_err(),
            PushRoutingSecretError::WrongDomain,
        );
    }

    #[test]
    fn decode_empty_falls_through_via_try_unwrap() {
        assert!(try_unwrap_push_routing_secret(&[]).unwrap().is_none());
        assert_eq!(
            decode_push_routing_secret(&[]).unwrap_err(),
            PushRoutingSecretError::WrongDomain,
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        // Domain separator + version + only 4 secret bytes.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(PUSH_ROUTING_SECRET_VERSION_V1);
        wrapped.extend_from_slice(&[0u8; 4]);
        assert_eq!(
            decode_push_routing_secret(&wrapped).unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 5,
            },
        );
    }

    #[test]
    fn decode_rejects_missing_secret() {
        // Domain separator + version byte only (no secret).
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(PUSH_ROUTING_SECRET_VERSION_V1);
        assert_eq!(
            decode_push_routing_secret(&wrapped).unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: 1,
            },
        );
    }

    #[test]
    fn decode_rejects_overlong_payload() {
        // Domain separator + version + 33 secret bytes (one too many).
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(PUSH_ROUTING_SECRET_VERSION_V1);
        wrapped.extend_from_slice(&[0xABu8; PUSH_ROUTING_SECRET_LEN + 1]);
        assert_eq!(
            decode_push_routing_secret(&wrapped).unwrap_err(),
            PushRoutingSecretError::BadLength {
                expected: PUSH_ROUTING_SECRET_PAYLOAD_LEN,
                got: PUSH_ROUTING_SECRET_LEN + 2,
            },
        );
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(0x02); // version 2 is unknown
        wrapped.extend_from_slice(&SAMPLE_SECRET);
        assert_eq!(
            decode_push_routing_secret(&wrapped).unwrap_err(),
            PushRoutingSecretError::UnknownVersion(0x02),
        );
    }

    // -------------------------------------------------------------------------
    // format pins + cross-kind isolation
    // -------------------------------------------------------------------------

    #[test]
    fn domain_separator_is_29_bytes_and_ends_with_null() {
        assert_eq!(PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.len(), 29);
        assert_eq!(*PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1.last().unwrap(), 0u8);
        let printable = &PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1[..28];
        assert_eq!(
            core::str::from_utf8(printable).unwrap(),
            "lakko/push-routing-secret/v1",
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        // Cross-kind isolation across every envelope-domain
        // constant in the crate — a future collision surfaces here.
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            b"lakko-invite-revoke/v1\x00",
            b"lakko-call-invite-revoke/v1\x00",
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
        ];
        for d in other_domains {
            assert_ne!(
                *d, PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
                "push-routing-secret separator must be byte-distinct from {d:?}",
            );
        }
    }

    #[test]
    fn does_not_cross_dispatch_as_sibling() {
        // A push-routing-secret envelope MUST NOT decode as a call
        // welcome / KP-forward / media-state, and vice versa.
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};

        let envelope = compose_push_routing_secret(&SAMPLE_SECRET);
        assert!(try_unwrap_call_welcome(&envelope).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&envelope).unwrap().is_none());
        assert!(try_unwrap_media_state(&envelope).unwrap().is_none());

        let welcome = wrap_call_welcome(&vec![0xABu8; 256]);
        assert!(try_unwrap_push_routing_secret(&welcome).unwrap().is_none());
        let kp = wrap_call_kp_forward(&vec![0xCDu8; 512]);
        assert!(try_unwrap_push_routing_secret(&kp).unwrap().is_none());
        let media = compose_media_state(true);
        assert!(try_unwrap_push_routing_secret(&media).unwrap().is_none());
    }
}
