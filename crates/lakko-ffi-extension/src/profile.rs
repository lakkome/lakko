// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` profile surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0048 profile envelope).
//!
//! ## Why the NSE needs this at all
//!
//! The third arrival of the reason [`crate::push_routing`] and
//! [`crate::committer_beacon`] exist. A profile is published as an ordinary
//! MLS application message on the group journal — on joining a group, on a
//! profile change, and whenever the group gains a member (ADR-0048 §2) — and
//! [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback degrades it to a `Chat` body. Unfiltered, the notification reads
//! `lakko/profile/v1` followed by binary length bytes and the sender's own
//! name. A group's growth makes every member republish, so the newest row —
//! the one the NSE previews — is often a profile right after an add.
//!
//! ## Scope: DECODE + the length bounds, nothing else
//!
//! [`decode_profile_envelope`] answers "is this a profile envelope I must not
//! render?" and **deliberately returns nothing else** — not the name, not the
//! username. The NSE is read-only-throwaway (C-59): it must never store a
//! profile, and ADR-0048's open question is answered "it only reads what the
//! app already stored". Handing it the decoded fields would be the first step
//! toward it acting on them. Compose, the newer-than order and display-name
//! preparation are absent for the same reason.
//!
//! [`profile_envelope_min_len`] / [`profile_envelope_max_len`] are what the
//! NSE's cheap pre-filter needs. Unlike the two fixed-length siblings the
//! profile envelope is variable-length, so the peek is a RANGE plus the first
//! byte, and both ends are exported rather than hand-copied into Swift.
//!
//! ## A malformed profile is NOT classified here
//!
//! The decode is strict: an envelope carrying the profile domain but failing
//! a bound (a right-to-left override in the name, a look-alike username)
//! returns `Err`, exactly as a non-profile does. The NSE's filter chain then
//! falls through to the chat parser, which renders it as lossy text. The
//! app's own dispatcher drops such an envelope instead. This mirrors how the
//! beacon and routing-secret arms behave today and is recorded rather than
//! fixed here, because changing it means the NSE needs a three-way answer
//! (`not a profile` / `a profile` / `a broken profile`) that no sibling arm
//! gives either — see [`tests::a_malformed_profile_is_refused_like_a_non_profile`].

use lakko_contacts::profile::{
    decode_profile as rust_decode_profile, PROFILE_ENVELOPE_MAX_LEN, PROFILE_ENVELOPE_MIN_LEN,
};

use crate::FfiError;

/// Strictly decode a profile envelope (ADR-0048). Returns `Ok(())` — the
/// classification is the entire result: "this is a service envelope, render
/// nothing". The profile's fields are deliberately not returned (see the
/// module docs).
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid profile envelope
///   (wrong domain, malformed, or a field out of bounds). On the NSE filter
///   path that is the EXPECTED, common outcome — every chat message lands
///   here — not a fault.
#[uniffi::export]
pub fn decode_profile_envelope(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_profile(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Shortest well-formed profile envelope in bytes (no name, no username).
///
/// Derived from the core's constants, so a wire revision moves it here
/// automatically. Shape-parallel to `committer_beacon_envelope_len`.
#[uniffi::export]
#[must_use]
pub fn profile_envelope_min_len() -> u32 {
    u32::try_from(PROFILE_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed profile envelope in bytes (maximal name and username).
///
/// A pre-filter bound that is too LOW fails in the dangerous direction — it
/// stops suppressing long profiles — which is why it is exported and pinned
/// by a golden rather than copied.
#[uniffi::export]
#[must_use]
pub fn profile_envelope_max_len() -> u32 {
    u32::try_from(PROFILE_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::profile::{compose_profile, Profile, PROFILE_ENVELOPE_DOMAIN_V1};

    fn profile(name: Option<&str>, username: Option<&str>) -> Profile {
        Profile {
            display_name: name.map(str::to_string),
            username: username.map(str::to_string),
            updated_at_ms: 1_700_000_000_000,
            account_id: None,
        }
    }

    #[test]
    fn decodes_a_well_formed_profile() {
        for p in [
            profile(Some("Alice Petrova"), Some("alice")),
            profile(None, None),
        ] {
            decode_profile_envelope(compose_profile(&p).unwrap()).expect("a profile decodes");
        }
    }

    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_profile_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
    }

    /// Pinned as the reason the module exists: without this arm the NSE
    /// renders a profile — the sender's name included — as chat text.
    #[test]
    fn an_unfiltered_profile_is_never_chat_text() {
        let wire = compose_profile(&profile(Some("Alice"), Some("alice"))).unwrap();
        assert_eq!(
            lakko_attachments::parse_body(&wire),
            lakko_attachments::MessageBody::ServiceEnvelope,
            "a service envelope is never chat text, even with this arm absent",
        );
        decode_profile_envelope(wire).expect("the filter arm must recognise it");
    }

    #[test]
    fn a_malformed_profile_is_refused_like_a_non_profile() {
        let mut bad = PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(0x01);
        bad.extend_from_slice(&0u64.to_be_bytes());
        let name = "Alice\u{202E}".as_bytes();
        bad.push(u8::try_from(name.len()).unwrap());
        bad.extend_from_slice(name);
        bad.push(0);
        assert!(matches!(
            decode_profile_envelope(bad),
            Err(FfiError::Contacts(_))
        ));
    }

    /// The Swift pre-filter keys on
    /// `min <= count <= max && first == 0x6c`. The bounds and the first byte
    /// are written as LITERALS so a wire change fails a Rust test rather than
    /// silently narrowing the NSE peek.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(profile_envelope_min_len(), 29);
        assert_eq!(profile_envelope_max_len(), 332);
        let smallest = compose_profile(&profile(None, None)).unwrap();
        assert_eq!(smallest.len(), 29);
        assert_eq!(smallest[0], 0x6c);
        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        let mut maximal = profile(Some(&name), Some(&"a".repeat(32)));
        maximal.account_id = Some([7; 16]);
        let largest = compose_profile(&maximal).unwrap();
        assert_eq!(largest.len(), 332);
        decode_profile_envelope(largest).expect("the maximal envelope decodes");
    }

    /// Cross-kind isolation at the slim boundary: none of the three service
    /// arms accepts another's bytes.
    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let profile_wire = compose_profile(&profile(Some("Alice"), None)).unwrap();
        assert!(crate::committer_beacon::decode_committer_beacon(profile_wire.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(profile_wire).is_err());
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_profile_envelope(beacon).is_err());
        let routing = lakko_contacts::push_routing::compose_push_routing_secret(7, &[0x42; 32]);
        assert!(decode_profile_envelope(routing).is_err());
    }
}
