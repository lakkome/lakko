// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` group-name surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0051 group-name envelope).
//!
//! ## Why the NSE needs this at all
//!
//! The fourth arrival of the reason [`crate::push_routing`],
//! [`crate::committer_beacon`] and [`crate::profile`] exist. A group name is
//! sent as an ordinary MLS application message on the group journal — right
//! after the creating commit, on every rename, and re-sent by the owner and
//! admins whenever the group grows (ADR-0051 §3) — and
//! [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback degrades it to a `Chat` body. Unfiltered, the notification reads
//! `lakko/group-name/v1` followed by binary stamp bytes and the name. A rename
//! is exactly the kind of row that is newest when a wake arrives.
//!
//! ## Scope: DECODE + the length bounds, nothing else
//!
//! [`decode_group_name_envelope`] answers "is this a group-name envelope I
//! must not render as a message?" and returns the decoded record. Unlike the
//! profile arm it does return the fields: a group name is not personal data
//! about a person, and the read-only-throwaway discipline (C-59) forbids the
//! NSE to STORE it, not to read it — the app's own dispatcher and role gate
//! remain the only path that persists a name. **No compose, no order
//! comparison, no role gate lives here**: the NSE never sets a name and takes
//! no acceptance decision; it only refuses to preview a service envelope as
//! chat text.
//!
//! [`group_name_envelope_min_len`] / [`group_name_envelope_max_len`] are what
//! the NSE's cheap pre-filter needs. The envelope is variable-length, so the
//! peek is a RANGE plus the first byte, and both ends are exported rather than
//! hand-copied into Swift (the ADR-0044 drift class: a stale literal fails in
//! the DANGEROUS direction, silently ceasing to suppress).
//!
//! ## A malformed group name is NOT classified here
//!
//! The decode is strict: an envelope carrying the group-name domain but
//! failing a bound returns `Err`, exactly as a non-group-name does, and the
//! NSE's filter chain falls through to the chat parser. The app's own
//! dispatcher drops such an envelope instead. This mirrors every sibling arm
//! and is recorded rather than fixed here — see
//! [`crate::profile`]'s module docs for the three-way-answer reasoning.

use lakko_contacts::group_name::{
    decode_group_name as rust_decode_group_name, GROUP_NAME_ENVELOPE_MAX_LEN,
    GROUP_NAME_ENVELOPE_MIN_LEN,
};

use crate::FfiError;

/// A group's shared name (ADR-0051 §1), as decoded by the slim surface.
/// Field-identical to `lakko-ffi-uniffi`'s `FfiGroupName`, so the two
/// frameworks present one shape to Swift.
///
/// **Read-only in this process (C-59).** The NSE may title a preview with it;
/// it must never persist it — acceptance (the ADR-0051 §2 role gate and the
/// newer-than order) is the app's decision, on the app's own receive path.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiGroupName {
    /// The name. **Empty means "clear the name."**
    pub name: String,
    /// Setter-clock UNIX milliseconds at which the name was set.
    pub updated_at_ms: u64,
}

