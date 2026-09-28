// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim ADR-0058 surface — the CLASSIFY half only, for the four "who can
//! find me" service envelopes (ADR-0041 Phase 2).
//!
//! ## Why the NSE needs this at all
//!
//! Four new envelopes ride MLS application messages: the introduction offer
//! and the introductions bundle (in 1:1 conversations, ADR-0058 §4), the
//! "who can find me" setting and the reach-key state (on the owner's
//! DeviceGroup, §7). Unrecognised, each falls through to
//! [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback and reaches the notification body as its domain string plus
//! binary bytes — the same defect the push-routing, beacon, profile and pin
//! arms exist to pre-empt.
//!
//! ## Scope: ONE decode + the length range, nothing else
//!
//! [`decode_scoped_discovery_envelope`] answers "is this one of the four
//! envelopes I must not render?" and **returns nothing else**: an offer or a
//! bundle carries sealed records whose grants are secret, and a setting or a
//! reach state is the owner's private configuration. The NSE is
//! read-only-throwaway (C-59); it must never learn, store or act on any of
//! it. Compose, the orders and the dispatcher's data are the app's.
//!
//! One function for four kinds, rather than four, because the NSE's answer
//! is the same for all of them (suppress) and each export is surface the
//! size-budgeted extension carries. The length range spans all four;
//! [`scoped_discovery_envelope_min_len`] / [`scoped_discovery_envelope_max_len`]
//! are derived from the core's constants and pinned by goldens that compose
//! each kind's extremes.
//!
//! ## A malformed envelope is NOT classified here
//!
//! As for the profile arm (see [`crate::profile`]): an envelope carrying one
//! of the four domains but failing a bound returns `Err`, exactly as a
//! non-envelope does, and the NSE falls through to the chat parser. The
//! app's dispatcher drops it instead. Recorded rather than changed, for the
//! reason the profile module gives.

use lakko_contacts::introduction::{
    try_unwrap_introduction_offer, try_unwrap_introductions, INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
    INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1, INTRODUCTION_RECORD_MAX_LEN,
    INTRODUCTION_RECORD_MIN_LEN, MAX_BUNDLE_RECORDS,
};
use lakko_contacts::reach::MAX_REACH_SCOPE_ID_LEN;
use lakko_contacts::visibility::{
    try_unwrap_reach_state, try_unwrap_visibility, MAX_CHOSEN_SCOPES, MAX_REACH_STATE_ENTRIES,
    MAX_SCOPE_ID_LEN, REACH_STATE_ENVELOPE_DOMAIN_V1, VISIBILITY_ENVELOPE_DOMAIN_V1,
};

use crate::FfiError;

// The four layouts' extremes, from the core's public constants. Each is
// pinned against a composed envelope in the tests below, so a layout change
// in the core that these sums missed fails a Rust test.

/// Offer: `domain || version(1) || record`.
const OFFER_MIN: usize =
    INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1.len() + 1 + INTRODUCTION_RECORD_MIN_LEN;
const OFFER_MAX: usize =
    INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1.len() + 1 + INTRODUCTION_RECORD_MAX_LEN;
/// Bundle: `domain || version(1) || updated_at(8) || count(2)` then per
/// record `len(2) || record`; the smallest bucket is one record.
const BUNDLE_HEADER: usize = INTRODUCTIONS_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 2;
const BUNDLE_MIN: usize = BUNDLE_HEADER + 2 + INTRODUCTION_RECORD_MIN_LEN;
const BUNDLE_MAX: usize = BUNDLE_HEADER + MAX_BUNDLE_RECORDS * (2 + INTRODUCTION_RECORD_MAX_LEN);
/// Setting: `domain || version(1) || updated_at(8) || mode flags groups
/// last_groups (4) || count(2)` then per scope `kind(1) || len(1) || id`.
const VISIBILITY_MIN: usize = VISIBILITY_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 4 + 2;
const VISIBILITY_MAX: usize = VISIBILITY_MIN + MAX_CHOSEN_SCOPES * (2 + MAX_SCOPE_ID_LEN);
/// Reach state: `domain || version(1) || updated_at(8) || count(2)` then per
/// entry `class(1) || len(1) || scope || version(4)`.
const REACH_STATE_MIN: usize = REACH_STATE_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 2;
const REACH_STATE_MAX: usize =
    REACH_STATE_MIN + MAX_REACH_STATE_ENTRIES * (1 + 1 + MAX_REACH_SCOPE_ID_LEN + 4);

const fn min2(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}

