// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` committer-beacon surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0045 decision-2 beacon).
//!
//! ## Why the NSE needs this at all
//!
//! Exactly the reason [`crate::push_routing`] exists, reached a second time by
//! a second envelope kind. A committer beacon is an ordinary MLS application
//! message on the group journal, and
//! [`crate::attachments::parse_body_envelope`] has a deliberate legacy fallback
//! that degrades non-JSON bytes to `Chat` via `from_utf8_lossy` — so a beacon
//! reaches the notification body as the literal text
//! `lakko/committer-beacon/v1` followed by two control bytes.
//!
//! This is **more** likely to be seen than the push-routing case rather than
//! less: a routing secret is broadcast on a membership change, while a beacon
//! is emitted by every owner/admin every
//! `committerBeaconIntervalMs`, so in an active group the newest journal row —
//! the one the NSE previews — is frequently a beacon.
//!
//! The NSE needs the CODEC, not a hardcoded domain prefix in Swift, which would
//! silently drift on the next version bump (the failure class ADR-0044
//! documents).
//!
//! ## Scope: DECODE + the envelope length, nothing else
//!
//! [`decode_committer_beacon`] — the authoritative classification — plus
//! [`committer_beacon_envelope_len`], the one wire constant the NSE's cheap
//! pre-filter needs so it does not hand-copy `27` into Swift (the ADR-0044
//! drift class: a stale literal fails in the DANGEROUS direction, silently
//! ceasing to suppress beacons).
//!
//! `compose_committer_beacon` / `acting_committer_verdict` /
//! `should_emit_beacon` and the TIMING constants are DELIBERATELY absent: the
//! NSE only asks "is this a service envelope I must not render?". It never
//! emits a beacon — it structurally cannot sweep (the read-only-throwaway
//! discipline, C-59), so a beacon from it would be a lie — and it never folds a
//! verdict, because it holds no observation history and takes no committer
//! decision. Keeping the surface to the classification question is the same
//! discipline the rest of this crate follows; a length is a pure value on the
//! read side of that question, not a step toward emitting one.

use lakko_contacts::committer_beacon::{
    decode_committer_beacon as rust_decode_committer_beacon, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
    COMMITTER_BEACON_PAYLOAD_LEN,
};

use crate::FfiError;

