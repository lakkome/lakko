// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` group-closed surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0065 phase-2 group-closed envelope).
//!
//! ## Why the NSE needs this at all
//!
//! The same reason [`crate::group_name`] exists. A closed notice is sent as
//! an ordinary MLS application message on the old group's journal — once by
//! the admin who recreated the group, and re-sent on growth
//! (`docs/ux/identity-recovery.md` §16.5) — and
//! [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback would degrade it to a `Chat` body. Unfiltered, the notification
//! reads `lakko/group-closed/v1` followed by binary stamp and id bytes. A
//! closed notice is exactly the kind of row that is newest when a wake
//! arrives, since it is the last thing posted into the old group.
//!
//! ## Scope: DECODE + the length bounds, nothing else
//!
//! [`decode_group_closed_envelope`] answers "is this a group-closed envelope
//! I must not render as a message?" and returns the decoded record. Like the
//! group-name arm it does return the fields: a closed notice is not personal
//! data about a person, and the read-only-throwaway discipline (C-59)
//! forbids the NSE to STORE it, not to read it — the app's own dispatcher
//! and role gate remain the only path that marks a group closed. **No
//! compose, no order comparison, no role gate lives here**: the NSE never
//! closes a group and takes no acceptance decision; it only refuses to
//! preview a service envelope as chat text.
//!
//! [`group_closed_envelope_min_len`] / [`group_closed_envelope_max_len`] are
//! what the NSE's cheap pre-filter needs. The envelope has exactly two
//! lengths (with and without a successor), so the peek is a RANGE plus the
//! first byte, and both ends are exported rather than hand-copied into Swift
//! (the ADR-0044 drift class: a stale literal fails in the DANGEROUS
//! direction, silently ceasing to suppress).
//!
//! ## A malformed notice is NOT classified here
//!
//! The decode is strict: an envelope carrying the group-closed domain but
//! failing a bound returns `Err`, exactly as a non-group-closed does, and
//! the NSE's filter chain falls through to the chat parser. The app's own
//! dispatcher drops such an envelope instead. This mirrors every sibling arm
//! and is recorded rather than fixed here — see [`crate::profile`]'s module
//! docs for the three-way-answer reasoning.

use lakko_contacts::group_closed::{
    decode_group_closed as rust_decode_group_closed, GROUP_CLOSED_ENVELOPE_MAX_LEN,
    GROUP_CLOSED_ENVELOPE_MIN_LEN,
};

use crate::FfiError;

/// A group's closed notice (ADR-0065 phase 2), as decoded by the slim
/// surface. Field-identical to `lakko-ffi-uniffi`'s `FfiGroupClosed`, so the
/// two frameworks present one shape to Swift.
///
/// **Read-only in this process (C-59).** The NSE may word a preview with it;
/// it must never persist it — acceptance (the role gate and the newer-than
/// order) is the app's decision, on the app's own receive path.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiGroupClosed {
    /// Closer-clock UNIX milliseconds at which the group was closed.
    pub closed_at_ms: u64,
    /// The recreated group's 32-byte id, or `None` when the group was closed
    /// without a successor.
    pub successor_group_id: Option<Vec<u8>>,
}