/// Strictly decode a group-name envelope (ADR-0051 §1). Fail-closed: a
/// plaintext without the group-name domain separator, malformed, or with the
/// name out of bounds is refused as a whole.
///
/// The NSE calls this as one arm of its service-envelope filter, behind a
/// cheap length peek. An `Err` means "not this envelope" and the caller falls
/// through to the next arm and then to the normal chat parse.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid group-name
///   envelope (wrong domain / truncated / trailing bytes / unknown version /
///   a name out of bounds). On the NSE filter path that is the EXPECTED,
///   common outcome — every chat message lands here — not a fault.
#[uniffi::export]
pub fn decode_group_name_envelope(plaintext: Vec<u8>) -> Result<FfiGroupName, FfiError> {
    rust_decode_group_name(&plaintext)
        .map(|g| FfiGroupName {
            name: g.name,
            updated_at_ms: g.updated_at_ms,
        })
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Shortest well-formed group-name envelope in bytes (an empty name — the
/// "clear" envelope).
///
/// Derived from the core's constants, so a wire revision moves it here
/// automatically. Shape-parallel to `profile_envelope_min_len`.
#[uniffi::export]
#[must_use]
pub fn group_name_envelope_min_len() -> u32 {
    u32::try_from(GROUP_NAME_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed group-name envelope in bytes (a maximal name).
///
/// A pre-filter bound that is too LOW fails in the dangerous direction — it
/// stops suppressing long names — which is why it is exported and pinned by
/// a golden rather than copied.
#[uniffi::export]
#[must_use]
pub fn group_name_envelope_max_len() -> u32 {
    u32::try_from(GROUP_NAME_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::group_name::{
        compose_group_name, GroupName, GROUP_NAME_ENVELOPE_DOMAIN_V1, GROUP_NAME_VERSION_V1,
    };

    fn group_name(name: &str) -> GroupName {
        GroupName {
            name: name.to_string(),
            updated_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn decodes_a_well_formed_group_name() {
        for name in ["Trio", ""] {
            let decoded =
                decode_group_name_envelope(compose_group_name(&group_name(name)).unwrap())
                    .expect("a group name decodes");
            assert_eq!(decoded.name, name);
            assert_eq!(decoded.updated_at_ms, 1_700_000_000_000);
        }
    }

    /// The decode is what makes the NSE filter fail-closed the RIGHT way: a
    /// real chat envelope must NOT decode, or every chat message would be
    /// suppressed as a service envelope.
    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_group_name_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
    }

    /// Pinned as the reason the module exists: without this arm the NSE
    /// renders a rename — the domain text, the stamp bytes and the name — as
    /// chat text.
    #[test]
    fn an_unfiltered_group_name_would_otherwise_render_as_chat_text() {
        let wire = compose_group_name(&group_name("Trio")).unwrap();
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => assert!(
                text.starts_with("lakko/group-name/v1"),
                "the lossy fallback is what this arm pre-empts",
            ),
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        decode_group_name_envelope(wire).expect("the filter arm must recognise it");
    }

    #[test]
    fn a_malformed_group_name_is_refused_like_a_non_group_name() {
        let mut bad = GROUP_NAME_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(GROUP_NAME_VERSION_V1);
        bad.extend_from_slice(&0u64.to_be_bytes());
        let name = "Trio\u{202E}".as_bytes();
        bad.push(u8::try_from(name.len()).unwrap());
        bad.extend_from_slice(name);
        assert!(matches!(
            decode_group_name_envelope(bad),
            Err(FfiError::Contacts(_))
        ));
        // Domain, no payload at all.
        assert!(matches!(
            decode_group_name_envelope(GROUP_NAME_ENVELOPE_DOMAIN_V1.to_vec()),
            Err(FfiError::Contacts(_))
        ));
    }

    /// The Swift pre-filter keys on
    /// `min <= count <= max && first == 0x6c`. The bounds, the first byte
    /// and the domain prefix are written as LITERALS so a wire change fails a
    /// Rust test rather than silently narrowing the NSE peek.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(group_name_envelope_min_len(), 30);
        assert_eq!(group_name_envelope_max_len(), 285);
        let smallest = compose_group_name(&group_name("")).unwrap();
        assert_eq!(smallest.len(), 30);
        assert_eq!(smallest[0], 0x6c);
        assert_eq!(&smallest[..20], b"lakko/group-name/v1\x00");
        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        let largest = compose_group_name(&group_name(&name)).unwrap();
        assert_eq!(largest.len(), 285);
        decode_group_name_envelope(largest).expect("the maximal envelope decodes");
    }

    /// Cross-kind isolation at the slim boundary: none of the four service
    /// arms accepts another's bytes.
    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let group_name_wire = compose_group_name(&group_name("Trio")).unwrap();
        assert!(crate::committer_beacon::decode_committer_beacon(group_name_wire.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(group_name_wire.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(group_name_wire).is_err());

        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_group_name_envelope(beacon).is_err());
        let routing = lakko_contacts::push_routing::compose_push_routing_secret(7, &[0x42; 32]);
        assert!(decode_group_name_envelope(routing).is_err());
        let profile = lakko_contacts::profile::compose_profile(&lakko_contacts::profile::Profile {
            display_name: Some("Alice".to_string()),
            username: None,
            updated_at_ms: 1,
            account_id: None,
        })
        .unwrap();
        assert!(decode_group_name_envelope(profile).is_err());
    }
}
