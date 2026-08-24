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
//! ## Scope: DECODE only
//!
//! [`decode_committer_beacon`] and nothing else.
//! `compose_committer_beacon` / `acting_committer_verdict` /
//! `should_emit_beacon` and the timing constants are DELIBERATELY absent: the
//! NSE only asks "is this a service envelope I must not render?". It never
//! emits a beacon — it structurally cannot sweep (the read-only-throwaway
//! discipline, C-59), so a beacon from it would be a lie — and it never folds a
//! verdict, because it holds no observation history and takes no committer
//! decision. Keeping the surface to the classification question is the same
//! discipline the rest of this crate follows.

use lakko_contacts::committer_beacon::decode_committer_beacon as rust_decode_committer_beacon;

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
    fn an_unfiltered_beacon_would_otherwise_render_as_chat_text() {
        let wire = compose_committer_beacon();
        let body = lakko_attachments::parse_body(&wire);
        match body {
            lakko_attachments::MessageBody::Chat { text, .. } => assert!(
                text.starts_with("lakko/committer-beacon/v1"),
                "the legacy lossy-UTF-8 fallback is what this filter arm exists to pre-empt",
            ),
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        // ...and the filter arm catches it first.
        decode_committer_beacon(wire).expect("the filter arm must recognise it");
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