const fn max2(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

const RANGE_MIN: usize = min2(
    min2(OFFER_MIN, BUNDLE_MIN),
    min2(VISIBILITY_MIN, REACH_STATE_MIN),
);
const RANGE_MAX: usize = max2(
    max2(OFFER_MAX, BUNDLE_MAX),
    max2(VISIBILITY_MAX, REACH_STATE_MAX),
);

/// Recognise one of the four ADR-0058 service envelopes (introduction offer,
/// introductions bundle, "who can find me" setting, reach state). `Ok(())`
/// for a well-formed one — "this is a service envelope, render nothing". No
/// field is returned (see the module docs).
///
/// # Errors
///
/// [`FfiError::Contacts`] for anything else, including one of the four
/// domains with a malformed payload. On the NSE filter path that is the
/// EXPECTED, common outcome — every chat message lands here — not a fault.
#[uniffi::export]
pub fn decode_scoped_discovery_envelope(plaintext: Vec<u8>) -> Result<(), FfiError> {
    let err = |e: &dyn core::fmt::Display| FfiError::Contacts(e.to_string());
    if try_unwrap_introduction_offer(&plaintext)
        .map_err(|e| err(&e))?
        .is_some()
        || try_unwrap_introductions(&plaintext)
            .map_err(|e| err(&e))?
            .is_some()
        || try_unwrap_visibility(&plaintext)
            .map_err(|e| err(&e))?
            .is_some()
        || try_unwrap_reach_state(&plaintext)
            .map_err(|e| err(&e))?
            .is_some()
    {
        return Ok(());
    }
    Err(FfiError::Contacts(
        "not an ADR-0058 service envelope".to_string(),
    ))
}

/// Shortest well-formed envelope of the four, in bytes (an empty reach
/// state). The NSE pre-filter's lower bound.
#[uniffi::export]
#[must_use]
pub fn scoped_discovery_envelope_min_len() -> u32 {
    u32::try_from(RANGE_MIN).unwrap_or(u32::MAX)
}

/// Longest well-formed envelope of the four, in bytes (a full introductions
/// bundle of maximal records). A bound that is too LOW fails in the
/// dangerous direction — it stops suppressing — hence exported, not copied.
#[uniffi::export]
#[must_use]
pub fn scoped_discovery_envelope_max_len() -> u32 {
    u32::try_from(RANGE_MAX).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::introduction::{
        compose_bundle, compose_introduction, compose_introduction_offer, IntroductionPayload,
        IntroductionRecord,
    };
    use lakko_contacts::reach::{ReachClass, ReachGrant};
    use lakko_contacts::visibility::{
        compose_reach_state, compose_visibility, GroupsForm, ReachState, ReachStateEntry, ScopeId,
        ScopeKind, VisibilitySetting,
    };

    fn payload(maximal: bool) -> IntroductionPayload {
        IntroductionPayload {
            identity_pk: [1; 32],
            account_id: [2; 16],
            display_name: maximal.then(|| format!("{}\u{20AC}", "\u{1F600}".repeat(63))),
            username: maximal.then(|| "a".repeat(32)),
            username_certificate: maximal.then(|| vec![9; 153]),
            fof: ReachGrant::derive(&[3; 32], ReachClass::Fof, b"", 1).unwrap(),
        }
    }

    fn record(maximal: bool) -> Vec<u8> {
        let terms: Vec<[u8; 64]> = if maximal {
            (0..65u8).map(|i| [i; 64]).collect()
        } else {
            vec![[5; 64]]
        };
        compose_introduction(&payload(maximal), &terms, 1).unwrap()
    }

    fn visibility(maximal: bool) -> Vec<u8> {
        let mut s = VisibilitySetting::default_for_new_account();
        if maximal {
            s.groups = Some(GroupsForm::Chosen);
            s.chosen = (0..1024u16)
                .map(|i| {
                    let mut id = vec![0u8; 64];
                    id[..2].copy_from_slice(&i.to_be_bytes());
                    ScopeId {
                        kind: ScopeKind::Group,
                        id,
                    }
                })
                .collect();
        }
        compose_visibility(&s).unwrap()
    }

    fn reach_state(maximal: bool) -> Vec<u8> {
        let entries = if maximal {
            (0..4096u16)
                .map(|i| {
                    let mut scope_id = vec![0u8; 64];
                    scope_id[..2].copy_from_slice(&i.to_be_bytes());
                    ReachStateEntry {
                        class: ReachClass::Group,
                        scope_id,
                        version: 1,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        compose_reach_state(&ReachState {
            entries,
            updated_at_ms: 1,
        })
        .unwrap()
    }

    #[test]
    fn recognises_all_four_kinds() {
        let rec = record(false);
        let bundle = compose_bundle(&[IntroductionRecord::decode(&rec).unwrap()], 2).unwrap();
        for wire in [
            compose_introduction_offer(&rec).unwrap(),
            bundle,
            visibility(false),
            reach_state(false),
        ] {
            decode_scoped_discovery_envelope(wire).expect("recognised");
        }
    }

    #[test]
    fn refuses_a_chat_envelope_and_a_malformed_one() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_scoped_discovery_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
        let mut broken = visibility(false);
        broken.push(0);
        assert!(decode_scoped_discovery_envelope(broken).is_err());
    }

    /// Pinned as the reason the module exists: unfiltered, each of the four
    /// would render as lossy chat text starting with its domain.
    #[test]
    fn an_unfiltered_envelope_would_otherwise_render_as_chat_text() {
        let wire = visibility(false);
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => {
                assert!(text.starts_with("lakko/visibility/v1"));
            },
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        decode_scoped_discovery_envelope(wire).expect("the filter arm must recognise it");
    }

    /// The Swift pre-filter keys on `first == 0x6c` and the length range;
    /// written as LITERALS so a wire change fails a Rust test rather than
    /// silently narrowing the NSE peek. Each kind's composed extremes must
    /// equal the derived constants.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(
            INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            b"lakko/introduction-offer/v1\x00"
        );
        assert_eq!(
            INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            b"lakko/introductions/v1\x00"
        );
        assert_eq!(VISIBILITY_ENVELOPE_DOMAIN_V1, b"lakko/visibility/v1\x00");
        assert_eq!(REACH_STATE_ENVELOPE_DOMAIN_V1, b"lakko/reach-state/v1\x00");

        let min_rec = record(false);
        let max_rec = record(true);
        assert_eq!(min_rec.len(), INTRODUCTION_RECORD_MIN_LEN);
        assert_eq!(max_rec.len(), INTRODUCTION_RECORD_MAX_LEN);

        let offer_min = compose_introduction_offer(&min_rec).unwrap();
        let offer_max = compose_introduction_offer(&max_rec).unwrap();
        assert_eq!((offer_min.len(), offer_max.len()), (OFFER_MIN, OFFER_MAX));

        let bundle_min =
            compose_bundle(&[IntroductionRecord::decode(&min_rec).unwrap()], 1).unwrap();
        assert_eq!(bundle_min.len(), BUNDLE_MIN);
        let max_decoded = IntroductionRecord::decode(&max_rec).unwrap();
        let bundle_max = compose_bundle(&vec![max_decoded; MAX_BUNDLE_RECORDS], 1).unwrap();
        assert_eq!(bundle_max.len(), BUNDLE_MAX);

        let (v_min, v_max) = (visibility(false), visibility(true));
        assert_eq!((v_min.len(), v_max.len()), (VISIBILITY_MIN, VISIBILITY_MAX));
        let (r_min, r_max) = (reach_state(false), reach_state(true));
        assert_eq!(
            (r_min.len(), r_max.len()),
            (REACH_STATE_MIN, REACH_STATE_MAX)
        );

        // The literals.
        assert_eq!(scoped_discovery_envelope_min_len(), 32);
        assert_eq!(REACH_STATE_MIN, 32);
        assert_eq!(VISIBILITY_MIN, 35);
        assert_eq!(scoped_discovery_envelope_max_len() as usize, BUNDLE_MAX);
        for wire in [
            &offer_min,
            &offer_max,
            &bundle_min,
            &bundle_max,
            &v_min,
            &v_max,
            &r_min,
            &r_max,
        ] {
            assert_eq!(wire[0], 0x6c);
            assert!((32..=scoped_discovery_envelope_max_len() as usize).contains(&wire.len()));
            decode_scoped_discovery_envelope(wire.clone()).expect("every extreme decodes");
        }
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let wire = visibility(false);
        assert!(crate::profile::decode_profile_envelope(wire.clone()).is_err());
        assert!(crate::pin::decode_pin_envelope(wire.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(wire.clone()).is_err());
        assert!(crate::committer_beacon::decode_committer_beacon(wire.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(wire.clone()).is_err());
        assert!(crate::profile_request::decode_profile_request(wire).is_err());

        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_scoped_discovery_envelope(beacon).is_err());
        let request = lakko_contacts::profile_request::compose_profile_request();
        assert!(decode_scoped_discovery_envelope(request).is_err());
    }
}