/// Strictly decode a group-closed envelope (ADR-0065 phase 2). Fail-closed:
/// a plaintext without the group-closed domain separator, truncated, with
/// trailing bytes, an unknown version or a successor of the wrong length is
/// refused as a whole.
///
/// The NSE calls this as one arm of its service-envelope filter, behind a
/// cheap length peek. An `Err` means "not this envelope" and the caller falls
/// through to the next arm and then to the normal chat parse.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid group-closed
///   envelope. On the NSE filter path that is the EXPECTED, common outcome —
///   every chat message lands here — not a fault.
#[uniffi::export]
pub fn decode_group_closed_envelope(plaintext: Vec<u8>) -> Result<FfiGroupClosed, FfiError> {
    rust_decode_group_closed(&plaintext)
        .map(|c| FfiGroupClosed {
            closed_at_ms: c.closed_at_ms,
            successor_group_id: c.successor_group_id.map(|id| id.to_vec()),
        })
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Shortest well-formed group-closed envelope in bytes (closed without a
/// successor).
///
/// Derived from the core's constants, so a wire revision moves it here
/// automatically. Shape-parallel to `group_name_envelope_min_len`.
#[uniffi::export]
#[must_use]
pub fn group_closed_envelope_min_len() -> u32 {
    u32::try_from(GROUP_CLOSED_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed group-closed envelope in bytes (closed with a
/// successor).
///
/// A pre-filter bound that is too LOW fails in the dangerous direction — it
/// stops suppressing the with-successor envelope — which is why it is
/// exported and pinned by a golden rather than copied.
#[uniffi::export]
#[must_use]
pub fn group_closed_envelope_max_len() -> u32 {
    u32::try_from(GROUP_CLOSED_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::group_closed::{
        compose_group_closed, GroupClosed, GROUP_CLOSED_ENVELOPE_DOMAIN_V1, GROUP_CLOSED_VERSION_V1,
    };

    const fn closed(successor: Option<[u8; 32]>) -> GroupClosed {
        GroupClosed {
            closed_at_ms: 1_700_000_000_000,
            successor_group_id: successor,
        }
    }

    #[test]
    fn decodes_a_well_formed_notice() {
        for successor in [Some([0x5A; 32]), None] {
            let decoded = decode_group_closed_envelope(compose_group_closed(&closed(successor)))
                .expect("a closed notice decodes");
            assert_eq!(decoded.closed_at_ms, 1_700_000_000_000);
            assert_eq!(decoded.successor_group_id, successor.map(|id| id.to_vec()));
        }
    }

    /// The decode is what makes the NSE filter fail-closed the RIGHT way: a
    /// real chat envelope must NOT decode, or every chat message would be
    /// suppressed as a service envelope.
    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_group_closed_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
    }

    /// Pinned as the reason the module exists: without this arm the NSE
    /// renders a closed notice — the domain text, the stamp and the id bytes
    /// — as chat text.
    #[test]
    fn an_unfiltered_notice_is_never_chat_text() {
        let wire = compose_group_closed(&closed(Some([0x5A; 32])));
        assert_eq!(
            lakko_attachments::parse_body(&wire),
            lakko_attachments::MessageBody::ServiceEnvelope,
            "a service envelope is never chat text, even with this arm absent",
        );
        decode_group_closed_envelope(wire).expect("the filter arm must recognise it");
    }

    #[test]
    fn a_malformed_notice_is_refused_like_a_non_notice() {
        let mut bad = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(GROUP_CLOSED_VERSION_V1);
        bad.extend_from_slice(&0u64.to_be_bytes());
        bad.push(16);
        bad.extend_from_slice(&[0; 16]);
        assert!(matches!(
            decode_group_closed_envelope(bad),
            Err(FfiError::Contacts(_))
        ));
        // Domain, no payload at all.
        assert!(matches!(
            decode_group_closed_envelope(GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec()),
            Err(FfiError::Contacts(_))
        ));
    }

    /// The Swift pre-filter keys on
    /// `min <= count <= max && first == 0x6c`. The bounds, the first byte
    /// and the domain prefix are written as LITERALS so a wire change fails a
    /// Rust test rather than silently narrowing the NSE peek.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(group_closed_envelope_min_len(), 32);
        assert_eq!(group_closed_envelope_max_len(), 64);
        let smallest = compose_group_closed(&closed(None));
        assert_eq!(smallest.len(), 32);
        assert_eq!(smallest[0], 0x6c);
        assert_eq!(&smallest[..22], b"lakko/group-closed/v1\x00");
        let largest = compose_group_closed(&closed(Some([0xFF; 32])));
        assert_eq!(largest.len(), 64);
        decode_group_closed_envelope(largest).expect("the maximal envelope decodes");
    }

    /// Cross-kind isolation at the slim boundary: no sibling arm accepts a
    /// closed notice, and this arm accepts none of theirs.
    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let wire = compose_group_closed(&closed(Some([0x5A; 32])));
        assert!(crate::committer_beacon::decode_committer_beacon(wire.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(wire.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(wire.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(wire.clone()).is_err());
        assert!(crate::pin::decode_pin_envelope(wire).is_err());

        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_group_closed_envelope(beacon).is_err());
        let routing = lakko_contacts::push_routing::compose_push_routing_secret(7, &[0x42; 32]);
        assert!(decode_group_closed_envelope(routing).is_err());
        let name = lakko_contacts::group_name::compose_group_name(
            &lakko_contacts::group_name::GroupName {
                name: "Trio".to_string(),
                updated_at_ms: 1,
            },
        )
        .unwrap();
        assert!(decode_group_closed_envelope(name).is_err());
        let pin = lakko_contacts::pin::compose_pin(&lakko_contacts::pin::PinOp {
            message_id: 1,
            action: lakko_contacts::pin::PinAction::Pin,
            updated_at_ms: 1,
        });
        assert!(decode_group_closed_envelope(pin).is_err());
    }
}