/// Strictly decode a committer-liveness beacon (ADR-0045 decision 2). Returns
/// `Ok(())` — the beacon is field-less, so a successful decode IS the whole
/// result: "this is a service envelope, render nothing".
///
/// The NSE calls this as one arm of its service-envelope filter, behind a cheap
/// length peek. An `Err` means "not this envelope" and the caller falls through
/// to the next arm and then to the normal chat parse. Fail-closed: a plaintext
/// without the beacon domain separator, with the wrong payload length, or with
/// an unknown schema version is refused rather than guessed at.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid committer beacon
///   (wrong domain / bad length / unknown version). On this filter path that is
///   the EXPECTED, common outcome — every chat message lands here — not a
///   fault.
#[uniffi::export]
pub fn decode_committer_beacon(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_committer_beacon(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Exact byte length of a composed committer beacon (ADR-0045) — the domain
/// separator plus the one-byte payload.
///
/// Shape-identical to `lakko-ffi-uniffi`'s `committer_beacon_envelope_len` (the
/// full surface the APP links), deliberately: the two frameworks are loaded
/// side by side on one device, and one wire fact must not be named two ways.
/// The value is DERIVED from the core's own constants rather than restated, so
/// a wire revision moves it here automatically.
///
/// Exposed for the cheap length pre-filter the NSE runs before the
/// authoritative [`decode_committer_beacon`], so that length is not hardcoded
/// on a fourth platform. Because the beacon is field-less, every beacon is
/// byte-identical, so an exact-length pre-filter is a total test of the size.
///
/// A pre-filter that is too NARROW fails in the dangerous direction — it stops
/// suppressing beacons and restores the garbage-notification defect — which is
/// why this is exported rather than copied: `golden_pins_the_swift_prefilter_shape`
/// below makes a wire drift fail Rust CI instead of silently un-suppressing the
/// Swift filter.
#[uniffi::export]
#[must_use]
pub fn committer_beacon_envelope_len() -> u32 {
    u32::try_from(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len() + COMMITTER_BEACON_PAYLOAD_LEN)
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::committer_beacon::{
        compose_committer_beacon, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
    };

    /// A well-formed beacon round-trips through the slim decoder — the surface
    /// the NSE filter calls.
    #[test]
    fn decodes_a_well_formed_beacon() {
        decode_committer_beacon(compose_committer_beacon()).expect("a beacon decodes");
    }

    /// The decode is what makes the NSE filter fail-closed the RIGHT way: a
    /// real chat envelope must NOT decode, or every chat message would be
    /// suppressed as a service envelope.
    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_committer_beacon(chat),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// Bytes carrying the right domain but a malformed payload are refused —
    /// the length and version gates, both reachable from a corrupt or hostile
    /// journal row.
    #[test]
    fn refuses_a_malformed_payload() {
        // Domain, no payload at all.
        assert!(matches!(
            decode_committer_beacon(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec()),
            Err(FfiError::Contacts(_)),
        ));
        // Domain, oversized payload.
        let mut long = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        long.extend_from_slice(&[0x01, 0x00]);
        assert!(matches!(
            decode_committer_beacon(long),
            Err(FfiError::Contacts(_)),
        ));
        // Domain, unknown schema version.
        let mut bad_version = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        bad_version.push(0xFF);
        assert!(matches!(
            decode_committer_beacon(bad_version),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// Without this arm the NSE renders a beacon as chat text. Pinned as the
    /// reason the module exists, so deleting it fails loudly rather than
    /// quietly reinstating the bug.
    #[test]
    fn an_unfiltered_beacon_is_never_chat_text() {
        let wire = compose_committer_beacon();
        let body = lakko_attachments::parse_body(&wire);
        assert_eq!(
            body,
            lakko_attachments::MessageBody::ServiceEnvelope,
            "a service envelope is never chat text, even with this arm absent",
        );
        // ...and the filter arm catches it first.
        decode_committer_beacon(wire).expect("the filter arm must recognise it");
    }

    /// The Swift-side cheap pre-filter keys on
    /// `count == committerBeaconEnvelopeLen() && first == 0x6c`. Pin both here,
    /// exactly as `push_routing::golden_pins_the_swift_prefilter_shape` does for
    /// the sibling envelope, so a wire change fails a Rust test rather than
    /// silently narrowing the NSE peek — which would stop suppressing beacons
    /// and restore the garbage-notification defect this filter closes.
    ///
    /// The 27 and the `0x6c` are written as LITERALS on purpose. Asserting the
    /// exported length against the same constants it is computed from would be a
    /// tautology that no wire revision can fail; a literal is what makes the
    /// golden discriminate.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        let wire = compose_committer_beacon();
        assert_eq!(wire.len(), 27, "NSE pre-filter pins the envelope length");
        assert_eq!(wire[0], 0x6c, "NSE pre-filter pins the first domain byte");
        assert_eq!(
            committer_beacon_envelope_len(),
            27,
            "the exported length is what Swift reads — it must equal the wire",
        );
    }

    /// The exported length must be the length actually COMPOSED, not merely a
    /// number that agrees with itself. This is what lets the Swift pre-filter
    /// treat `committerBeaconEnvelopeLen()` as the wire truth: if `compose` ever
    /// grew a field without the constants moving, this fails here rather than in
    /// a silently-narrowed notification filter.
    #[test]
    fn advertised_length_matches_the_composed_wire() {
        let wire = compose_committer_beacon();
        assert_eq!(
            u32::try_from(wire.len()).expect("a beacon is far below u32::MAX"),
            committer_beacon_envelope_len(),
        );
        // Field-less ⇒ byte-identical, so an exact-length peek is a total test
        // of the size and can never be too narrow for a legitimate beacon.
        assert_eq!(wire, compose_committer_beacon());
        // ...and that exact length is what the authoritative codec accepts.
        decode_committer_beacon(wire).expect("the composed length must decode");
    }

    /// The pre-filter's length gate must DISCRIMINATE: one byte either side of
    /// the advertised length is not a beacon, and the codec agrees. Pins that
    /// the exact-equality peek Swift now runs never widens (an over-long body
    /// slipping through as a service envelope would suppress a real message).
    #[test]
    fn a_wrong_length_is_not_a_beacon() {
        let len = committer_beacon_envelope_len() as usize;
        let mut short = compose_committer_beacon();
        short.pop();
        assert_ne!(short.len(), len);
        assert!(matches!(
            decode_committer_beacon(short),
            Err(FfiError::Contacts(_)),
        ));

        let mut long = compose_committer_beacon();
        long.push(0x00);
        assert_ne!(long.len(), len);
        assert!(matches!(
            decode_committer_beacon(long),
            Err(FfiError::Contacts(_)),
        ));
    }

    /// Cross-kind isolation at the slim boundary: the two service-envelope arms
    /// must not accept each other's bytes, or the NSE's filter chain would
    /// classify one as the other.
    #[test]
    fn does_not_cross_accept_a_push_routing_envelope() {
        let secret = [0x42u8; 32];
        let routing = lakko_contacts::push_routing::compose_push_routing_secret(7, &secret);
        assert!(matches!(
            decode_committer_beacon(routing),
            Err(FfiError::Contacts(_)),
        ));
        assert!(matches!(
            crate::push_routing::decode_push_routing_secret(compose_committer_beacon()),
            Err(FfiError::Contacts(_)),
        ));
    }
}
